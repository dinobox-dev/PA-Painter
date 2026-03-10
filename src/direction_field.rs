//! **Pipeline stage 1** — direction field generation from user-placed guides.
//!
//! Computes a per-texel flow direction by blending contributions from directional,
//! source, sink, and vortex guides with distance-based influence falloff.

use glam::Vec2;

use crate::types::{Guide, GuideType};

/// Canonicalize a direction vector to the upper half-plane.
/// For headless (180°-symmetric) directions, d and -d are equivalent.
/// We pick the representative with y > 0, or positive x when y == 0.
fn canonicalize(dir: Vec2) -> Vec2 {
    if dir.y < 0.0 || (dir.y == 0.0 && dir.x < 0.0) {
        -dir
    } else {
        dir
    }
}

/// Smoothstep-based weight: 1.0 at d=0, 0.0 at d=influence, smooth falloff.
fn guide_weight(d: f32, influence: f32) -> f32 {
    let t = (d / influence).clamp(0.0, 1.0);
    let s = 1.0 - t;
    s * s * (3.0 - 2.0 * s)
}

/// Returns true if this guide type uses headless (180°-symmetric) directions.
/// Directional guides have no inherent "forward" — (1,0) and (-1,0) are equivalent.
/// Source/Sink/Vortex are headed — their direction depends on the query position.
fn is_headless(guide_type: GuideType) -> bool {
    matches!(guide_type, GuideType::Directional)
}

/// Compute the direction contribution of a single guide at a query UV point.
/// For Directional guides, returns the stored direction.
/// For Source/Sink/Vortex, computes direction based on relative position.
fn guide_contribution(guide: &Guide, query_uv: Vec2) -> Vec2 {
    match guide.guide_type {
        GuideType::Directional => guide.direction.normalize_or_zero(),
        GuideType::Source => {
            let radial = query_uv - guide.position;
            if radial.length_squared() < 1e-8 {
                Vec2::X
            } else {
                radial.normalize()
            }
        }
        GuideType::Sink => {
            let radial = guide.position - query_uv;
            if radial.length_squared() < 1e-8 {
                Vec2::X
            } else {
                radial.normalize()
            }
        }
        GuideType::Vortex => {
            let radial = query_uv - guide.position;
            if radial.length_squared() < 1e-8 {
                Vec2::X
            } else {
                // 90° rotation of radial; direction.x < 0 → clockwise
                let tangent = Vec2::new(-radial.y, radial.x).normalize();
                if guide.direction.x < 0.0 {
                    -tangent
                } else {
                    tangent
                }
            }
        }
    }
}

/// Compute direction at a UV coordinate given a set of guide vertices.
///
/// Returns a normalized Vec2 representing the stroke direction.
/// If no guides have influence at this point, returns the nearest guide's direction.
/// If guides is empty, returns Vec2::X (default horizontal).
///
/// Uses smoothstep-based weighting. For Directional (headless) guides,
/// applies canonicalization and sign alignment for 180° symmetry.
/// For Source/Sink/Vortex (headed) guides, uses raw direction contributions.
pub fn direction_at(uv: Vec2, guides: &[Guide]) -> Vec2 {
    if guides.is_empty() {
        return Vec2::X;
    }

    if guides.len() == 1 {
        let dir = guide_contribution(&guides[0], uv);
        return if dir.length_squared() > 0.0 {
            dir
        } else {
            Vec2::X
        };
    }

    // Collect (weight, direction, headless?) for guides within influence
    struct WeightedDir {
        weight: f32,
        dir: Vec2,
        headless: bool,
    }
    let mut weighted: Vec<WeightedDir> = Vec::new();

    for g in guides {
        let d = uv.distance(g.position);

        if d > g.influence {
            continue;
        }

        let w = guide_weight(d, g.influence) * g.strength;
        let dir = guide_contribution(g, uv);
        if dir.length_squared() < 1e-12 {
            continue;
        }

        let headless = is_headless(g.guide_type);
        let dir = if headless { canonicalize(dir) } else { dir };
        weighted.push(WeightedDir {
            weight: w,
            dir,
            headless,
        });
    }

    // If no guide has influence, use nearest-neighbor fallback
    if weighted.is_empty() {
        let nearest = guides
            .iter()
            .min_by(|a, b| {
                uv.distance(a.position)
                    .partial_cmp(&uv.distance(b.position))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("guides must not be empty for nearest-neighbor fallback");
        let dir = guide_contribution(nearest, uv);
        return if dir.length_squared() > 0.0 {
            dir
        } else {
            Vec2::X
        };
    }

    // Use the highest-weight direction as reference for sign alignment
    let ref_dir = weighted
        .iter()
        .max_by(|a, b| {
            a.weight
                .partial_cmp(&b.weight)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .expect("weighted list must not be empty")
        .dir;

    // Weighted mean with sign alignment for headless guides
    let mut sum = Vec2::ZERO;
    for wd in &weighted {
        let aligned = if wd.headless && wd.dir.dot(ref_dir) < 0.0 {
            -wd.dir
        } else {
            wd.dir
        };
        sum += wd.weight * aligned;
    }

    if sum.length_squared() < 1e-12 {
        return ref_dir;
    }

    sum.normalize()
}

/// Generate a full direction field texture.
///
/// Returns a Vec<Vec2> of length resolution * resolution, row-major.
/// Each element is a normalized direction vector.
pub fn generate_direction_field(guides: &[Guide], resolution: u32) -> Vec<Vec2> {
    let res = resolution as usize;
    let mut field = Vec::with_capacity(res * res);
    let inv_res = 1.0 / resolution as f32;

    for y in 0..resolution {
        for x in 0..resolution {
            let uv = Vec2::new((x as f32 + 0.5) * inv_res, (y as f32 + 0.5) * inv_res);
            field.push(direction_at(uv, guides));
        }
    }

    field
}

/// Precomputed direction field for fast lookup during streamline tracing.
///
/// Stores a grid of direction vectors at a fixed resolution. Lookups use
/// bilinear interpolation with hemisphere alignment to prevent sign-flip
/// artifacts near the x-axis for headless (180°-symmetric) directions.
pub struct DirectionField {
    data: Vec<Vec2>,
    resolution: u32,
}

impl DirectionField {
    /// Build a direction field from guide vertices.
    ///
    /// Resolution is scaled to 1/4 of output resolution, clamped to [64, 2048].
    pub fn new(guides: &[Guide], resolution: u32) -> Self {
        let res = (resolution / 4).clamp(64, 2048);
        let data = generate_direction_field(guides, res);
        Self {
            data,
            resolution: res,
        }
    }

    /// Sample the direction field at a UV coordinate using bilinear interpolation.
    ///
    /// Returns a normalized direction vector. Handles 180° symmetry by aligning
    /// all four texel samples to the same hemisphere before interpolation.
    /// At guide boundaries (high directional variance), falls back to nearest
    /// neighbor to avoid incorrect hemisphere collapse.
    pub fn sample(&self, uv: Vec2) -> Vec2 {
        let res = self.resolution as f32;
        let tx = uv.x * res - 0.5;
        let ty = uv.y * res - 0.5;

        let x0 = (tx.floor() as i32).clamp(0, self.resolution as i32 - 1) as usize;
        let y0 = (ty.floor() as i32).clamp(0, self.resolution as i32 - 1) as usize;
        let x1 = (x0 + 1).min(self.resolution as usize - 1);
        let y1 = (y0 + 1).min(self.resolution as usize - 1);

        let fx = (tx - x0 as f32).clamp(0.0, 1.0);
        let fy = (ty - y0 as f32).clamp(0.0, 1.0);

        let w = self.resolution as usize;
        let d00 = self.data[y0 * w + x0];
        let d10 = self.data[y0 * w + x1];
        let d01 = self.data[y1 * w + x0];
        let d11 = self.data[y1 * w + x1];

        // Boundary detection: check directional coherence across all texel pairs.
        // At guide boundaries, texels point in very different directions; the
        // minimum absolute dot product among all pairs drops below the threshold.
        let min_abs_dot = [
            d00.dot(d10).abs(),
            d00.dot(d01).abs(),
            d00.dot(d11).abs(),
            d10.dot(d01).abs(),
            d10.dot(d11).abs(),
            d01.dot(d11).abs(),
        ]
        .into_iter()
        .fold(f32::INFINITY, f32::min);

        if min_abs_dot < 0.5 {
            // High variance — guide boundary. Use nearest texel to preserve
            // each guide region's direction without cross-boundary blending.
            let nearest = if fx < 0.5 {
                if fy < 0.5 {
                    d00
                } else {
                    d01
                }
            } else if fy < 0.5 {
                d10
            } else {
                d11
            };
            return if nearest.length_squared() < 1e-12 {
                d00
            } else {
                nearest.normalize()
            };
        }

        // Weighted reference selection: pick the texel with the largest
        // bilinear weight as the hemisphere alignment reference, instead of
        // always using d00. This ensures the reference is the most relevant
        // texel for the query point.
        let w00 = (1.0 - fx) * (1.0 - fy);
        let w10 = fx * (1.0 - fy);
        let w01 = (1.0 - fx) * fy;
        let w11 = fx * fy;

        let ref_dir = if w00 >= w10 && w00 >= w01 && w00 >= w11 {
            d00
        } else if w10 >= w01 && w10 >= w11 {
            d10
        } else if w01 >= w11 {
            d01
        } else {
            d11
        };

        // Hemisphere alignment: align all texels to the reference hemisphere
        let mut a00 = d00;
        let mut a10 = d10;
        let mut a01 = d01;
        let mut a11 = d11;
        if a00.dot(ref_dir) < 0.0 {
            a00 = -a00;
        }
        if a10.dot(ref_dir) < 0.0 {
            a10 = -a10;
        }
        if a01.dot(ref_dir) < 0.0 {
            a01 = -a01;
        }
        if a11.dot(ref_dir) < 0.0 {
            a11 = -a11;
        }

        let top = a00.lerp(a10, fx);
        let bottom = a01.lerp(a11, fx);
        let result = top.lerp(bottom, fy);

        if result.length_squared() < 1e-12 {
            ref_dir
        } else {
            result.normalize()
        }
    }
}

// ── Direction field overlay texture ────────────────────────────────

/// Render the direction field as an RGBA arrow overlay texture.
///
/// Returns `resolution × resolution` pixels as `Vec<u8>` in RGBA order.
/// Arrows are drawn on a grid with `arrow_spacing` pixel intervals.
/// Transparent background; arrows are white with alpha for overlay blending.
pub fn render_direction_field_overlay(
    guides: &[Guide],
    resolution: u32,
    arrow_spacing: u32,
) -> Vec<u8> {
    let res = resolution as usize;
    let mut pixels = vec![0u8; res * res * 4]; // fully transparent

    let inv_res = 1.0 / resolution as f32;
    let half_spacing = arrow_spacing as f32 * 0.4;
    let head_len = half_spacing * 0.4;
    let line_w = 1.5f32;
    let head_w = 3.0f32;

    // For each grid cell, draw an arrow
    let offset = arrow_spacing / 2;
    let mut cy = offset;
    while cy < resolution {
        let mut cx = offset;
        while cx < resolution {
            let uv = Vec2::new((cx as f32 + 0.5) * inv_res, (cy as f32 + 0.5) * inv_res);
            let dir = direction_at(uv, guides);
            if dir.length_squared() < 1e-6 {
                cx += arrow_spacing;
                continue;
            }

            let center = Vec2::new(cx as f32, cy as f32);
            let tip = center + dir * half_spacing;
            let tail = center - dir * half_spacing;

            // Draw line from tail to tip
            draw_line_aa(&mut pixels, res, tail, tip, line_w, [255, 255, 255, 180]);

            // Draw arrowhead
            let perp = Vec2::new(-dir.y, dir.x);
            let head_base = tip - dir * head_len;
            let left = head_base + perp * head_w;
            let right = head_base - perp * head_w;
            draw_line_aa(&mut pixels, res, tip, left, line_w, [255, 255, 255, 220]);
            draw_line_aa(&mut pixels, res, tip, right, line_w, [255, 255, 255, 220]);

            cx += arrow_spacing;
        }
        cy += arrow_spacing;
    }

    pixels
}

/// Draw an anti-aliased line into an RGBA pixel buffer using distance-based alpha.
fn draw_line_aa(pixels: &mut [u8], res: usize, p0: Vec2, p1: Vec2, width: f32, color: [u8; 4]) {
    let dx = p1.x - p0.x;
    let dy = p1.y - p0.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 0.01 {
        return;
    }

    // Bounding box with padding
    let pad = width + 1.0;
    let min_x = (p0.x.min(p1.x) - pad).floor().max(0.0) as usize;
    let max_x = (p0.x.max(p1.x) + pad).ceil().min(res as f32 - 1.0) as usize;
    let min_y = (p0.y.min(p1.y) - pad).floor().max(0.0) as usize;
    let max_y = (p0.y.max(p1.y) + pad).ceil().min(res as f32 - 1.0) as usize;

    let half_w = width * 0.5;

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            // Project point onto line segment
            let t = ((px - p0.x) * dx + (py - p0.y) * dy) / (len * len);
            let t = t.clamp(0.0, 1.0);
            let closest_x = p0.x + t * dx;
            let closest_y = p0.y + t * dy;
            let dist = ((px - closest_x).powi(2) + (py - closest_y).powi(2)).sqrt();

            if dist < half_w + 1.0 {
                // Anti-aliased alpha based on distance from line center
                let alpha = ((half_w + 0.5 - dist).clamp(0.0, 1.0) * color[3] as f32) as u8;
                if alpha > 0 {
                    let idx = (y * res + x) * 4;
                    // Max-blend: overlay takes the brightest alpha
                    let existing_a = pixels[idx + 3];
                    if alpha > existing_a {
                        pixels[idx] = color[0];
                        pixels[idx + 1] = color[1];
                        pixels[idx + 2] = color[2];
                        pixels[idx + 3] = alpha;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_1_SQRT_2;

    const EPS: f32 = 1e-3;

    fn approx_eq(a: f32, b: f32) -> bool {
        (a - b).abs() < EPS
    }

    fn approx_vec2(a: Vec2, b: Vec2) -> bool {
        approx_eq(a.x, b.x) && approx_eq(a.y, b.y)
    }

    fn is_normalized(v: Vec2) -> bool {
        (v.length() - 1.0).abs() < EPS
    }

    #[test]
    fn empty_guides_returns_x() {
        let dir = direction_at(Vec2::new(0.5, 0.5), &[]);
        assert!(approx_vec2(dir, Vec2::X));
    }

    #[test]
    fn single_guide_horizontal() {
        let guides = vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::new(1.0, 0.0),
            influence: 0.4,
            ..Guide::default()
        }];
        let dir = direction_at(Vec2::new(0.3, 0.5), &guides);
        assert!(approx_vec2(dir, Vec2::X));
        assert!(is_normalized(dir));
    }

    #[test]
    fn single_guide_vertical() {
        let guides = vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::new(0.0, 1.0),
            influence: 0.4,
            ..Guide::default()
        }];
        let dir = direction_at(Vec2::new(0.5, 0.5), &guides);
        assert!(approx_vec2(dir, Vec2::Y));
        assert!(is_normalized(dir));
    }

    #[test]
    fn single_guide_diagonal() {
        let guides = vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::new(1.0, 1.0),
            influence: 0.5,
            ..Guide::default()
        }];
        let dir = direction_at(Vec2::new(0.5, 0.5), &guides);
        let expected = Vec2::new(FRAC_1_SQRT_2, FRAC_1_SQRT_2);
        assert!(approx_vec2(dir, expected));
        assert!(is_normalized(dir));
    }

    #[test]
    fn two_guides_same_direction() {
        let guides = vec![
            Guide {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
                ..Guide::default()
            },
        ];
        let dir = direction_at(Vec2::new(0.5, 0.5), &guides);
        assert!(approx_vec2(dir, Vec2::X));
    }

    #[test]
    fn two_guides_90_degrees_at_midpoint() {
        let guides = vec![
            Guide {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.5,
                ..Guide::default()
            },
        ];
        // At the midpoint, both guides are equidistant, so result should be ~45°
        let dir = direction_at(Vec2::new(0.5, 0.5), &guides);
        let expected = Vec2::new(FRAC_1_SQRT_2, FRAC_1_SQRT_2);
        assert!(approx_vec2(dir, expected));
        assert!(is_normalized(dir));
    }

    #[test]
    fn symmetry_180_degrees_no_cancellation() {
        // (1,0) and (-1,0) are the same headless direction
        let guides = vec![
            Guide {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::new(-1.0, 0.0),
                influence: 0.5,
                ..Guide::default()
            },
        ];
        let dir = direction_at(Vec2::new(0.5, 0.5), &guides);
        // Should NOT be zero — both canonicalize to (1,0)
        assert!(dir.length() > 0.9);
        assert!(dir.x.abs() > 0.9);
    }

    #[test]
    fn outside_all_influence_uses_nearest() {
        let guides = vec![
            Guide {
                position: Vec2::new(0.1, 0.1),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.05,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.9, 0.9),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.05,
                ..Guide::default()
            },
        ];
        // Query near the first guide but outside influence
        let dir = direction_at(Vec2::new(0.15, 0.15), &guides);
        // Nearest is guide[0] at (0.1, 0.1) with dir (0, 1)
        assert!(approx_vec2(dir, Vec2::Y));
    }

    #[test]
    fn influence_radius_respected() {
        let guides = vec![
            Guide {
                position: Vec2::new(0.0, 0.0),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.1,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(1.0, 1.0),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.1,
                ..Guide::default()
            },
        ];
        // Far from both — should fallback to nearest
        let dir = direction_at(Vec2::new(0.5, 0.5), &guides);
        // Both equidistant, so either direction is acceptable
        assert!(is_normalized(dir));
    }

    #[test]
    fn distance_falloff_closer_guide_dominates() {
        let guides = vec![
            Guide {
                position: Vec2::new(0.3, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.9, 0.5),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.5,
                ..Guide::default()
            },
        ];
        // Query close to guide[0]
        let dir = direction_at(Vec2::new(0.35, 0.5), &guides);
        // Should be much closer to horizontal than vertical
        assert!(dir.x > 0.9);
    }

    #[test]
    fn all_vectors_normalized() {
        let guides = vec![
            Guide {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.4,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.4,
                ..Guide::default()
            },
        ];
        for i in 0..10 {
            let x = i as f32 / 10.0 + 0.05;
            let dir = direction_at(Vec2::new(x, 0.5), &guides);
            assert!(
                is_normalized(dir),
                "not normalized at x={x}: length={}",
                dir.length()
            );
        }
    }

    #[test]
    fn generate_field_correct_length() {
        let guides = vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 1.0,
            ..Guide::default()
        }];
        let field = generate_direction_field(&guides, 16);
        assert_eq!(field.len(), 16 * 16);
    }

    #[test]
    fn generate_field_all_normalized() {
        let guides = vec![
            Guide {
                position: Vec2::new(0.25, 0.5),
                direction: Vec2::X,
                influence: 0.5,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.75, 0.5),
                direction: Vec2::Y,
                influence: 0.5,
                ..Guide::default()
            },
        ];
        let field = generate_direction_field(&guides, 32);
        for (i, v) in field.iter().enumerate() {
            assert!(
                is_normalized(*v),
                "not normalized at index {i}: length={}",
                v.length()
            );
        }
    }

    #[test]
    fn deterministic_same_inputs_same_output() {
        let guides = vec![
            Guide {
                position: Vec2::new(0.3, 0.3),
                direction: Vec2::new(1.0, 0.5).normalize(),
                influence: 0.4,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.7, 0.7),
                direction: Vec2::new(-0.5, 1.0).normalize(),
                influence: 0.4,
                ..Guide::default()
            },
        ];
        let field1 = generate_direction_field(&guides, 32);
        let field2 = generate_direction_field(&guides, 32);
        assert_eq!(field1.len(), field2.len());
        for (a, b) in field1.iter().zip(field2.iter()) {
            assert!(approx_vec2(*a, *b));
        }
    }

    #[test]
    fn uniform_field_with_single_guide_full_coverage() {
        let guides = vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::new(1.0, 0.0),
            influence: 1.5,
            ..Guide::default()
        }];
        let field = generate_direction_field(&guides, 16);
        for v in &field {
            assert!(approx_vec2(*v, Vec2::X));
        }
    }

    #[test]
    fn canonicalization_negative_direction_equivalent() {
        // (0,-1) and (0,1) are the same headless direction
        let guides_pos = vec![
            Guide {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.5,
                ..Guide::default()
            },
        ];
        let guides_neg = vec![
            Guide {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::new(0.0, -1.0), // flipped
                influence: 0.5,
                ..Guide::default()
            },
        ];
        // Should produce identical results at every point
        for i in 0..10 {
            let x = i as f32 / 10.0 + 0.05;
            let uv = Vec2::new(x, 0.5);
            let a = direction_at(uv, &guides_pos);
            let b = direction_at(uv, &guides_neg);
            assert!(approx_vec2(a, b), "mismatch at x={x}: pos={a:?} neg={b:?}");
        }
    }

    #[test]
    fn three_guides_smooth_fan() {
        // Three guides in a fan: 0°, 45°, 90°
        let guides = vec![
            Guide {
                position: Vec2::new(0.1, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::new(FRAC_1_SQRT_2, FRAC_1_SQRT_2),
                influence: 0.5,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.9, 0.5),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.5,
                ..Guide::default()
            },
        ];
        // At the center guide, result should be ~45° (dominated by the closest guide)
        let dir = direction_at(Vec2::new(0.5, 0.5), &guides);
        assert!(is_normalized(dir));
        let angle = dir.y.atan2(dir.x);
        let expected_angle = std::f32::consts::FRAC_PI_4;
        assert!(
            (angle - expected_angle).abs() < 0.15,
            "angle={:.1}° expected ~45°",
            angle.to_degrees()
        );
    }

    #[test]
    fn three_guides_wide_spread() {
        // Three guides at 10°, 80°, 150° — wide spread but all in upper half
        let guides = vec![
            Guide {
                position: Vec2::new(0.1, 0.5),
                direction: Vec2::new(10.0_f32.to_radians().cos(), 10.0_f32.to_radians().sin()),
                influence: 0.5,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::new(80.0_f32.to_radians().cos(), 80.0_f32.to_radians().sin()),
                influence: 0.5,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.9, 0.5),
                direction: Vec2::new(150.0_f32.to_radians().cos(), 150.0_f32.to_radians().sin()),
                influence: 0.5,
                ..Guide::default()
            },
        ];
        // At the center, the 80° guide dominates (d=0 vs d=0.4)
        let dir = direction_at(Vec2::new(0.5, 0.5), &guides);
        assert!(is_normalized(dir));
        let angle = dir.y.atan2(dir.x).to_degrees();
        // Should be close to the center guide's 80°
        assert!(
            (angle - 80.0).abs() < 15.0,
            "angle={angle:.1}° expected near 80°"
        );
    }

    #[test]
    fn partial_influence_overlap_only_one_in_range() {
        // Two guides, but only one is within influence at the query point
        let guides = vec![
            Guide {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.2, // covers up to x=0.4
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.2, // covers from x=0.6
                ..Guide::default()
            },
        ];
        // Query at x=0.3: only guide[0] is in range
        let dir = direction_at(Vec2::new(0.3, 0.5), &guides);
        assert!(approx_vec2(dir, Vec2::X));
        // Query at x=0.7: only guide[1] is in range
        let dir = direction_at(Vec2::new(0.7, 0.5), &guides);
        assert!(approx_vec2(dir, Vec2::Y));
    }
}

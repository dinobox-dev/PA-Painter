use glam::Vec2;

use crate::types::GuideVertex;

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

/// Compute direction at a UV coordinate given a set of guide vertices.
///
/// Returns a normalized Vec2 representing the stroke direction.
/// If no guides have influence at this point, returns the nearest guide's direction.
/// If guides is empty, returns Vec2::X (default horizontal).
///
/// Uses smoothstep-based weighting with reference-based sign alignment
/// to handle the 180° symmetry of headless direction vectors. All directions
/// are canonicalized to the upper half-plane before blending.
pub fn direction_at(uv: Vec2, guides: &[GuideVertex]) -> Vec2 {
    if guides.is_empty() {
        return Vec2::X;
    }

    if guides.len() == 1 {
        let dir = guides[0].direction.normalize_or_zero();
        return if dir.length_squared() > 0.0 { dir } else { Vec2::X };
    }

    // Collect (weight, canonicalized direction) for guides within influence
    let mut weighted: Vec<(f32, Vec2)> = Vec::new();

    for g in guides {
        let d = uv.distance(g.position);

        if d > g.influence {
            continue;
        }

        let w = guide_weight(d, g.influence);
        let dir = g.direction.normalize_or_zero();
        if dir.length_squared() < 1e-12 {
            continue;
        }
        weighted.push((w, canonicalize(dir)));
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
            .unwrap();
        let dir = nearest.direction.normalize_or_zero();
        return if dir.length_squared() > 0.0 { dir } else { Vec2::X };
    }

    // Use the highest-weight direction as reference for sign alignment
    let ref_dir = weighted
        .iter()
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap()
        .1;

    // IDW-weighted mean with sign flipping for 180° symmetry
    let mut sum = Vec2::ZERO;
    for &(w, dir) in &weighted {
        let aligned = if dir.dot(ref_dir) < 0.0 { -dir } else { dir };
        sum += w * aligned;
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
pub fn generate_direction_field(guides: &[GuideVertex], resolution: u32) -> Vec<Vec2> {
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
    /// Resolution is capped at 512 to limit memory usage.
    pub fn new(guides: &[GuideVertex], resolution: u32) -> Self {
        let res = resolution.min(512);
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
        let mut d10 = self.data[y0 * w + x1];
        let mut d01 = self.data[y1 * w + x0];
        let mut d11 = self.data[y1 * w + x1];

        // Hemisphere alignment: align all to d00 to prevent cancellation
        if d10.dot(d00) < 0.0 {
            d10 = -d10;
        }
        if d01.dot(d00) < 0.0 {
            d01 = -d01;
        }
        if d11.dot(d00) < 0.0 {
            d11 = -d11;
        }

        let top = d00.lerp(d10, fx);
        let bottom = d01.lerp(d11, fx);
        let result = top.lerp(bottom, fy);

        if result.length_squared() < 1e-12 {
            d00
        } else {
            result.normalize()
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
        let guides = vec![GuideVertex {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::new(1.0, 0.0),
            influence: 0.4,
        }];
        let dir = direction_at(Vec2::new(0.3, 0.5), &guides);
        assert!(approx_vec2(dir, Vec2::X));
        assert!(is_normalized(dir));
    }

    #[test]
    fn single_guide_vertical() {
        let guides = vec![GuideVertex {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::new(0.0, 1.0),
            influence: 0.4,
        }];
        let dir = direction_at(Vec2::new(0.5, 0.5), &guides);
        assert!(approx_vec2(dir, Vec2::Y));
        assert!(is_normalized(dir));
    }

    #[test]
    fn single_guide_diagonal() {
        let guides = vec![GuideVertex {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::new(1.0, 1.0),
            influence: 0.5,
        }];
        let dir = direction_at(Vec2::new(0.5, 0.5), &guides);
        let expected = Vec2::new(FRAC_1_SQRT_2, FRAC_1_SQRT_2);
        assert!(approx_vec2(dir, expected));
        assert!(is_normalized(dir));
    }

    #[test]
    fn two_guides_same_direction() {
        let guides = vec![
            GuideVertex {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
            },
            GuideVertex {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
            },
        ];
        let dir = direction_at(Vec2::new(0.5, 0.5), &guides);
        assert!(approx_vec2(dir, Vec2::X));
    }

    #[test]
    fn two_guides_90_degrees_at_midpoint() {
        let guides = vec![
            GuideVertex {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
            },
            GuideVertex {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.5,
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
            GuideVertex {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
            },
            GuideVertex {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::new(-1.0, 0.0),
                influence: 0.5,
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
            GuideVertex {
                position: Vec2::new(0.1, 0.1),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.05,
            },
            GuideVertex {
                position: Vec2::new(0.9, 0.9),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.05,
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
            GuideVertex {
                position: Vec2::new(0.0, 0.0),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.1,
            },
            GuideVertex {
                position: Vec2::new(1.0, 1.0),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.1,
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
            GuideVertex {
                position: Vec2::new(0.3, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
            },
            GuideVertex {
                position: Vec2::new(0.9, 0.5),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.5,
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
            GuideVertex {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.4,
            },
            GuideVertex {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.4,
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
        let guides = vec![GuideVertex {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 1.0,
        }];
        let field = generate_direction_field(&guides, 16);
        assert_eq!(field.len(), 16 * 16);
    }

    #[test]
    fn generate_field_all_normalized() {
        let guides = vec![
            GuideVertex {
                position: Vec2::new(0.25, 0.5),
                direction: Vec2::X,
                influence: 0.5,
            },
            GuideVertex {
                position: Vec2::new(0.75, 0.5),
                direction: Vec2::Y,
                influence: 0.5,
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
            GuideVertex {
                position: Vec2::new(0.3, 0.3),
                direction: Vec2::new(1.0, 0.5).normalize(),
                influence: 0.4,
            },
            GuideVertex {
                position: Vec2::new(0.7, 0.7),
                direction: Vec2::new(-0.5, 1.0).normalize(),
                influence: 0.4,
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
        let guides = vec![GuideVertex {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::new(1.0, 0.0),
            influence: 1.5,
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
            GuideVertex {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
            },
            GuideVertex {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.5,
            },
        ];
        let guides_neg = vec![
            GuideVertex {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
            },
            GuideVertex {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::new(0.0, -1.0), // flipped
                influence: 0.5,
            },
        ];
        // Should produce identical results at every point
        for i in 0..10 {
            let x = i as f32 / 10.0 + 0.05;
            let uv = Vec2::new(x, 0.5);
            let a = direction_at(uv, &guides_pos);
            let b = direction_at(uv, &guides_neg);
            assert!(
                approx_vec2(a, b),
                "mismatch at x={x}: pos={a:?} neg={b:?}"
            );
        }
    }

    #[test]
    fn three_guides_smooth_fan() {
        // Three guides in a fan: 0°, 45°, 90°
        let guides = vec![
            GuideVertex {
                position: Vec2::new(0.1, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
            },
            GuideVertex {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::new(FRAC_1_SQRT_2, FRAC_1_SQRT_2),
                influence: 0.5,
            },
            GuideVertex {
                position: Vec2::new(0.9, 0.5),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.5,
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
            GuideVertex {
                position: Vec2::new(0.1, 0.5),
                direction: Vec2::new(10.0_f32.to_radians().cos(), 10.0_f32.to_radians().sin()),
                influence: 0.5,
            },
            GuideVertex {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::new(80.0_f32.to_radians().cos(), 80.0_f32.to_radians().sin()),
                influence: 0.5,
            },
            GuideVertex {
                position: Vec2::new(0.9, 0.5),
                direction: Vec2::new(
                    150.0_f32.to_radians().cos(),
                    150.0_f32.to_radians().sin(),
                ),
                influence: 0.5,
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
            GuideVertex {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.2, // covers up to x=0.4
            },
            GuideVertex {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.2, // covers from x=0.6
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

use glam::Vec2;

use crate::math::perpendicular;
use crate::types::StrokePath;

/// Transform table mapping local frame coordinates to UV coordinates.
pub struct LocalFrameTransform {
    /// UV coordinate for each local pixel. Row-major: uv_map[ly * width + lx]
    /// Pixels that map outside UV bounds store Vec2::NAN.
    pub uv_map: Vec<Vec2>,
    pub width: usize,  // stroke_length_px + margin
    pub height: usize, // brush_width_px + margin * 2
    pub margin: usize, // ridge_width_px
}

impl LocalFrameTransform {
    /// Get the UV coordinate for a local frame pixel.
    /// Returns None if the pixel maps outside UV bounds.
    pub fn local_to_uv(&self, lx: usize, ly: usize) -> Option<Vec2> {
        if lx >= self.width || ly >= self.height {
            return None;
        }
        let uv = self.uv_map[ly * self.width + lx];
        if uv.x.is_nan() {
            None
        } else {
            Some(uv)
        }
    }

    /// Convert a UV coordinate to pixel indices in the global map.
    pub fn uv_to_pixel(uv: Vec2, resolution: u32) -> (i32, i32) {
        let x = (uv.x * resolution as f32) as i32;
        let y = (uv.y * resolution as f32) as i32;
        (x, y)
    }
}

/// Build a local frame transform table for a stroke path.
///
/// - `path`: the stroke path (in UV space)
/// - `brush_width_px`: brush width in pixels
/// - `ridge_width_px`: ridge width in pixels (used as margin)
/// - `resolution`: output resolution (for UV <-> pixel conversion)
///
/// Returns the transform table that maps local frame pixels to UV coordinates.
pub fn build_local_frame(
    path: &StrokePath,
    brush_width_px: usize,
    ridge_width_px: usize,
    resolution: u32,
) -> LocalFrameTransform {
    let stroke_length_uv = path.arc_length();
    let stroke_length_px = (stroke_length_uv * resolution as f32).ceil() as usize;
    let margin = ridge_width_px;

    let local_width = stroke_length_px + margin;
    let local_height = brush_width_px + margin * 2;

    let mut uv_map = vec![Vec2::NAN; local_height * local_width];

    if stroke_length_px == 0 || local_width == 0 || local_height == 0 {
        return LocalFrameTransform {
            uv_map,
            width: local_width,
            height: local_height,
            margin,
        };
    }

    for lx in 0..local_width {
        // Map local x to path parameter t
        let t = (lx as f32 - margin as f32) / stroke_length_px as f32;
        let t_clamped = t.clamp(0.0, 1.0);

        let center = path.sample(t_clamped);
        let tangent = path.tangent(t_clamped);
        let normal = perpendicular(tangent);

        for ly in 0..local_height {
            let offset_px = ly as f32 - (local_height as f32 / 2.0);
            let offset_uv = offset_px / resolution as f32;

            let mut uv = center + normal * offset_uv;

            // Handle front ridge zone: push back along reverse tangent
            if t < 0.0 {
                let backtrack_uv = t * stroke_length_uv; // negative value
                uv += tangent * backtrack_uv;
            }

            // Mark out-of-bounds
            if uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 {
                uv_map[ly * local_width + lx] = Vec2::NAN;
            } else {
                uv_map[ly * local_width + lx] = uv;
            }
        }
    }

    LocalFrameTransform {
        uv_map,
        width: local_width,
        height: local_height,
        margin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-4;

    fn make_path(pts: Vec<Vec2>) -> StrokePath {
        StrokePath::new(pts, 0, 0)
    }

    #[test]
    fn dimensions() {
        // path_len = 100px worth of UV at res=512 → arc_length = 100/512
        let arc = 100.0 / 512.0;
        let path = make_path(vec![Vec2::new(0.3, 0.5), Vec2::new(0.3 + arc, 0.5)]);
        let transform = build_local_frame(&path, 30, 5, 512);

        // stroke_length_px = ceil(arc * 512) = 100
        assert_eq!(transform.width, 100 + 5, "width = stroke_len + margin");
        assert_eq!(transform.height, 30 + 5 * 2, "height = brush + 2*margin");
        assert_eq!(transform.margin, 5);
        assert_eq!(transform.uv_map.len(), 105 * 40);
    }

    #[test]
    fn straight_horizontal_center_row() {
        // Horizontal path from (0.1, 0.5) to (0.9, 0.5)
        let path = make_path(vec![Vec2::new(0.1, 0.5), Vec2::new(0.9, 0.5)]);
        let transform = build_local_frame(&path, 20, 5, 512);

        let center_ly = transform.height / 2;

        // Center row should map to points on the path
        for lx in transform.margin..transform.width {
            if let Some(uv) = transform.local_to_uv(lx, center_ly) {
                // y should be ~0.5
                assert!(
                    (uv.y - 0.5).abs() < 0.01,
                    "center row lx={} maps to y={:.4}, expected ~0.5",
                    lx,
                    uv.y
                );
                // x should be between 0.1 and 0.9
                assert!(
                    uv.x >= 0.1 - 0.01 && uv.x <= 0.9 + 0.01,
                    "center row lx={} maps to x={:.4}, expected [0.1, 0.9]",
                    lx,
                    uv.x
                );
            }
        }
    }

    #[test]
    fn straight_vertical_center_row() {
        // Vertical path from (0.5, 0.1) to (0.5, 0.9)
        let path = make_path(vec![Vec2::new(0.5, 0.1), Vec2::new(0.5, 0.9)]);
        let transform = build_local_frame(&path, 20, 5, 512);

        let center_ly = transform.height / 2;

        for lx in transform.margin..transform.width {
            if let Some(uv) = transform.local_to_uv(lx, center_ly) {
                // x should be ~0.5
                assert!(
                    (uv.x - 0.5).abs() < 0.01,
                    "center row lx={} maps to x={:.4}, expected ~0.5",
                    lx,
                    uv.x
                );
                // y should be between 0.1 and 0.9
                assert!(
                    uv.y >= 0.1 - 0.01 && uv.y <= 0.9 + 0.01,
                    "center row lx={} maps to y={:.4}, expected [0.1, 0.9]",
                    lx,
                    uv.y
                );
            }
        }
    }

    #[test]
    fn center_pixel_maps_to_path_midpoint() {
        let path = make_path(vec![Vec2::new(0.2, 0.3), Vec2::new(0.8, 0.7)]);
        let transform = build_local_frame(&path, 20, 5, 512);

        let stroke_length_px =
            (path.arc_length() * 512.0).ceil() as usize;
        let lx = transform.margin + stroke_length_px / 2;
        let ly = transform.height / 2;

        let uv = transform.local_to_uv(lx, ly).expect("center should be valid");
        let expected = path.sample(0.5);

        assert!(
            (uv - expected).length() < 0.01,
            "center pixel maps to ({:.4}, {:.4}), expected ({:.4}, {:.4})",
            uv.x,
            uv.y,
            expected.x,
            expected.y
        );
    }

    #[test]
    fn top_edge_offset() {
        // Horizontal path in center of UV space
        let path = make_path(vec![Vec2::new(0.2, 0.5), Vec2::new(0.8, 0.5)]);
        let brush_width_px = 20;
        let ridge_width_px = 5;
        let resolution = 512u32;
        let transform = build_local_frame(&path, brush_width_px, ridge_width_px, resolution);

        // Sample at body midpoint, top edge (ly=0)
        let lx = transform.margin + (transform.width - transform.margin) / 2;
        let ly_top = 0;
        let ly_center = transform.height / 2;

        let uv_top = transform.local_to_uv(lx, ly_top);
        let uv_center = transform.local_to_uv(lx, ly_center);

        if let (Some(top), Some(center)) = (uv_top, uv_center) {
            let dist_px = (top - center).length() * resolution as f32;
            let expected_px = transform.height as f32 / 2.0;
            assert!(
                (dist_px - expected_px).abs() < 1.5,
                "top edge offset = {:.1}px, expected ~{:.1}px",
                dist_px,
                expected_px
            );
        }
    }

    #[test]
    fn normal_offsets_perpendicular() {
        // Horizontal path: normal should be vertical
        let path = make_path(vec![Vec2::new(0.2, 0.5), Vec2::new(0.8, 0.5)]);
        let transform = build_local_frame(&path, 20, 5, 512);

        let lx = transform.margin + (transform.width - transform.margin) / 2;
        let ly_top = transform.margin; // top of body
        let ly_bot = transform.margin + 20 - 1; // bottom of body

        let uv_top = transform.local_to_uv(lx, ly_top).unwrap();
        let uv_bot = transform.local_to_uv(lx, ly_bot).unwrap();

        // For a horizontal path, the normal offset should be purely in y
        assert!(
            (uv_top.x - uv_bot.x).abs() < EPS,
            "normal offset should be vertical: top.x={:.5}, bot.x={:.5}",
            uv_top.x,
            uv_bot.x
        );
        // Top should have smaller y (perpendicular CCW from rightward tangent = upward)
        assert!(
            uv_top.y < uv_bot.y,
            "top should be above bottom: top.y={:.4}, bot.y={:.4}",
            uv_top.y,
            uv_bot.y
        );
    }

    #[test]
    fn front_ridge_zone_maps_before_start() {
        let path = make_path(vec![Vec2::new(0.3, 0.5), Vec2::new(0.7, 0.5)]);
        let transform = build_local_frame(&path, 20, 5, 512);

        let ly = transform.height / 2;
        let path_start = path.sample(0.0);

        // Front ridge zone: lx < margin
        for lx in 0..transform.margin {
            if let Some(uv) = transform.local_to_uv(lx, ly) {
                // Should be to the left of path start (behind the path)
                assert!(
                    uv.x < path_start.x + EPS,
                    "front ridge lx={} maps to x={:.4}, should be before path start x={:.4}",
                    lx,
                    uv.x,
                    path_start.x
                );
            }
        }
    }

    #[test]
    fn out_of_bounds_marking() {
        // Path near the left edge of UV space
        let path = make_path(vec![Vec2::new(0.01, 0.5), Vec2::new(0.05, 0.5)]);
        let transform = build_local_frame(&path, 30, 10, 512);

        // Front ridge zone should have some NAN pixels (before path start, near UV=0)
        let ly = transform.height / 2;
        let mut has_nan = false;
        for lx in 0..transform.margin {
            if transform.local_to_uv(lx, ly).is_none() {
                has_nan = true;
                break;
            }
        }
        assert!(has_nan, "front ridge near UV boundary should have NAN pixels");
    }

    #[test]
    fn round_trip_straight_path() {
        // For a straight path, center row should lie exactly on the path
        let path = make_path(vec![Vec2::new(0.2, 0.5), Vec2::new(0.8, 0.5)]);
        let resolution = 512u32;
        let transform = build_local_frame(&path, 20, 5, resolution);

        let stroke_length_px =
            (path.arc_length() * resolution as f32).ceil() as usize;
        let center_ly = transform.height / 2;

        for lx in transform.margin..(transform.margin + stroke_length_px) {
            let t = (lx - transform.margin) as f32 / stroke_length_px as f32;
            let expected = path.sample(t);

            if let Some(uv) = transform.local_to_uv(lx, center_ly) {
                let err = (uv - expected).length();
                assert!(
                    err < 0.002,
                    "round-trip error at lx={}: got ({:.5}, {:.5}), expected ({:.5}, {:.5}), err={:.6}",
                    lx, uv.x, uv.y, expected.x, expected.y, err
                );
            }
        }
    }

    #[test]
    fn curved_path_local_frame() {
        // Create a gently curved path (quarter circle)
        let mut points = Vec::new();
        for i in 0..50 {
            let t = i as f32 / 49.0;
            let angle = t * std::f32::consts::FRAC_PI_2;
            points.push(Vec2::new(0.5 + 0.3 * angle.cos(), 0.5 + 0.3 * angle.sin()));
        }
        let path = StrokePath::new(points, 0, 0);
        let transform = build_local_frame(&path, 20, 5, 512);

        let stroke_length_px =
            (path.arc_length() * 512.0).ceil() as usize;

        // Verify: center row maps close to path points
        let center_ly = transform.height / 2;
        for lx in transform.margin..transform.width {
            if let Some(uv) = transform.local_to_uv(lx, center_ly) {
                let t =
                    (lx - transform.margin) as f32 / stroke_length_px as f32;
                let t_clamped = t.clamp(0.0, 1.0);
                let expected = path.sample(t_clamped);
                assert!(
                    (uv - expected).length() < 0.01,
                    "curved path: lx={}, uv=({:.4}, {:.4}), expected=({:.4}, {:.4})",
                    lx,
                    uv.x,
                    uv.y,
                    expected.x,
                    expected.y
                );
            }
        }
    }

    #[test]
    fn uv_to_pixel_conversion() {
        let (x, y) = LocalFrameTransform::uv_to_pixel(Vec2::new(0.5, 0.25), 1024);
        assert_eq!(x, 512);
        assert_eq!(y, 256);
    }

    #[test]
    fn local_to_uv_out_of_bounds_indices() {
        let path = make_path(vec![Vec2::new(0.3, 0.5), Vec2::new(0.7, 0.5)]);
        let transform = build_local_frame(&path, 20, 5, 512);

        assert!(transform.local_to_uv(transform.width, 0).is_none());
        assert!(transform.local_to_uv(0, transform.height).is_none());
        assert!(
            transform
                .local_to_uv(transform.width + 10, transform.height + 10)
                .is_none()
        );
    }

    #[test]
    fn empty_path() {
        let path = make_path(vec![Vec2::new(0.5, 0.5)]);
        let transform = build_local_frame(&path, 20, 5, 512);
        // Single-point path has arc_length=0, stroke_length_px=0
        // width = 0 + 5 = 5, height = 20 + 10 = 30
        assert_eq!(transform.width, 5);
        assert_eq!(transform.height, 30);
    }

    #[test]
    fn diagonal_path_center_row() {
        // 45-degree diagonal path
        let path = make_path(vec![Vec2::new(0.2, 0.2), Vec2::new(0.8, 0.8)]);
        let transform = build_local_frame(&path, 20, 5, 512);

        let center_ly = transform.height / 2;

        // Center row should map to points on the diagonal
        for lx in transform.margin..transform.width {
            if let Some(uv) = transform.local_to_uv(lx, center_ly) {
                // On a 45-degree line, x and y should be approximately equal
                // (relative to path endpoints)
                let dx = uv.x - 0.2;
                let dy = uv.y - 0.2;
                assert!(
                    (dx - dy).abs() < 0.02,
                    "diagonal: lx={}, uv=({:.4}, {:.4}), dx={:.4}, dy={:.4}",
                    lx,
                    uv.x,
                    uv.y,
                    dx,
                    dy
                );
            }
        }
    }

    // ── Visual Inspection Tests (output PNGs to test/output/) ──

    /// Render the local frame footprint onto a UV-space image.
    /// - Green: body zone
    /// - Red: front ridge zone
    /// - Blue: side ridge zones
    /// - Yellow dots: path centerline
    /// - White gridlines: every 10 local-frame pixels
    fn render_local_frame_to_uv_png(
        transform: &LocalFrameTransform,
        path: &StrokePath,
        resolution: u32,
        filename: &str,
    ) {
        let res = resolution as usize;
        let mut img = vec![30u8; res * res * 3]; // dark background

        let margin = transform.margin;
        let body_x_start = margin;
        let body_y_start = margin;
        let body_y_end = transform.height - margin;

        for ly in 0..transform.height {
            for lx in 0..transform.width {
                if let Some(uv) = transform.local_to_uv(lx, ly) {
                    let px = (uv.x * resolution as f32) as i32;
                    let py = (uv.y * resolution as f32) as i32;
                    if px < 0 || px >= res as i32 || py < 0 || py >= res as i32 {
                        continue;
                    }
                    let idx = (py as usize * res + px as usize) * 3;

                    let in_body_x = lx >= body_x_start;
                    let in_body_y = ly >= body_y_start && ly < body_y_end;

                    // Grid lines (white) every 10px in local frame
                    if lx % 10 == 0 || ly % 10 == 0 {
                        img[idx] = 200;
                        img[idx + 1] = 200;
                        img[idx + 2] = 200;
                    } else if in_body_x && in_body_y {
                        // Body: green
                        img[idx] = 40;
                        img[idx + 1] = 160;
                        img[idx + 2] = 40;
                    } else if !in_body_x {
                        // Front ridge: red
                        img[idx] = 180;
                        img[idx + 1] = 40;
                        img[idx + 2] = 40;
                    } else {
                        // Side ridge: blue
                        img[idx] = 40;
                        img[idx + 1] = 40;
                        img[idx + 2] = 180;
                    }
                }
            }
        }

        // Draw path centerline in yellow
        for i in 0..200 {
            let t = i as f32 / 199.0;
            let uv = path.sample(t);
            let px = (uv.x * resolution as f32) as i32;
            let py = (uv.y * resolution as f32) as i32;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let x = px + dx;
                    let y = py + dy;
                    if x >= 0 && x < res as i32 && y >= 0 && y < res as i32 {
                        let idx = (y as usize * res + x as usize) * 3;
                        img[idx] = 255;
                        img[idx + 1] = 255;
                        img[idx + 2] = 0;
                    }
                }
            }
        }

        let out = crate::test_module_output_dir("local_frame").join(filename);
        image::save_buffer(&out, &img, resolution, resolution, image::ColorType::Rgb8)
            .unwrap();
        eprintln!("Wrote visual test: {}", out.display());
    }

    #[test]
    fn visual_straight_horizontal() {
        // INSPECT: Clean horizontal rectangle. Green body, red cap on left,
        // blue stripes top/bottom. White grid lines should be parallel.
        // Yellow centerline runs through the middle of green zone.
        let path = make_path(vec![Vec2::new(0.1, 0.5), Vec2::new(0.9, 0.5)]);
        let transform = build_local_frame(&path, 30, 8, 512);
        render_local_frame_to_uv_png(&transform, &path, 512, "straight_horizontal.png");
    }

    #[test]
    fn visual_quarter_circle() {
        // INSPECT: Rectangle bends along a quarter-circle arc.
        // Grid lines should fan out on the outside of the curve (no overlaps).
        // Yellow path should bisect the green zone exactly.
        // No gaps between grid cells.
        let mut points = Vec::new();
        for i in 0..80 {
            let t = i as f32 / 79.0;
            let angle = t * std::f32::consts::FRAC_PI_2;
            points.push(Vec2::new(0.5 + 0.3 * angle.cos(), 0.2 + 0.3 * angle.sin()));
        }
        let path = StrokePath::new(points, 0, 0);
        let transform = build_local_frame(&path, 30, 8, 512);
        render_local_frame_to_uv_png(&transform, &path, 512, "quarter_circle.png");
    }

    #[test]
    fn visual_s_curve() {
        // INSPECT: S-shaped stroke. Check for:
        // - No grid line crossings (would indicate twisted mapping)
        // - Smooth curvature transitions
        // - Consistent width along the stroke
        // - Front ridge cap at the start
        let mut points = Vec::new();
        for i in 0..100 {
            let t = i as f32 / 99.0;
            let x = 0.15 + t * 0.7;
            let y = 0.5 + 0.15 * (t * std::f32::consts::TAU).sin();
            points.push(Vec2::new(x, y));
        }
        let path = StrokePath::new(points, 0, 0);
        let transform = build_local_frame(&path, 30, 8, 512);
        render_local_frame_to_uv_png(&transform, &path, 512, "s_curve.png");
    }

    #[test]
    fn stroke_length_sync_with_phase02() {
        // Verify stroke_length_px computation matches the formula from Phase 02
        let path = make_path(vec![Vec2::new(0.1, 0.5), Vec2::new(0.7, 0.5)]);
        let resolution = 512u32;
        let transform = build_local_frame(&path, 30, 5, resolution);

        let expected_stroke_len =
            (path.arc_length() * resolution as f32).ceil() as usize;
        let actual_stroke_len = transform.width - transform.margin;

        assert_eq!(
            actual_stroke_len, expected_stroke_len,
            "stroke_length_px mismatch with Phase 02 formula"
        );
    }
}

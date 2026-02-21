use std::collections::HashMap;

use glam::Vec2;

use crate::direction_field::DirectionField;
use crate::math::rotate_vec2;
use crate::rng::SeededRng;
use crate::types::{PaintLayer, StrokePath, StrokeParams};

/// Generate seed points across the full UV space [0,1]².
///
/// Returns UV-space seed points distributed on a uniform grid with 20% jitter.
pub fn generate_seeds(params: &StrokeParams, resolution: u32) -> Vec<Vec2> {
    let spacing = params.brush_width / resolution as f32 * params.stroke_spacing;
    let jitter_amount = spacing * 0.2;
    let mut rng = SeededRng::new(params.seed);

    let mut seeds = Vec::new();
    let mut y = 0.0f32;
    while y <= 1.0 {
        let mut x = 0.0f32;
        while x <= 1.0 {
            let pos = Vec2::new(x, y) + rng.random_in_circle(jitter_amount);

            if pos.x >= 0.0 && pos.x <= 1.0 && pos.y >= 0.0 && pos.y <= 1.0 {
                seeds.push(pos);
            }

            x += spacing;
        }
        y += spacing;
    }

    seeds
}

/// Trace a single streamline from a seed point.
///
/// The seed must be within UV [0,1]² bounds. The stroke traces along the
/// direction field until it hits a UV boundary, exceeds curvature limits,
/// or reaches the target length.
///
/// Returns `None` if the resulting path is shorter than `brush_width * 2` (in UV
/// units).
pub fn trace_streamline(
    seed: Vec2,
    field: &DirectionField,
    params: &StrokeParams,
    resolution: u32,
    rng: &mut SeededRng,
) -> Option<Vec<Vec2>> {
    let step_size_uv = 1.0 / resolution as f32;

    // Consume RNG for target length FIRST.
    // Power distribution: target = max * U^0.5 gives linearly increasing density
    // toward max_length (PDF ∝ t), with median ≈ 0.707 * max.
    let max_length_uv = params.max_stroke_length / resolution as f32;
    let target_length = max_length_uv * rng.next_f32().sqrt();

    let mut pos = seed;
    let mut prev_dir = field.sample(pos);

    // ── Tracing phase ──
    let mut path = vec![pos];
    let mut length = 0.0_f32;

    let max_turn_rad = params.max_turn_angle.to_radians();
    let angle_var_rad = params.angle_variation.to_radians();

    while length < target_length {
        let mut dir = field.sample(pos);

        // Ensure consistent tracing direction: align with prev_dir
        if dir.dot(prev_dir) < 0.0 {
            dir = -dir;
        }

        // Apply angle deviation (gradual)
        let angle_offset = (rng.next_f32() - 0.5) * angle_var_rad * 2.0;
        dir = rotate_vec2(dir, angle_offset * 0.1);

        // Ensure dir is normalized
        dir = dir.normalize_or_zero();
        if dir == Vec2::ZERO {
            break;
        }

        // Curvature limit
        let dot = prev_dir.dot(dir).clamp(-1.0, 1.0);
        let turn = dot.acos();
        if turn > max_turn_rad {
            break;
        }

        let next_pos = pos + dir * step_size_uv;

        // UV boundary check
        if next_pos.x < 0.0 || next_pos.x > 1.0 || next_pos.y < 0.0 || next_pos.y > 1.0 {
            break;
        }

        path.push(next_pos);
        prev_dir = dir;
        pos = next_pos;
        length += step_size_uv;
    }

    // Minimum length filter
    let min_length = params.brush_width / resolution as f32 * 2.0;
    if length < min_length {
        return None;
    }

    Some(path)
}

/// Filter paths by removing those that overlap excessively with existing paths.
///
/// A path is removed if >= 70% of its points are within `brush_width_uv * 0.3`
/// of any accepted path's centerline points.
///
/// Uses a spatial grid index for O(n × m × k) performance instead of brute-force
/// O(n² × m²), where k is the average number of points per grid cell.
pub fn filter_overlapping_paths(paths: &mut Vec<Vec<Vec2>>, brush_width_uv: f32) {
    let threshold_dist = brush_width_uv * 0.3;
    let threshold_sq = threshold_dist * threshold_dist;
    let overlap_ratio = 0.7;
    let cell_size = threshold_dist;
    let inv_cell = 1.0 / cell_size;

    let mut grid: HashMap<(i32, i32), Vec<Vec2>> = HashMap::new();
    let mut accepted: Vec<Vec<Vec2>> = Vec::new();

    for path in paths.drain(..) {
        let mut close_count = 0u32;
        for &point in &path {
            let cx = (point.x * inv_cell).floor() as i32;
            let cy = (point.y * inv_cell).floor() as i32;
            let mut is_close = false;
            'outer: for dx in -1..=1i32 {
                for dy in -1..=1i32 {
                    if let Some(cell_points) = grid.get(&(cx + dx, cy + dy)) {
                        for &ep in cell_points {
                            if (point - ep).length_squared() < threshold_sq {
                                is_close = true;
                                break 'outer;
                            }
                        }
                    }
                }
            }
            if is_close {
                close_count += 1;
            }
        }

        if accepted.is_empty() || (close_count as f32 / path.len() as f32) < overlap_ratio {
            for &point in &path {
                let cx = (point.x * inv_cell).floor() as i32;
                let cy = (point.y * inv_cell).floor() as i32;
                grid.entry((cx, cy)).or_default().push(point);
            }
            accepted.push(path);
        }
    }

    *paths = accepted;
}

/// Generate all stroke paths for a layer.
///
/// Returns paths in paint order (sorted by seed y-coordinate, top to bottom).
/// Each path is assigned a globally unique stroke ID: `(layer_index << 16) | index`.
pub fn generate_paths(layer: &PaintLayer, layer_index: u32, resolution: u32) -> Vec<StrokePath> {
    let seeds = generate_seeds(&layer.params, resolution);
    let field = DirectionField::new(&layer.guides, resolution);

    let mut rng = SeededRng::new(layer.params.seed);
    let mut raw_paths: Vec<Vec<Vec2>> = Vec::new();
    for seed_point in &seeds {
        if let Some(path) =
            trace_streamline(*seed_point, &field, &layer.params, resolution, &mut rng)
        {
            raw_paths.push(path);
        }
    }

    let brush_width_uv = layer.params.brush_width / resolution as f32;
    filter_overlapping_paths(&mut raw_paths, brush_width_uv);

    // Sort paths by seed point y-coordinate (top-to-bottom row order).
    // Critical for correct wet-on-wet compositing order in Phase 08.
    raw_paths.sort_by(|a, b| {
        let ya = a[0].y;
        let yb = b[0].y;
        ya.partial_cmp(&yb).unwrap()
    });

    // Convert to StrokePath with globally unique IDs
    raw_paths
        .into_iter()
        .enumerate()
        .map(|(i, path)| StrokePath::new(path, layer_index, (layer_index << 16) | (i as u32)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GuideVertex, PaintLayer, StrokeParams};

    fn make_layer() -> PaintLayer {
        PaintLayer {
            name: String::from("test"),
            order: 0,
            params: StrokeParams::default(),
            guides: vec![GuideVertex {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::X,
                influence: 1.5,
            }],
        }
    }

    // ── Seed Distribution Tests ──

    #[test]
    fn seed_grid_coverage() {
        let layer = make_layer();
        let seeds = generate_seeds(&layer.params, 512);
        // Expected: ~(512/30)^2 ≈ 291 seeds
        let expected = ((512.0 / 30.0) as u32).pow(2);
        let count = seeds.len() as u32;
        assert!(
            count > expected / 2 && count < expected * 2,
            "seed count {count} not near expected ~{expected}"
        );
    }

    #[test]
    fn seeds_within_uv_bounds() {
        let layer = make_layer();
        let seeds = generate_seeds(&layer.params, 512);
        assert!(!seeds.is_empty());
        for seed in &seeds {
            assert!(
                seed.x >= 0.0 && seed.x <= 1.0 && seed.y >= 0.0 && seed.y <= 1.0,
                "seed ({:.3}, {:.3}) is outside UV [0,1] bounds",
                seed.x,
                seed.y
            );
        }
    }

    #[test]
    fn seed_determinism() {
        let layer = make_layer();
        let seeds1 = generate_seeds(&layer.params, 512);
        let seeds2 = generate_seeds(&layer.params, 512);
        assert_eq!(seeds1.len(), seeds2.len());
        for (a, b) in seeds1.iter().zip(seeds2.iter()) {
            assert_eq!(a.x, b.x);
            assert_eq!(a.y, b.y);
        }
    }

    #[test]
    fn seed_jitter_bounds() {
        let layer = make_layer();
        let spacing = layer.params.brush_width / 512.0 * layer.params.stroke_spacing;
        let max_jitter = spacing * 0.2 * 2.0_f32.sqrt(); // diagonal of circle
        let grid_origin = Vec2::ZERO;

        let seeds = generate_seeds(&layer.params, 512);

        for seed in &seeds {
            let gx = ((seed.x - grid_origin.x) / spacing).round() * spacing + grid_origin.x;
            let gy = ((seed.y - grid_origin.y) / spacing).round() * spacing + grid_origin.y;
            let dist = (*seed - Vec2::new(gx, gy)).length();
            assert!(
                dist <= max_jitter + 1e-6,
                "seed ({:.4}, {:.4}) is {:.4} from grid ({:.4}, {:.4}), max jitter = {:.4}",
                seed.x,
                seed.y,
                dist,
                gx,
                gy,
                max_jitter
            );
        }
    }

    // ── Streamline Tracing Tests ──

    #[test]
    fn streamline_straight_horizontal() {
        let layer = make_layer();
        let mut params = StrokeParams::default();
        params.angle_variation = 0.0;

        let field = DirectionField::new(&layer.guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(Vec2::new(0.5, 0.5), &field, &params, 512, &mut rng);

        let path = path.expect("path should exist");
        for p in &path {
            assert!(
                (p.y - 0.5).abs() < 0.01,
                "point ({:.4}, {:.4}) deviates from y=0.5",
                p.x,
                p.y
            );
        }
    }

    #[test]
    fn streamline_straight_vertical() {
        let guides = vec![GuideVertex {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::Y,
            influence: 1.5,
        }];
        let mut params = StrokeParams::default();
        params.angle_variation = 0.0;

        let field = DirectionField::new(&guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(Vec2::new(0.5, 0.5), &field, &params, 512, &mut rng);

        let path = path.expect("path should exist");
        for p in &path {
            assert!(
                (p.x - 0.5).abs() < 0.01,
                "point ({:.4}, {:.4}) deviates from x=0.5",
                p.x,
                p.y
            );
        }
    }

    #[test]
    fn streamline_uv_boundary_termination() {
        let guides = vec![GuideVertex {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 1.5,
        }];
        let params = StrokeParams {
            angle_variation: 0.0,
            max_stroke_length: 500.0,
            ..StrokeParams::default()
        };

        let field = DirectionField::new(&guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(Vec2::new(0.9, 0.5), &field, &params, 512, &mut rng);

        if let Some(path) = path {
            let last = path.last().unwrap();
            assert!(
                last.x <= 1.01,
                "path should stop near x=1.0 UV boundary, got x={:.4}",
                last.x
            );
        }
    }

    #[test]
    fn streamline_min_length_filter() {
        let guides = vec![GuideVertex {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 1.5,
        }];
        // Very wide brush → high min_length, very short max → path too short
        let params = StrokeParams {
            brush_width: 100.0,
            max_stroke_length: 1.0,
            angle_variation: 0.0,
            ..StrokeParams::default()
        };

        let field = DirectionField::new(&guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(Vec2::new(0.5, 0.5), &field, &params, 512, &mut rng);

        // min_length = 100/512*2 ≈ 0.39 UV, but max target ≈ 0.002 UV
        assert!(path.is_none(), "short path should be filtered out");
    }

    #[test]
    fn streamline_length_variation() {
        let layer = make_layer();
        let params = StrokeParams {
            angle_variation: 0.0,
            ..StrokeParams::default()
        };

        let field = DirectionField::new(&layer.guides, 512);
        let mut lengths = Vec::new();
        let mut rng = SeededRng::new(42);
        for _ in 0..200 {
            if let Some(path) = trace_streamline(
                Vec2::new(0.1, 0.5),
                &field,
                &params,
                512,
                &mut rng,
            ) {
                let len: f32 = path
                    .windows(2)
                    .map(|w| (w[1] - w[0]).length())
                    .sum();
                lengths.push(len);
            }
        }

        assert!(lengths.len() >= 20, "need enough paths to check distribution");
        let min_len = lengths.iter().cloned().fold(f32::MAX, f32::min);
        let max_len = lengths.iter().cloned().fold(f32::MIN, f32::max);
        assert!(
            max_len > min_len * 1.5,
            "lengths should vary widely: min={min_len:.4}, max={max_len:.4}"
        );

        let max_length_uv = params.max_stroke_length / 512.0;
        let short_count = lengths.iter().filter(|&&l| l < max_length_uv * 0.3).count();
        let long_count = lengths.iter().filter(|&&l| l > max_length_uv * 0.7).count();
        assert!(short_count > 0, "should have some short strokes");
        assert!(long_count > 0, "should have some long strokes");
    }

    // ── Path Quality Filter Tests ──

    #[test]
    fn filter_no_overlap_keeps_all() {
        let brush_width_uv = 30.0 / 512.0;
        let mut paths = vec![
            vec![Vec2::new(0.1, 0.1), Vec2::new(0.2, 0.1)],
            vec![Vec2::new(0.1, 0.9), Vec2::new(0.2, 0.9)],
        ];
        filter_overlapping_paths(&mut paths, brush_width_uv);
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn filter_identical_paths_removes_duplicate() {
        let brush_width_uv = 30.0 / 512.0;
        let p: Vec<Vec2> = (0..20)
            .map(|i| Vec2::new(0.1 + i as f32 * 0.01, 0.5))
            .collect();
        let mut paths = vec![p.clone(), p];
        filter_overlapping_paths(&mut paths, brush_width_uv);
        assert_eq!(paths.len(), 1, "duplicate path should be removed");
    }

    #[test]
    fn filter_partial_overlap_keeps_both() {
        let brush_width_uv = 30.0 / 512.0;
        let offset = brush_width_uv * 0.5;
        let p1: Vec<Vec2> = (0..20)
            .map(|i| Vec2::new(0.1 + i as f32 * 0.01, 0.5))
            .collect();
        let p2: Vec<Vec2> = (0..20)
            .map(|i| Vec2::new(0.1 + i as f32 * 0.01, 0.5 + offset))
            .collect();
        let mut paths = vec![p1, p2];
        filter_overlapping_paths(&mut paths, brush_width_uv);
        assert_eq!(paths.len(), 2, "partially overlapping paths should both be kept");
    }

    // ── Full Pipeline Tests ──

    #[test]
    fn generate_paths_determinism() {
        let layer = make_layer();
        let paths1 = generate_paths(&layer, 0, 256);
        let paths2 = generate_paths(&layer, 0, 256);

        assert_eq!(paths1.len(), paths2.len());
        for (a, b) in paths1.iter().zip(paths2.iter()) {
            assert_eq!(a.points.len(), b.points.len());
            assert_eq!(a.stroke_id, b.stroke_id);
            assert_eq!(a.layer_index, b.layer_index);
            for (pa, pb) in a.points.iter().zip(b.points.iter()) {
                assert_eq!(pa.x, pb.x);
                assert_eq!(pa.y, pb.y);
            }
        }
    }

    #[test]
    fn generate_paths_sorted_by_y() {
        let layer = make_layer();
        let paths = generate_paths(&layer, 0, 256);

        assert!(!paths.is_empty(), "should generate some paths");
        for i in 1..paths.len() {
            assert!(
                paths[i].points[0].y >= paths[i - 1].points[0].y,
                "paths should be sorted by y: path[{}].y={:.4} < path[{}].y={:.4}",
                i,
                paths[i].points[0].y,
                i - 1,
                paths[i - 1].points[0].y
            );
        }
    }

    #[test]
    fn generate_paths_unique_stroke_ids() {
        let layer = make_layer();
        let paths = generate_paths(&layer, 0, 256);

        let mut ids: Vec<u32> = paths.iter().map(|p| p.stroke_id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), paths.len(), "all stroke IDs should be unique");
    }

    #[test]
    fn generate_paths_stroke_id_encoding() {
        let layer = make_layer();
        let paths = generate_paths(&layer, 3, 256);

        for (i, path) in paths.iter().enumerate() {
            assert_eq!(path.layer_index, 3);
            assert_eq!(
                path.stroke_id,
                (3 << 16) | (i as u32),
                "stroke_id encoding mismatch at index {i}"
            );
        }
    }

    #[test]
    fn area_coverage_90_percent() {
        let layer = make_layer();
        let resolution = 512u32;
        let paths = generate_paths(&layer, 0, resolution);

        let mut covered = vec![false; (resolution * resolution) as usize];
        let brush_radius_uv = layer.params.brush_width / resolution as f32 / 2.0;
        let r = (brush_radius_uv * resolution as f32).ceil() as i32;

        for path in &paths {
            for point in &path.points {
                let px = (point.x * resolution as f32) as i32;
                let py = (point.y * resolution as f32) as i32;
                for dy in -r..=r {
                    for dx in -r..=r {
                        if dx * dx + dy * dy <= r * r {
                            let x = px + dx;
                            let y = py + dy;
                            if x >= 0
                                && x < resolution as i32
                                && y >= 0
                                && y < resolution as i32
                            {
                                covered[(y * resolution as i32 + x) as usize] = true;
                            }
                        }
                    }
                }
            }
        }

        // Check coverage within interior [0.1, 0.9] to avoid UV edge effects
        let margin = (0.1 * resolution as f32) as u32;
        let inner = margin..(resolution - margin);
        let mut total = 0u32;
        let mut hit = 0u32;
        for py in inner.clone() {
            for px in inner.clone() {
                total += 1;
                if covered[(py * resolution + px) as usize] {
                    hit += 1;
                }
            }
        }

        let coverage = hit as f32 / total as f32;
        assert!(
            coverage >= 0.90,
            "Coverage {:.1}% is below 90%",
            coverage * 100.0
        );
    }

    #[test]
    fn paths_stay_within_uv() {
        let layer = make_layer();
        let paths = generate_paths(&layer, 0, 512);

        for path in &paths {
            for point in &path.points {
                assert!(
                    point.x >= 0.0 && point.x <= 1.0 && point.y >= 0.0 && point.y <= 1.0,
                    "path point ({:.4}, {:.4}) is outside UV [0,1] bounds",
                    point.x,
                    point.y
                );
            }
        }
    }

    #[test]
    fn streamline_curved_two_guides() {
        let guides = vec![
            GuideVertex {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::X,
                influence: 0.5,
            },
            GuideVertex {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::Y,
                influence: 0.5,
            },
        ];
        let params = StrokeParams {
            angle_variation: 0.0,
            ..StrokeParams::default()
        };

        let field = DirectionField::new(&guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(Vec2::new(0.2, 0.5), &field, &params, 512, &mut rng);

        let path = path.expect("curved path should exist");
        assert!(path.len() > 10, "path should have enough points");

        let first_dir = (path[1] - path[0]).normalize();
        let last_dir = (path[path.len() - 1] - path[path.len() - 2]).normalize();

        let dot = first_dir.dot(last_dir);
        assert!(
            dot < 0.99,
            "path should curve: dot between first and last direction = {dot:.4}"
        );
    }

    #[test]
    fn turn_angle_termination() {
        let guides = vec![
            GuideVertex {
                position: Vec2::new(0.3, 0.5),
                direction: Vec2::X,
                influence: 0.15,
            },
            GuideVertex {
                position: Vec2::new(0.7, 0.5),
                direction: Vec2::Y,
                influence: 0.15,
            },
        ];
        let params = StrokeParams {
            angle_variation: 0.0,
            max_turn_angle: 5.0,
            ..StrokeParams::default()
        };

        let field = DirectionField::new(&guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(Vec2::new(0.3, 0.5), &field, &params, 512, &mut rng);

        if let Some(path) = path {
            for w in path.windows(3) {
                let d1 = (w[1] - w[0]).normalize();
                let d2 = (w[2] - w[1]).normalize();
                let dot = d1.dot(d2).clamp(-1.0, 1.0);
                let angle = dot.acos().to_degrees();
                assert!(
                    angle <= 10.0,
                    "consecutive turn angle {angle:.1}° exceeds limit"
                );
            }
        }
    }

    // ── Visual Inspection Tests ──

    fn draw_paths_to_png(
        paths: &[StrokePath],
        resolution: u32,
        filename: &str,
    ) {
        let res = resolution as usize;
        let mut img = vec![40u8; res * res * 3]; // dark gray background

        for (idx, path) in paths.iter().enumerate() {
            let hue = (idx as f32 * 0.618034) % 1.0;
            let (r, g, b) = hue_to_rgb(hue);

            for point in &path.points {
                let px = (point.x * resolution as f32) as i32;
                let py = (point.y * resolution as f32) as i32;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let x = px + dx;
                        let y = py + dy;
                        if x >= 0 && x < resolution as i32 && y >= 0 && y < resolution as i32 {
                            let i = (y as usize * res + x as usize) * 3;
                            img[i] = r;
                            img[i + 1] = g;
                            img[i + 2] = b;
                        }
                    }
                }
            }
        }

        let path = crate::test_module_output_dir("path_placement").join(filename);
        image::save_buffer(
            &path,
            &img,
            resolution,
            resolution,
            image::ColorType::Rgb8,
        )
        .unwrap();
        eprintln!("Wrote visual test: {}", path.display());
    }

    fn hue_to_rgb(h: f32) -> (u8, u8, u8) {
        let h = h * 6.0;
        let c = 1.0_f32;
        let x = c * (1.0 - (h % 2.0 - 1.0).abs());
        let (r, g, b) = match h as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        (
            (r * 220.0 + 35.0) as u8,
            (g * 220.0 + 35.0) as u8,
            (b * 220.0 + 35.0) as u8,
        )
    }

    #[test]
    fn visual_horizontal_strokes() {
        let layer = make_layer();
        let resolution = 512;
        let paths = generate_paths(&layer, 0, resolution);

        eprintln!("visual_horizontal_strokes: {} paths generated", paths.len());
        draw_paths_to_png(&paths, resolution, "horizontal_strokes.png");
        assert!(!paths.is_empty());
    }

    #[test]
    fn visual_spiral_guides() {
        let layer = PaintLayer {
            name: String::from("spiral"),
            order: 0,
            params: StrokeParams {
                brush_width: 20.0,
                stroke_spacing: 1.0,
                angle_variation: 3.0,
                max_turn_angle: 30.0,
                ..StrokeParams::default()
            },
            guides: vec![
                GuideVertex {
                    position: Vec2::new(0.5, 0.2),
                    direction: Vec2::new(1.0, 0.0),
                    influence: 0.5,
                },
                GuideVertex {
                    position: Vec2::new(0.8, 0.5),
                    direction: Vec2::new(0.0, 1.0),
                    influence: 0.5,
                },
                GuideVertex {
                    position: Vec2::new(0.5, 0.8),
                    direction: Vec2::new(-1.0, 0.0),
                    influence: 0.5,
                },
                GuideVertex {
                    position: Vec2::new(0.2, 0.5),
                    direction: Vec2::new(0.0, -1.0),
                    influence: 0.5,
                },
            ],
        };
        let resolution = 512;
        let paths = generate_paths(&layer, 0, resolution);

        eprintln!("visual_spiral_guides: {} paths generated", paths.len());
        draw_paths_to_png(&paths, resolution, "spiral_guides.png");
        assert!(!paths.is_empty());
    }

    // ── Stroke Length Distribution Tests ──

    #[test]
    fn stroke_length_distribution_median() {
        let layer = make_layer();
        let params = StrokeParams {
            angle_variation: 0.0,
            ..StrokeParams::default()
        };

        let field = DirectionField::new(&layer.guides, 512);
        let mut lengths = Vec::new();
        let mut rng = SeededRng::new(123);
        for _ in 0..500 {
            if let Some(path) = trace_streamline(
                Vec2::new(0.1, 0.5),
                &field,
                &params,
                512,
                &mut rng,
            ) {
                let len: f32 = path
                    .windows(2)
                    .map(|w| (w[1] - w[0]).length())
                    .sum();
                lengths.push(len);
            }
        }

        lengths.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = lengths[lengths.len() / 2];
        let max_length_uv = params.max_stroke_length / 512.0;
        let expected_median = max_length_uv * 0.707;

        assert!(
            (median - expected_median).abs() < expected_median * 0.15,
            "median {median:.4} should be near expected {expected_median:.4} (±15%)"
        );
    }

    #[test]
    fn stroke_length_respects_max() {
        let layer = make_layer();
        let params = StrokeParams {
            angle_variation: 0.0,
            ..StrokeParams::default()
        };

        let max_length_uv = params.max_stroke_length / 512.0;
        let field = DirectionField::new(&layer.guides, 512);
        let mut rng = SeededRng::new(77);
        for i in 0..500 {
            if let Some(path) = trace_streamline(
                Vec2::new(0.1, 0.5),
                &field,
                &params,
                512,
                &mut rng,
            ) {
                let len: f32 = path
                    .windows(2)
                    .map(|w| (w[1] - w[0]).length())
                    .sum();
                assert!(
                    len <= max_length_uv + 0.005,
                    "path {i} length {len:.4} exceeds max {max_length_uv:.4}"
                );
            }
        }
    }

    #[test]
    fn stroke_length_one_rng_call() {
        let layer = make_layer();
        let params = StrokeParams {
            angle_variation: 0.0,
            ..StrokeParams::default()
        };

        let field = DirectionField::new(&layer.guides, 512);

        let mut rng1 = SeededRng::new(99);
        let mut rng2 = SeededRng::new(99);
        let path1 = trace_streamline(Vec2::new(0.5, 0.5), &field, &params, 512, &mut rng1);
        let path2 = trace_streamline(Vec2::new(0.5, 0.5), &field, &params, 512, &mut rng2);

        match (path1, path2) {
            (Some(p1), Some(p2)) => {
                assert_eq!(p1.len(), p2.len(), "deterministic paths should match");
                for (a, b) in p1.iter().zip(p2.iter()) {
                    assert_eq!(a.x, b.x);
                    assert_eq!(a.y, b.y);
                }
            }
            (None, None) => {}
            _ => panic!("determinism broken: one path exists, the other doesn't"),
        }
    }

    #[test]
    fn max_stroke_length_scaling() {
        let layer = make_layer();
        let field = DirectionField::new(&layer.guides, 512);
        let mut medians = Vec::new();
        for &max_len in &[120.0_f32, 240.0] {
            let params = StrokeParams {
                angle_variation: 0.0,
                max_stroke_length: max_len,
                ..StrokeParams::default()
            };

            let mut lengths = Vec::new();
            let mut rng = SeededRng::new(55);
            for _ in 0..300 {
                if let Some(path) = trace_streamline(
                    Vec2::new(0.1, 0.5),
                    &field,
                    &params,
                    512,
                    &mut rng,
                ) {
                    let len: f32 = path
                        .windows(2)
                        .map(|w| (w[1] - w[0]).length())
                        .sum();
                    lengths.push(len);
                }
            }
            lengths.sort_by(|a, b| a.partial_cmp(b).unwrap());
            medians.push(lengths[lengths.len() / 2]);
        }

        let ratio = medians[1] / medians[0];
        assert!(
            ratio > 1.5 && ratio < 2.5,
            "doubling max_stroke_length should roughly double median: ratio={ratio:.2} (medians: {:.4}, {:.4})",
            medians[0],
            medians[1]
        );
    }
}

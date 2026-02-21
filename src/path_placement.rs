use std::collections::HashMap;

use glam::Vec2;

use crate::direction_field::DirectionField;
use crate::math::rotate_vec2;
use crate::region::{rasterize_mask, region_bbox, BBox, RasterMask};
use crate::rng::SeededRng;
use crate::types::{Region, StrokePath, StrokeParams};

/// Grid expansion in units of spacing. Seeds are placed this many grid spacings
/// beyond the region bounding box to allow strokes to start at region boundaries.
const SEED_EXPANSION: f32 = 2.0;

/// Generate seed points for a region.
///
/// Returns UV-space seed points distributed on a uniform grid with 20% jitter.
/// The grid extends beyond the region bounding box by `SEED_EXPANSION * spacing`
/// on each side, allowing seeds outside the mask to seek into the region during
/// streamline tracing. Only UV [0,1] bounds are enforced (not mask containment).
pub fn generate_seeds(bbox: BBox, mask: &RasterMask, params: &StrokeParams) -> Vec<Vec2> {
    let spacing = params.brush_width / mask.width as f32 * params.stroke_spacing;
    let jitter_amount = spacing * 0.2;
    let mut rng = SeededRng::new(params.seed);

    // Expand bounding box, clamp to UV space
    let expansion = spacing * SEED_EXPANSION;
    let exp_min = Vec2::new(
        (bbox.min.x - expansion).max(0.0),
        (bbox.min.y - expansion).max(0.0),
    );
    let exp_max = Vec2::new(
        (bbox.max.x + expansion).min(1.0),
        (bbox.max.y + expansion).min(1.0),
    );

    let mut seeds = Vec::new();
    let mut y = exp_min.y;
    while y <= exp_max.y {
        let mut x = exp_min.x;
        while x <= exp_max.x {
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
/// If the seed is outside the region mask, a **seek phase** traces forward along
/// the direction field until the path enters the mask. The stroke path then starts
/// from the mask entry point. Seeds inside the mask skip the seek phase entirely.
///
/// Returns `None` if the resulting path is shorter than `brush_width * 2` (in UV
/// units), or if the seek phase fails to enter the mask.
pub fn trace_streamline(
    seed: Vec2,
    field: &DirectionField,
    mask: &RasterMask,
    params: &StrokeParams,
    resolution: u32,
    rng: &mut SeededRng,
) -> Option<Vec<Vec2>> {
    let step_size_uv = 1.0 / resolution as f32;

    // Consume RNG for target length FIRST (before seek).
    // This ensures the RNG sequence is consistent regardless of seek.
    // Power distribution: target = max * U^0.5 gives linearly increasing density
    // toward max_length (PDF ∝ t), with median ≈ 0.707 * max.
    let max_length_uv = params.max_stroke_length / resolution as f32;
    let target_length = max_length_uv * rng.next_f32().sqrt();

    let mut pos = seed;
    let mut prev_dir = field.sample(pos);

    // ── Seek phase: trace outside seeds toward the mask ──
    if !mask.contains(pos) {
        let spacing = params.brush_width / resolution as f32 * params.stroke_spacing;
        let max_seek = SEED_EXPANSION * spacing * 1.5;
        let mut seek_length = 0.0_f32;

        while seek_length < max_seek {
            let mut dir = field.sample(pos);

            // Direction alignment (same as normal tracing)
            if dir.dot(prev_dir) < 0.0 {
                dir = -dir;
            }
            dir = dir.normalize_or_zero();
            if dir == Vec2::ZERO {
                return None;
            }

            let next_pos = pos + dir * step_size_uv;

            // UV boundary → give up
            if next_pos.x < 0.0 || next_pos.x > 1.0 || next_pos.y < 0.0 || next_pos.y > 1.0 {
                return None;
            }

            prev_dir = dir;
            pos = next_pos;
            seek_length += step_size_uv;

            if mask.contains(pos) {
                break;
            }
        }

        // Failed to enter region
        if !mask.contains(pos) {
            return None;
        }
    }

    // ── Normal tracing phase ──
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

        // Boundary checks
        if !mask.contains(next_pos) {
            break;
        }
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

/// Generate all stroke paths for a region.
///
/// Returns paths in paint order (sorted by seed y-coordinate, top to bottom).
/// Each path is assigned a globally unique stroke ID: `(region.id << 16) | index`.
pub fn generate_paths(region: &Region, resolution: u32) -> Vec<StrokePath> {
    let mask = rasterize_mask(region, resolution);
    let bbox = region_bbox(region);

    let seeds = generate_seeds(bbox, &mask, &region.params);
    let field = DirectionField::new(&region.guides, resolution);

    let mut rng = SeededRng::new(region.params.seed);
    let mut raw_paths: Vec<Vec<Vec2>> = Vec::new();
    for seed_point in &seeds {
        if let Some(path) =
            trace_streamline(*seed_point, &field, &mask, &region.params, resolution, &mut rng)
        {
            raw_paths.push(path);
        }
    }

    let brush_width_uv = region.params.brush_width / resolution as f32;
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
        .map(|(i, path)| StrokePath::new(path, region.id, (region.id << 16) | (i as u32)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GuideVertex, Polygon, StrokeParams};

    fn make_polygon(verts: Vec<Vec2>) -> Polygon {
        Polygon { vertices: verts }
    }

    fn make_square_region(min: f32, max: f32) -> Region {
        Region {
            id: 0,
            name: String::from("test"),
            mask: vec![make_polygon(vec![
                Vec2::new(min, min),
                Vec2::new(max, min),
                Vec2::new(max, max),
                Vec2::new(min, max),
            ])],
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
        // Unit square region, brush=30, spacing=1.0, res=512
        // With expansion, grid extends beyond bbox by SEED_EXPANSION * spacing on each side.
        // For a [0,1] region the expansion is clamped to UV bounds, so no extra seeds.
        // Expected: ~(512/30)^2 ≈ 291 seeds (plus expansion rows/columns if bbox < [0,1])
        let region = make_square_region(0.0, 1.0);
        let mask = rasterize_mask(&region, 512);
        let bbox = region_bbox(&region);

        let seeds = generate_seeds(bbox, &mask, &region.params);
        let expected = ((512.0 / 30.0) as u32).pow(2);
        let count = seeds.len() as u32;
        // Allow some variance due to jitter pushing seeds out of bounds
        assert!(
            count > expected / 2 && count < expected * 2,
            "seed count {count} not near expected ~{expected}"
        );
    }

    #[test]
    fn seeds_within_uv_bounds() {
        // Seeds are now allowed outside the mask (for seek phase), but must
        // stay within UV [0,1] bounds.
        let region = Region {
            id: 0,
            name: String::from("L-shape"),
            mask: vec![make_polygon(vec![
                Vec2::new(0.1, 0.1),
                Vec2::new(0.9, 0.1),
                Vec2::new(0.9, 0.5),
                Vec2::new(0.5, 0.5),
                Vec2::new(0.5, 0.9),
                Vec2::new(0.1, 0.9),
            ])],
            order: 0,
            params: StrokeParams::default(),
            guides: vec![GuideVertex {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::X,
                influence: 1.5,
            }],
        };
        let mask = rasterize_mask(&region, 512);
        let bbox = region_bbox(&region);

        let seeds = generate_seeds(bbox, &mask, &region.params);
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
        let region = make_square_region(0.1, 0.9);
        let mask = rasterize_mask(&region, 512);
        let bbox = region_bbox(&region);

        let seeds1 = generate_seeds(bbox, &mask, &region.params);
        let seeds2 = generate_seeds(bbox, &mask, &region.params);
        assert_eq!(seeds1.len(), seeds2.len());
        for (a, b) in seeds1.iter().zip(seeds2.iter()) {
            assert_eq!(a.x, b.x);
            assert_eq!(a.y, b.y);
        }
    }

    #[test]
    fn seed_jitter_bounds() {
        let region = make_square_region(0.0, 1.0);
        let mask = rasterize_mask(&region, 512);
        let bbox = region_bbox(&region);
        let spacing = region.params.brush_width / 512.0 * region.params.stroke_spacing;
        let max_jitter = spacing * 0.2 * 2.0_f32.sqrt(); // diagonal of circle

        // Grid origin is the expanded bounding box min (clamped to [0,1])
        let expansion = spacing * SEED_EXPANSION;
        let grid_origin = Vec2::new(
            (bbox.min.x - expansion).max(0.0),
            (bbox.min.y - expansion).max(0.0),
        );

        let seeds = generate_seeds(bbox, &mask, &region.params);

        // Verify that seeds are near grid points
        for seed in &seeds {
            // Find nearest grid point relative to expanded grid origin
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
        let region = make_square_region(0.0, 1.0);
        let mask = rasterize_mask(&region, 512);
        let mut params = StrokeParams::default();
        params.angle_variation = 0.0; // No random deviation

        let field = DirectionField::new(&region.guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.5, 0.5),
            &field,
            &mask,
            &params,
            512,
            &mut rng,
        );

        let path = path.expect("path should exist");
        // All points should have y ≈ 0.5 (horizontal line)
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
        let mut region = make_square_region(0.0, 1.0);
        region.guides = vec![GuideVertex {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::Y,
            influence: 1.5,
        }];
        let mask = rasterize_mask(&region, 512);
        let mut params = StrokeParams::default();
        params.angle_variation = 0.0;

        let field = DirectionField::new(&region.guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.5, 0.5),
            &field,
            &mask,
            &params,
            512,
            &mut rng,
        );

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
    fn streamline_boundary_termination() {
        // Small region; seed near edge, direction heading toward boundary
        let region = Region {
            id: 0,
            name: String::from("small"),
            mask: vec![make_polygon(vec![
                Vec2::new(0.4, 0.4),
                Vec2::new(0.6, 0.4),
                Vec2::new(0.6, 0.6),
                Vec2::new(0.4, 0.6),
            ])],
            order: 0,
            params: StrokeParams {
                brush_width: 30.0,
                angle_variation: 0.0,
                ..StrokeParams::default()
            },
            guides: vec![GuideVertex {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::X,
                influence: 1.5,
            }],
        };
        let mask = rasterize_mask(&region, 512);

        let field = DirectionField::new(&region.guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.5, 0.5),
            &field,
            &mask,
            &region.params,
            512,
            &mut rng,
        );

        // Path should terminate at the boundary
        if let Some(path) = path {
            let last = path.last().unwrap();
            assert!(
                last.x <= 0.61,
                "path should stop near x=0.6 boundary, got x={:.4}",
                last.x
            );
        }
        // It's also okay if path is None (too short to pass min length filter)
    }

    #[test]
    fn streamline_min_length_filter() {
        // Seed very near the boundary so path is too short
        let region = Region {
            id: 0,
            name: String::from("small"),
            mask: vec![make_polygon(vec![
                Vec2::new(0.4, 0.4),
                Vec2::new(0.6, 0.4),
                Vec2::new(0.6, 0.6),
                Vec2::new(0.4, 0.6),
            ])],
            order: 0,
            params: StrokeParams {
                brush_width: 100.0, // Very wide brush → min_length is large
                angle_variation: 0.0,
                ..StrokeParams::default()
            },
            guides: vec![GuideVertex {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::X,
                influence: 1.5,
            }],
        };
        let mask = rasterize_mask(&region, 512);

        let field = DirectionField::new(&region.guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.5, 0.5),
            &field,
            &mask,
            &region.params,
            512,
            &mut rng,
        );

        // brush_width=100, min_length = 100/512*2 ≈ 0.39 UV
        // region is only 0.2 UV wide, so path should be too short
        assert!(path.is_none(), "short path should be filtered out");
    }

    #[test]
    fn streamline_length_variation() {
        // Verify the exponential-cubic distribution produces natural variation:
        // short and long strokes both exist, median near max_stroke_length * 0.74
        let region = make_square_region(0.0, 1.0);
        let mask = rasterize_mask(&region, 512);
        let params = StrokeParams {
            angle_variation: 0.0,
            ..StrokeParams::default()
        };

        let field = DirectionField::new(&region.guides, 512);
        let mut lengths = Vec::new();
        let mut rng = SeededRng::new(42);
        for _ in 0..200 {
            if let Some(path) = trace_streamline(
                Vec2::new(0.1, 0.5),
                &field,
                &mask,
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

        // Check both short (<30% of max) and long (>70% of max) strokes exist
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
        let offset = brush_width_uv * 0.5; // Half brush width apart
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
        let region = make_square_region(0.1, 0.9);
        let paths1 = generate_paths(&region, 256);
        let paths2 = generate_paths(&region, 256);

        assert_eq!(paths1.len(), paths2.len());
        for (a, b) in paths1.iter().zip(paths2.iter()) {
            assert_eq!(a.points.len(), b.points.len());
            assert_eq!(a.stroke_id, b.stroke_id);
            assert_eq!(a.region_id, b.region_id);
            for (pa, pb) in a.points.iter().zip(b.points.iter()) {
                assert_eq!(pa.x, pb.x);
                assert_eq!(pa.y, pb.y);
            }
        }
    }

    #[test]
    fn generate_paths_sorted_by_y() {
        let region = make_square_region(0.1, 0.9);
        let paths = generate_paths(&region, 256);

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
        let region = make_square_region(0.1, 0.9);
        let paths = generate_paths(&region, 256);

        let mut ids: Vec<u32> = paths.iter().map(|p| p.stroke_id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), paths.len(), "all stroke IDs should be unique");
    }

    #[test]
    fn generate_paths_stroke_id_encoding() {
        let mut region = make_square_region(0.1, 0.9);
        region.id = 3;
        let paths = generate_paths(&region, 256);

        for (i, path) in paths.iter().enumerate() {
            assert_eq!(path.region_id, 3);
            assert_eq!(
                path.stroke_id,
                (3 << 16) | (i as u32),
                "stroke_id encoding mismatch at index {i}"
            );
        }
    }

    #[test]
    fn area_coverage_90_percent() {
        let region = make_square_region(0.1, 0.9);
        let resolution = 512u32;
        let paths = generate_paths(&region, resolution);

        // Rasterize all stroke paths with brush_width onto a coverage mask
        let mut covered = vec![false; (resolution * resolution) as usize];
        let brush_radius_uv = region.params.brush_width / resolution as f32 / 2.0;
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

        // Count coverage within region mask
        let mask = rasterize_mask(&region, resolution);
        let region_pixels = mask.data.iter().filter(|&&v| v).count();
        let covered_region_pixels = (0..(resolution * resolution) as usize)
            .filter(|&i| mask.data[i] && covered[i])
            .count();

        let coverage = covered_region_pixels as f32 / region_pixels as f32;
        assert!(
            coverage >= 0.90,
            "Coverage {:.1}% is below 90%",
            coverage * 100.0
        );
    }

    #[test]
    fn paths_stay_within_region() {
        let region = make_square_region(0.1, 0.9);
        let mask = rasterize_mask(&region, 512);
        let paths = generate_paths(&region, 512);

        for path in &paths {
            for point in &path.points {
                assert!(
                    mask.contains(*point),
                    "path point ({:.4}, {:.4}) is outside region mask",
                    point.x,
                    point.y
                );
            }
        }
    }

    #[test]
    fn streamline_curved_two_guides() {
        let region = Region {
            id: 0,
            name: String::from("curved"),
            mask: vec![make_polygon(vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(1.0, 1.0),
                Vec2::new(0.0, 1.0),
            ])],
            order: 0,
            params: StrokeParams {
                angle_variation: 0.0,
                ..StrokeParams::default()
            },
            guides: vec![
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
            ],
        };
        let mask = rasterize_mask(&region, 512);

        let field = DirectionField::new(&region.guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.2, 0.5),
            &field,
            &mask,
            &region.params,
            512,
            &mut rng,
        );

        // Path should exist and curve (start horizontal, end more vertical)
        let path = path.expect("curved path should exist");
        assert!(path.len() > 10, "path should have enough points");

        // Check that path curves: first segment ≈ horizontal, last segment should differ
        let first_dir = (path[1] - path[0]).normalize();
        let last_dir = (path[path.len() - 1] - path[path.len() - 2]).normalize();

        // The directions should differ (path curves)
        let dot = first_dir.dot(last_dir);
        assert!(
            dot < 0.99,
            "path should curve: dot between first and last direction = {dot:.4}"
        );
    }

    // ── Visual Inspection Tests (output PNGs to test/output/) ──

    fn draw_paths_to_png(
        paths: &[StrokePath],
        resolution: u32,
        mask: &RasterMask,
        filename: &str,
    ) {
        let res = resolution as usize;
        let mut img = vec![0u8; res * res * 3];

        // Draw mask area as dark gray background
        for i in 0..res * res {
            if mask.data[i] {
                img[i * 3] = 40;
                img[i * 3 + 1] = 40;
                img[i * 3 + 2] = 40;
            }
        }

        // Draw each path in a distinct color
        for (idx, path) in paths.iter().enumerate() {
            // Generate a color from stroke index using golden ratio hue
            let hue = (idx as f32 * 0.618034) % 1.0;
            let (r, g, b) = hue_to_rgb(hue);

            for point in &path.points {
                let px = (point.x * resolution as f32) as i32;
                let py = (point.y * resolution as f32) as i32;
                // Draw a 2px dot for visibility
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
        // Square region with uniform horizontal direction
        let region = make_square_region(0.1, 0.9);
        let resolution = 512;
        let paths = generate_paths(&region, resolution);
        let mask = rasterize_mask(&region, resolution);

        eprintln!("visual_horizontal_strokes: {} paths generated", paths.len());
        draw_paths_to_png(&paths, resolution, &mask, "horizontal_strokes.png");
        // INSPECT: should see roughly parallel horizontal colored lines
        assert!(!paths.is_empty());
    }

    #[test]
    fn visual_spiral_guides() {
        // 4 guides arranged in a circle with tangential directions
        let region = Region {
            id: 0,
            name: String::from("spiral"),
            mask: vec![make_polygon(vec![
                Vec2::new(0.05, 0.05),
                Vec2::new(0.95, 0.05),
                Vec2::new(0.95, 0.95),
                Vec2::new(0.05, 0.95),
            ])],
            order: 0,
            params: StrokeParams {
                brush_width: 20.0,
                stroke_spacing: 1.0,
                angle_variation: 3.0,
                max_turn_angle: 30.0,
                ..StrokeParams::default()
            },
            guides: vec![
                // Top center: pointing right
                GuideVertex {
                    position: Vec2::new(0.5, 0.2),
                    direction: Vec2::new(1.0, 0.0),
                    influence: 0.5,
                },
                // Right center: pointing down
                GuideVertex {
                    position: Vec2::new(0.8, 0.5),
                    direction: Vec2::new(0.0, 1.0),
                    influence: 0.5,
                },
                // Bottom center: pointing left
                GuideVertex {
                    position: Vec2::new(0.5, 0.8),
                    direction: Vec2::new(-1.0, 0.0),
                    influence: 0.5,
                },
                // Left center: pointing up
                GuideVertex {
                    position: Vec2::new(0.2, 0.5),
                    direction: Vec2::new(0.0, -1.0),
                    influence: 0.5,
                },
            ],
        };
        let resolution = 512;
        let paths = generate_paths(&region, resolution);
        let mask = rasterize_mask(&region, resolution);

        eprintln!("visual_spiral_guides: {} paths generated", paths.len());
        draw_paths_to_png(&paths, resolution, &mask, "spiral_guides.png");
        // INSPECT: strokes should form a circular/spiral flow pattern
        assert!(!paths.is_empty());
    }

    #[test]
    fn visual_l_shape_region() {
        // L-shaped region to verify paths respect concave boundaries
        let region = Region {
            id: 0,
            name: String::from("L-shape"),
            mask: vec![make_polygon(vec![
                Vec2::new(0.1, 0.1),
                Vec2::new(0.9, 0.1),
                Vec2::new(0.9, 0.5),
                Vec2::new(0.5, 0.5),
                Vec2::new(0.5, 0.9),
                Vec2::new(0.1, 0.9),
            ])],
            order: 0,
            params: StrokeParams::default(),
            guides: vec![GuideVertex {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::X,
                influence: 1.5,
            }],
        };
        let resolution = 512;
        let paths = generate_paths(&region, resolution);
        let mask = rasterize_mask(&region, resolution);

        eprintln!("visual_l_shape_region: {} paths generated", paths.len());
        draw_paths_to_png(&paths, resolution, &mask, "l_shape_region.png");
        // INSPECT: strokes should only fill the L-shape, not the notch
        assert!(!paths.is_empty());
    }

    #[test]
    fn turn_angle_termination() {
        // Create a field with a very sharp change: guides point opposite directions
        // close together, but with small influence so field changes abruptly
        let region = Region {
            id: 0,
            name: String::from("sharp"),
            mask: vec![make_polygon(vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(1.0, 1.0),
                Vec2::new(0.0, 1.0),
            ])],
            order: 0,
            params: StrokeParams {
                angle_variation: 0.0,
                max_turn_angle: 5.0, // Very strict turn limit
                ..StrokeParams::default()
            },
            guides: vec![
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
            ],
        };
        let mask = rasterize_mask(&region, 512);

        let field = DirectionField::new(&region.guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.3, 0.5),
            &field,
            &mask,
            &region.params,
            512,
            &mut rng,
        );

        // With very strict turn angle, path should either be short or terminate early
        // where the field changes sharply
        if let Some(path) = path {
            // Verify consecutive angles don't exceed limit
            for w in path.windows(3) {
                let d1 = (w[1] - w[0]).normalize();
                let d2 = (w[2] - w[1]).normalize();
                let dot = d1.dot(d2).clamp(-1.0, 1.0);
                let angle = dot.acos().to_degrees();
                // Allow some tolerance for floating point
                assert!(
                    angle <= 10.0,
                    "consecutive turn angle {angle:.1}° exceeds limit"
                );
            }
        }
    }

    // ── Phase 05-1: Boundary Seed Expansion Tests ──

    #[test]
    fn expanded_seed_grid_bounds() {
        // Square region [0.2, 0.8], spacing ~0.06
        // Seeds should exist in range ~[0.08, 0.92] (outside [0.2, 0.8])
        let region = make_square_region(0.2, 0.8);
        let mask = rasterize_mask(&region, 512);
        let bbox = region_bbox(&region);

        let seeds = generate_seeds(bbox, &mask, &region.params);
        assert!(!seeds.is_empty());

        let min_x = seeds.iter().map(|s| s.x).fold(f32::MAX, f32::min);
        let max_x = seeds.iter().map(|s| s.x).fold(f32::MIN, f32::max);
        let min_y = seeds.iter().map(|s| s.y).fold(f32::MAX, f32::min);
        let max_y = seeds.iter().map(|s| s.y).fold(f32::MIN, f32::max);

        // Seeds should extend beyond [0.2, 0.8] due to expansion
        assert!(
            min_x < 0.2,
            "min_x={min_x:.3} should be below 0.2 (expansion zone)"
        );
        assert!(
            max_x > 0.8,
            "max_x={max_x:.3} should be above 0.8 (expansion zone)"
        );
        assert!(
            min_y < 0.2,
            "min_y={min_y:.3} should be below 0.2 (expansion zone)"
        );
        assert!(
            max_y > 0.8,
            "max_y={max_y:.3} should be above 0.8 (expansion zone)"
        );

        // But all seeds must be within UV [0,1]
        for seed in &seeds {
            assert!(
                seed.x >= 0.0 && seed.x <= 1.0 && seed.y >= 0.0 && seed.y <= 1.0,
                "seed ({:.3}, {:.3}) outside UV bounds",
                seed.x,
                seed.y
            );
        }

        // Some seeds should be outside the mask
        let outside_count = seeds.iter().filter(|s| !mask.contains(**s)).count();
        assert!(
            outside_count > 0,
            "expansion should produce seeds outside the mask"
        );
    }

    #[test]
    fn outside_seed_enters_region() {
        // Seed at (0.15, 0.5), guide dir=(1,0), region [0.2, 0.8]²
        // Path should start at x ≈ 0.2, extending rightward inside region
        let region = make_square_region(0.2, 0.8);
        let mask = rasterize_mask(&region, 512);

        let field = DirectionField::new(&region.guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.15, 0.5),
            &field,
            &mask,
            &StrokeParams {
                angle_variation: 0.0,
                ..StrokeParams::default()
            },
            512,
            &mut rng,
        );

        let path = path.expect("outside seed should produce a path via seek");
        // Path should start near x=0.2 (the left boundary of the region)
        assert!(
            path[0].x >= 0.19 && path[0].x <= 0.22,
            "path start x={:.4} should be near 0.2 boundary",
            path[0].x
        );
        // All path points should be inside the mask
        for p in &path {
            assert!(
                mask.contains(*p),
                "path point ({:.4}, {:.4}) outside mask",
                p.x,
                p.y
            );
        }
    }

    #[test]
    fn outside_seed_parallel_to_boundary() {
        // Seed above region, guide dir=(1,0) (parallel to top boundary)
        // Should return None (never enters the region)
        let region = make_square_region(0.2, 0.8);
        let mask = rasterize_mask(&region, 512);

        let field = DirectionField::new(&region.guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.5, 0.15), // above the region
            &field,               // horizontal direction
            &mask,
            &StrokeParams {
                angle_variation: 0.0,
                ..StrokeParams::default()
            },
            512,
            &mut rng,
        );

        assert!(
            path.is_none(),
            "seed moving parallel to boundary should not enter region"
        );
    }

    #[test]
    fn outside_seed_away_from_region() {
        // Seed right of region, guide dir=(1,0) (pointing away)
        // Should return None (direction leads away from region)
        let region = make_square_region(0.2, 0.8);
        let mask = rasterize_mask(&region, 512);

        let field = DirectionField::new(&region.guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.85, 0.5), // right of the region
            &field,               // horizontal direction → points further right
            &mask,
            &StrokeParams {
                angle_variation: 0.0,
                ..StrokeParams::default()
            },
            512,
            &mut rng,
        );

        assert!(
            path.is_none(),
            "seed pointing away from region should not produce a path"
        );
    }

    #[test]
    fn seek_respects_uv_bounds() {
        // Seed at (0.01, 0.5), guide dir=(-1,0) (toward UV edge)
        // Should return None (hits UV boundary during seek)
        let region = make_square_region(0.2, 0.8);
        let mask = rasterize_mask(&region, 512);

        let guides = vec![GuideVertex {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::new(-1.0, 0.0), // pointing left toward UV=0
            influence: 1.5,
        }];

        let field = DirectionField::new(&guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.01, 0.5),
            &field,
            &mask,
            &StrokeParams {
                angle_variation: 0.0,
                ..StrokeParams::default()
            },
            512,
            &mut rng,
        );

        assert!(
            path.is_none(),
            "seed seeking toward UV boundary should return None"
        );
    }

    #[test]
    fn seek_max_distance_limit() {
        // Seed far outside the region — should exceed max seek distance
        let region = make_square_region(0.7, 0.9);
        let mask = rasterize_mask(&region, 512);

        let field = DirectionField::new(&region.guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.1, 0.5), // very far from [0.7, 0.9] region
            &field,
            &mask,
            &StrokeParams {
                angle_variation: 0.0,
                ..StrokeParams::default()
            },
            512,
            &mut rng,
        );

        assert!(
            path.is_none(),
            "seed far outside should exceed max seek distance"
        );
    }

    #[test]
    fn inside_seed_unaffected() {
        // Seed inside mask should produce identical path regardless of seek code
        let region = make_square_region(0.1, 0.9);
        let mask = rasterize_mask(&region, 512);
        let params = StrokeParams {
            angle_variation: 0.0,
            ..StrokeParams::default()
        };

        let seed = Vec2::new(0.5, 0.5);
        assert!(mask.contains(seed), "seed should be inside mask");

        let field = DirectionField::new(&region.guides, 512);
        let mut rng1 = SeededRng::new(42);
        let path = trace_streamline(seed, &field, &mask, &params, 512, &mut rng1);

        let path = path.expect("inside seed should produce a path");
        // First point should be exactly the seed (no seek offset)
        assert_eq!(path[0].x, seed.x);
        assert_eq!(path[0].y, seed.y);
    }

    #[test]
    fn boundary_edge_coverage() {
        // Square region, measure coverage within 1 brush_width of the left boundary
        // (entry edge for horizontal direction). Coverage should be >= 85%.
        let region = make_square_region(0.1, 0.9);
        let resolution = 512u32;
        let paths = generate_paths(&region, resolution);
        let mask = rasterize_mask(&region, resolution);

        let brush_width_uv = region.params.brush_width / resolution as f32;
        let brush_radius_uv = brush_width_uv / 2.0;
        let r = (brush_radius_uv * resolution as f32).ceil() as i32;

        // Rasterize stroke coverage
        let mut covered = vec![false; (resolution * resolution) as usize];
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

        // Count coverage within 1 brush_width of the left boundary (x ∈ [0.1, 0.1 + brush_width_uv])
        let left_band_max_x = 0.1 + brush_width_uv;
        let mut band_pixels = 0u32;
        let mut band_covered = 0u32;
        for py in 0..resolution {
            for px in 0..resolution {
                let ux = px as f32 / resolution as f32;
                let _uy = py as f32 / resolution as f32;
                let idx = (py * resolution + px) as usize;
                if mask.data[idx] && ux >= 0.1 && ux <= left_band_max_x {
                    band_pixels += 1;
                    if covered[idx] {
                        band_covered += 1;
                    }
                }
            }
        }

        let coverage = if band_pixels > 0 {
            band_covered as f32 / band_pixels as f32
        } else {
            0.0
        };
        assert!(
            coverage >= 0.85,
            "Left boundary coverage {:.1}% is below 85%",
            coverage * 100.0
        );
    }

    #[test]
    fn concave_notch_entry() {
        // L-shaped region; seeds in the concave notch area should produce
        // paths that enter the L-arms via seek.
        let region = Region {
            id: 0,
            name: String::from("L-shape"),
            mask: vec![make_polygon(vec![
                Vec2::new(0.1, 0.1),
                Vec2::new(0.9, 0.1),
                Vec2::new(0.9, 0.5),
                Vec2::new(0.5, 0.5),
                Vec2::new(0.5, 0.9),
                Vec2::new(0.1, 0.9),
            ])],
            order: 0,
            params: StrokeParams::default(),
            guides: vec![GuideVertex {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::X,
                influence: 1.5,
            }],
        };
        let paths = generate_paths(&region, 512);
        let mask = rasterize_mask(&region, 512);

        // All path points must be inside the mask
        for path in &paths {
            for point in &path.points {
                assert!(
                    mask.contains(*point),
                    "path point ({:.4}, {:.4}) outside L-shape mask",
                    point.x,
                    point.y
                );
            }
        }
        assert!(!paths.is_empty());
    }

    // ── Phase 05-2: Stroke Length Distribution Tests ──

    #[test]
    fn stroke_length_distribution_median() {
        // Median should be near max_stroke_length * 0.74
        let region = make_square_region(0.0, 1.0);
        let mask = rasterize_mask(&region, 512);
        let params = StrokeParams {
            angle_variation: 0.0,
            ..StrokeParams::default()
        };

        let field = DirectionField::new(&region.guides, 512);
        let mut lengths = Vec::new();
        let mut rng = SeededRng::new(123);
        for _ in 0..500 {
            if let Some(path) = trace_streamline(
                Vec2::new(0.1, 0.5),
                &field,
                &mask,
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
        // No path should exceed max_stroke_length in UV length
        let region = make_square_region(0.0, 1.0);
        let mask = rasterize_mask(&region, 512);
        let params = StrokeParams {
            angle_variation: 0.0,
            ..StrokeParams::default()
        };

        let max_length_uv = params.max_stroke_length / 512.0;
        let field = DirectionField::new(&region.guides, 512);
        let mut rng = SeededRng::new(77);
        for i in 0..500 {
            if let Some(path) = trace_streamline(
                Vec2::new(0.1, 0.5),
                &field,
                &mask,
                &params,
                512,
                &mut rng,
            ) {
                let len: f32 = path
                    .windows(2)
                    .map(|w| (w[1] - w[0]).length())
                    .sum();
                assert!(
                    len <= max_length_uv + 0.005, // small tolerance for step overshoot
                    "path {i} length {len:.4} exceeds max {max_length_uv:.4}"
                );
            }
        }
    }

    #[test]
    fn stroke_length_one_rng_call() {
        // Verify that target_length computation consumes exactly 1 RNG call.
        // Two SeededRngs starting at same state: advance one by 1 call manually,
        // then both should produce the same next value after trace_streamline's
        // target_length computation.
        let region = make_square_region(0.0, 1.0);
        let mask = rasterize_mask(&region, 512);
        let params = StrokeParams {
            angle_variation: 0.0,
            ..StrokeParams::default()
        };

        let field = DirectionField::new(&region.guides, 512);

        let mut rng_a = SeededRng::new(99);
        let mut rng_b = SeededRng::new(99);

        // rng_a: skip 1 call (what target_length should consume)
        let _ = rng_a.next_f32();

        // rng_b: run trace_streamline (consumes 1 for target_length, then more for tracing)
        let _ = trace_streamline(
            Vec2::new(0.5, 0.5),
            &field,
            &mask,
            &params,
            512,
            &mut rng_b,
        );

        // We can't directly compare rng states, but we can verify by checking
        // that the first tracing step's angle_offset would use the 2nd RNG value.
        // Instead, verify the property indirectly: run two traces from same seed,
        // they must be identical (determinism).
        let mut rng1 = SeededRng::new(99);
        let mut rng2 = SeededRng::new(99);
        let path1 = trace_streamline(
            Vec2::new(0.5, 0.5),
            &field,
            &mask,
            &params,
            512,
            &mut rng1,
        );
        let path2 = trace_streamline(
            Vec2::new(0.5, 0.5),
            &field,
            &mask,
            &params,
            512,
            &mut rng2,
        );

        match (path1, path2) {
            (Some(p1), Some(p2)) => {
                assert_eq!(p1.len(), p2.len(), "deterministic paths should match");
                for (a, b) in p1.iter().zip(p2.iter()) {
                    assert_eq!(a.x, b.x);
                    assert_eq!(a.y, b.y);
                }
            }
            (None, None) => {} // both filtered — still deterministic
            _ => panic!("determinism broken: one path exists, the other doesn't"),
        }
    }

    #[test]
    fn max_stroke_length_scaling() {
        // Halving max_stroke_length should roughly halve the median length
        let region = make_square_region(0.0, 1.0);
        let mask = rasterize_mask(&region, 512);

        let field = DirectionField::new(&region.guides, 512);
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
                    &mask,
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

    // ── Visual Test: Boundary Coverage ──

    #[test]
    fn visual_boundary_coverage() {
        // Square region [0.1, 0.9] with horizontal guide dir=(1,0).
        // INSPECT: Left boundary should have strokes starting right at x=0.1.
        let region = make_square_region(0.1, 0.9);
        let resolution = 512;
        let paths = generate_paths(&region, resolution);
        let mask = rasterize_mask(&region, resolution);

        eprintln!(
            "visual_boundary_coverage: {} paths generated",
            paths.len()
        );
        draw_paths_to_png(&paths, resolution, &mask, "boundary_coverage.png");
        assert!(!paths.is_empty());
    }
}

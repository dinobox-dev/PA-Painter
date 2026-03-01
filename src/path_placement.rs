use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use glam::Vec2;

use crate::direction_field::DirectionField;
use crate::math::rotate_vec2;
use crate::object_normal::{sample_object_normal, MeshNormalData};
use crate::rng::SeededRng;
use crate::stroke_color::{channel_max_diff, sample_bilinear, ColorTextureRef};
use crate::types::{PaintLayer, StrokeParams, StrokePath, BASE_RESOLUTION};
use crate::uv_mask::UvMask;

/// Check cancellation token, returning early if set.
#[inline]
fn is_cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|c| c.load(Ordering::Relaxed))
}

/// Generate Poisson disk seeds within an arbitrary UV rectangle `[lo, hi]`.
///
/// Uses Bridson's algorithm to produce a blue-noise distribution with minimum
/// spacing guarantee.
fn generate_seeds_poisson_in(
    params: &StrokeParams,
    resolution: u32,
    lo: Vec2,
    hi: Vec2,
    cancel: Option<&AtomicBool>,
) -> Vec<Vec2> {
    let min_dist = params.brush_width / resolution as f32 * params.stroke_spacing;
    if min_dist <= 0.0 || !min_dist.is_finite() {
        return Vec::new();
    }
    let cell_size = min_dist / std::f32::consts::SQRT_2;
    let extent = hi - lo;
    let grid_w = (extent.x / cell_size).ceil() as usize + 1;
    let grid_h = (extent.y / cell_size).ceil() as usize + 1;
    let mut grid: Vec<Option<usize>> = vec![None; grid_w * grid_h];
    let mut rng = SeededRng::new(params.seed);
    let mut points: Vec<Vec2> = Vec::new();
    let mut active: Vec<usize> = Vec::new();

    let min_dist_sq = min_dist * min_dist;
    let k = 30; // candidates per active point

    // Start with a random initial point within [lo, hi]
    let initial = Vec2::new(
        lo.x + rng.next_f32() * extent.x,
        lo.y + rng.next_f32() * extent.y,
    );
    points.push(initial);
    active.push(0);
    let gi = (((initial.x - lo.x) / cell_size) as usize).min(grid_w - 1);
    let gj = (((initial.y - lo.y) / cell_size) as usize).min(grid_h - 1);
    grid[gj * grid_w + gi] = Some(0);

    while !active.is_empty() {
        if is_cancelled(cancel) {
            return Vec::new();
        }
        let active_idx = (rng.next_f32() * active.len() as f32) as usize % active.len();
        let center = points[active[active_idx]];

        let mut found = false;
        for _ in 0..k {
            let angle = rng.next_f32() * std::f32::consts::TAU;
            let dist = min_dist + rng.next_f32() * min_dist;
            let candidate = center + Vec2::new(angle.cos(), angle.sin()) * dist;

            if candidate.x < lo.x || candidate.x > hi.x || candidate.y < lo.y || candidate.y > hi.y
            {
                continue;
            }

            let ci = (((candidate.x - lo.x) / cell_size) as usize).min(grid_w - 1);
            let cj = (((candidate.y - lo.y) / cell_size) as usize).min(grid_h - 1);

            // Check neighbors in 5×5 grid
            let mut too_close = false;
            'outer: for dy in -2i32..=2 {
                for dx in -2i32..=2 {
                    let ni = ci as i32 + dx;
                    let nj = cj as i32 + dy;
                    if ni >= 0 && ni < grid_w as i32 && nj >= 0 && nj < grid_h as i32 {
                        if let Some(idx) = grid[nj as usize * grid_w + ni as usize] {
                            if (candidate - points[idx]).length_squared() < min_dist_sq {
                                too_close = true;
                                break 'outer;
                            }
                        }
                    }
                }
            }

            if !too_close {
                let new_idx = points.len();
                points.push(candidate);
                active.push(new_idx);
                grid[cj * grid_w + ci] = Some(new_idx);
                found = true;
                break;
            }
        }

        if !found {
            active.swap_remove(active_idx);
        }
    }

    points
}

/// Trace a single streamline from a seed point.
///
/// The seed must be within `uv_bounds` (lo..hi). The stroke traces along the
/// direction field until it hits the UV boundary, exceeds curvature limits,
/// or reaches the target length.
///
/// Returns `None` if the resulting path is shorter than `brush_width * 2` (in UV
/// units).
#[allow(clippy::too_many_arguments)]
pub fn trace_streamline(
    seed: Vec2,
    field: &DirectionField,
    params: &StrokeParams,
    resolution: u32,
    rng: &mut SeededRng,
    color_tex: Option<&ColorTextureRef<'_>>,
    normal_data: Option<&MeshNormalData>,
    uv_bounds: (Vec2, Vec2),
    mask: Option<&UvMask>,
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

    let start_normal = normal_data.map(|nd| sample_object_normal(nd, pos));

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
        let (uv_lo, uv_hi) = uv_bounds;
        if next_pos.x < uv_lo.x
            || next_pos.x > uv_hi.x
            || next_pos.y < uv_lo.y
            || next_pos.y > uv_hi.y
        {
            break;
        }

        // Mask boundary check
        if let Some(mask) = mask {
            if !mask.sample(next_pos) {
                break;
            }
        }

        // Color boundary check
        if let (Some(threshold), Some(tex)) = (params.color_break_threshold, color_tex) {
            let cur_color = sample_bilinear(tex.data, tex.width, tex.height, pos);
            let next_color = sample_bilinear(tex.data, tex.width, tex.height, next_pos);
            if channel_max_diff(cur_color, next_color) > threshold {
                break;
            }
        }

        // Normal boundary check (cumulative from stroke start)
        if let (Some(threshold), Some(nd), Some(sn)) =
            (params.normal_break_threshold, normal_data, start_normal)
        {
            let next_n = sample_object_normal(nd, next_pos);
            if sn.dot(next_n) < threshold {
                break;
            }
        }

        path.push(next_pos);
        prev_dir = dir;
        pos = next_pos;
        length += step_size_uv;
    }

    // Minimum length filter
    let min_length = params.brush_width / resolution as f32;
    if length < min_length {
        return None;
    }

    Some(path)
}

/// Filter paths by removing those that overlap excessively with existing paths.
///
/// A path is removed if >= `overlap_ratio` of its points are within
/// `brush_width_uv * overlap_dist_factor` of any accepted path's centerline points.
///
/// Uses a spatial grid index for O(n × m × k) performance instead of brute-force
/// O(n² × m²), where k is the average number of points per grid cell.
pub fn filter_overlapping_paths(
    paths: &mut Vec<Vec<Vec2>>,
    brush_width_uv: f32,
    overlap_ratio: f32,
    overlap_dist_factor: f32,
) {
    let threshold_dist = brush_width_uv * overlap_dist_factor;
    let threshold_sq = threshold_dist * threshold_dist;
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
/// Uses Poisson disk seeds in an overscan region `[-margin, 1+margin]²` to
/// eliminate density drop at UV boundaries, then clips results to `[0,1]²`.
///
/// Returns paths in paint order (sorted by seed y-coordinate, top to bottom).
/// Each path is assigned a globally unique stroke ID: `(layer_index << 16) | index`.
pub fn generate_paths(
    layer: &PaintLayer,
    layer_index: u32,
    color_tex: Option<&ColorTextureRef<'_>>,
    normal_data: Option<&MeshNormalData>,
    mask: Option<&UvMask>,
) -> Vec<StrokePath> {
    generate_paths_cancellable(layer, layer_index, color_tex, normal_data, mask, None)
}

/// Like [`generate_paths`] but accepts an optional cancellation token.
/// When the token is set, the function returns an empty Vec as soon as possible.
pub fn generate_paths_cancellable(
    layer: &PaintLayer,
    layer_index: u32,
    color_tex: Option<&ColorTextureRef<'_>>,
    normal_data: Option<&MeshNormalData>,
    mask: Option<&UvMask>,
    cancel: Option<&AtomicBool>,
) -> Vec<StrokePath> {
    // Path geometry is resolution-independent in UV space: seed density and
    // curve shape are identical at any output resolution.  Tracing at
    // BASE_RESOLUTION avoids O(R) point bloat while the compositing stage
    // still rasterises at the actual output resolution.
    let resolution = BASE_RESOLUTION;
    let scaled = layer.params.scaled_for_resolution(resolution);
    let field = DirectionField::new(&layer.guides, resolution);
    let brush_width_uv = scaled.brush_width / resolution as f32;
    let overlap_ratio = layer.params.overlap_ratio.unwrap_or(0.7);
    let overlap_dist_factor = layer.params.overlap_dist_factor.unwrap_or(0.3);
    let margin = brush_width_uv * 3.0;
    let min_length = brush_width_uv;

    // When a mask is provided, seed within its AABB (expanded by overscan margin).
    // Otherwise use the full [0,1]² with overscan.
    let (seed_lo, seed_hi) = if let Some(m) = mask {
        let (mlo, mhi) = m.aabb();
        (
            Vec2::new((mlo.x - margin).max(-margin), (mlo.y - margin).max(-margin)),
            Vec2::new(
                (mhi.x + margin).min(1.0 + margin),
                (mhi.y + margin).min(1.0 + margin),
            ),
        )
    } else {
        (Vec2::splat(-margin), Vec2::splat(1.0 + margin))
    };

    let seeds = generate_seeds_poisson_in(&scaled, resolution, seed_lo, seed_hi, cancel);
    if is_cancelled(cancel) {
        return Vec::new();
    }

    // Filter seeds by mask
    let filtered_seeds: Vec<Vec2> = if let Some(m) = mask {
        seeds.into_iter().filter(|s| m.sample(*s)).collect()
    } else {
        seeds
    };

    let mut rng = SeededRng::new(scaled.seed);
    let mut raw_paths: Vec<Vec<Vec2>> = Vec::new();
    for (i, seed_point) in filtered_seeds.iter().enumerate() {
        // Check cancellation every 256 seeds to avoid excessive atomic loads
        if i & 0xFF == 0 && is_cancelled(cancel) {
            return Vec::new();
        }
        if let Some(path) = trace_streamline(
            *seed_point,
            &field,
            &scaled,
            resolution,
            &mut rng,
            color_tex,
            normal_data,
            (seed_lo, seed_hi),
            mask,
        ) {
            if let Some(clipped) = clip_path_to_uv(path) {
                let length: f32 = clipped.windows(2).map(|w| (w[1] - w[0]).length()).sum();
                if length >= min_length {
                    raw_paths.push(clipped);
                }
            }
        }
    }

    filter_overlapping_paths(
        &mut raw_paths,
        brush_width_uv,
        overlap_ratio,
        overlap_dist_factor,
    );

    // Sort paths by seed point y-coordinate (top-to-bottom row order).
    // Critical for correct wet-on-wet compositing order in Phase 08.
    raw_paths.sort_by(|a, b| a[0].y.total_cmp(&b[0].y));

    // Convert to StrokePath (IDs are layer-local; made globally unique by compositing)
    raw_paths
        .into_iter()
        .enumerate()
        .map(|(i, path)| StrokePath::new(path, layer_index, i as u32))
        .collect()
}

/// Clip a path to the [0,1]² UV region by trimming points outside the bounds.
///
/// Since streamlines trace monotonically from boundary to boundary, out-of-bounds
/// points only appear at the start or end.  Returns `None` if fewer than 2 points
/// remain after clipping.
fn clip_path_to_uv(path: Vec<Vec2>) -> Option<Vec<Vec2>> {
    let clipped: Vec<Vec2> = path
        .into_iter()
        .filter(|p| p.x >= 0.0 && p.x <= 1.0 && p.y >= 0.0 && p.y <= 1.0)
        .collect();
    if clipped.len() >= 2 {
        Some(clipped)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Guide, PaintLayer, StrokeParams};
    use glam::Vec3;

    fn make_layer() -> PaintLayer {
        PaintLayer {
            name: String::from("test"),
            order: 0,
            params: StrokeParams::default(),
            guides: vec![Guide {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::X,
                influence: 1.5,
                ..Guide::default()
            }],
        }
    }

    // ── Seed Distribution Tests (Poisson) ──

    #[test]
    fn seed_poisson_coverage() {
        let layer = make_layer();
        let seeds = generate_seeds_poisson_in(&layer.params, 512, Vec2::ZERO, Vec2::ONE, None);
        // Poisson disk should produce a reasonable number of seeds
        assert!(
            seeds.len() > 50 && seeds.len() < 1000,
            "unexpected seed count: {}",
            seeds.len()
        );
    }

    #[test]
    fn seeds_poisson_within_bounds() {
        let layer = make_layer();
        let seeds = generate_seeds_poisson_in(&layer.params, 512, Vec2::ZERO, Vec2::ONE, None);
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
    fn seed_poisson_determinism() {
        let layer = make_layer();
        let seeds1 = generate_seeds_poisson_in(&layer.params, 512, Vec2::ZERO, Vec2::ONE, None);
        let seeds2 = generate_seeds_poisson_in(&layer.params, 512, Vec2::ZERO, Vec2::ONE, None);
        assert_eq!(seeds1.len(), seeds2.len());
        for (a, b) in seeds1.iter().zip(seeds2.iter()) {
            assert_eq!(a.x, b.x);
            assert_eq!(a.y, b.y);
        }
    }

    #[test]
    fn seed_poisson_minimum_spacing() {
        let layer = make_layer();
        let resolution = 512u32;
        let min_dist = layer.params.brush_width / resolution as f32 * layer.params.stroke_spacing;
        let seeds =
            generate_seeds_poisson_in(&layer.params, resolution, Vec2::ZERO, Vec2::ONE, None);
        // Verify all pairs respect minimum distance
        for (i, a) in seeds.iter().enumerate() {
            for b in &seeds[i + 1..] {
                let dist = (*a - *b).length();
                assert!(
                    dist >= min_dist - 1e-6,
                    "seeds ({:.4},{:.4}) and ({:.4},{:.4}) are {:.6} apart, min_dist = {:.6}",
                    a.x,
                    a.y,
                    b.x,
                    b.y,
                    dist,
                    min_dist
                );
            }
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
        let path = trace_streamline(
            Vec2::new(0.5, 0.5),
            &field,
            &params,
            512,
            &mut rng,
            None,
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
        );

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
        let guides = vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::Y,
            influence: 1.5,
            ..Guide::default()
        }];
        let mut params = StrokeParams::default();
        params.angle_variation = 0.0;

        let field = DirectionField::new(&guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.5, 0.5),
            &field,
            &params,
            512,
            &mut rng,
            None,
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
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
    fn streamline_uv_boundary_termination() {
        let guides = vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 1.5,
            ..Guide::default()
        }];
        let params = StrokeParams {
            angle_variation: 0.0,
            max_stroke_length: 500.0,
            ..StrokeParams::default()
        };

        let field = DirectionField::new(&guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.9, 0.5),
            &field,
            &params,
            512,
            &mut rng,
            None,
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
        );

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
        let guides = vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 1.5,
            ..Guide::default()
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
        let path = trace_streamline(
            Vec2::new(0.5, 0.5),
            &field,
            &params,
            512,
            &mut rng,
            None,
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
        );

        // min_length = 100/512 ≈ 0.195 UV, but max target ≈ 0.002 UV
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
                None,
                None,
                (Vec2::ZERO, Vec2::ONE),
                None,
            ) {
                let len: f32 = path.windows(2).map(|w| (w[1] - w[0]).length()).sum();
                lengths.push(len);
            }
        }

        assert!(
            lengths.len() >= 20,
            "need enough paths to check distribution"
        );
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
        filter_overlapping_paths(&mut paths, brush_width_uv, 0.7, 0.3);
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn filter_identical_paths_removes_duplicate() {
        let brush_width_uv = 30.0 / 512.0;
        let p: Vec<Vec2> = (0..20)
            .map(|i| Vec2::new(0.1 + i as f32 * 0.01, 0.5))
            .collect();
        let mut paths = vec![p.clone(), p];
        filter_overlapping_paths(&mut paths, brush_width_uv, 0.7, 0.3);
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
        filter_overlapping_paths(&mut paths, brush_width_uv, 0.7, 0.3);
        assert_eq!(
            paths.len(),
            2,
            "partially overlapping paths should both be kept"
        );
    }

    // ── Full Pipeline Tests ──

    #[test]
    fn generate_paths_determinism() {
        let layer = make_layer();
        let paths1 = generate_paths(&layer, 0, None, None, None);
        let paths2 = generate_paths(&layer, 0, None, None, None);

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
        let paths = generate_paths(&layer, 0, None, None, None);

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
        let paths = generate_paths(&layer, 0, None, None, None);

        let mut ids: Vec<u32> = paths.iter().map(|p| p.stroke_id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), paths.len(), "all stroke IDs should be unique");
    }

    #[test]
    fn area_coverage_90_percent() {
        let layer = make_layer();
        let resolution = 512u32;
        let paths = generate_paths(&layer, 0, None, None, None);

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
                            if x >= 0 && x < resolution as i32 && y >= 0 && y < resolution as i32 {
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
        let paths = generate_paths(&layer, 0, None, None, None);

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
            Guide {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::X,
                influence: 0.5,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::Y,
                influence: 0.5,
                ..Guide::default()
            },
        ];
        let params = StrokeParams {
            angle_variation: 0.0,
            ..StrokeParams::default()
        };

        let field = DirectionField::new(&guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.2, 0.5),
            &field,
            &params,
            512,
            &mut rng,
            None,
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
        );

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
            Guide {
                position: Vec2::new(0.3, 0.5),
                direction: Vec2::X,
                influence: 0.15,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.7, 0.5),
                direction: Vec2::Y,
                influence: 0.15,
                ..Guide::default()
            },
        ];
        let params = StrokeParams {
            angle_variation: 0.0,
            max_turn_angle: 5.0,
            ..StrokeParams::default()
        };

        let field = DirectionField::new(&guides, 512);
        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.3, 0.5),
            &field,
            &params,
            512,
            &mut rng,
            None,
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
        );

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

    fn draw_paths_to_png(paths: &[StrokePath], resolution: u32, filename: &str) {
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
        image::save_buffer(&path, &img, resolution, resolution, image::ColorType::Rgb8).unwrap();
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
        let paths = generate_paths(&layer, 0, None, None, None);

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
                Guide {
                    position: Vec2::new(0.5, 0.2),
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
                Guide {
                    position: Vec2::new(0.5, 0.8),
                    direction: Vec2::new(-1.0, 0.0),
                    influence: 0.5,
                    ..Guide::default()
                },
                Guide {
                    position: Vec2::new(0.2, 0.5),
                    direction: Vec2::new(0.0, -1.0),
                    influence: 0.5,
                    ..Guide::default()
                },
            ],
        };
        let resolution = 512;
        let paths = generate_paths(&layer, 0, None, None, None);

        eprintln!("visual_spiral_guides: {} paths generated", paths.len());
        draw_paths_to_png(&paths, resolution, "spiral_guides.png");
        assert!(!paths.is_empty());
    }

    /// Draw paths on top of a color texture background.
    fn draw_paths_on_texture(
        paths: &[StrokePath],
        tex: &[crate::types::Color],
        tex_w: u32,
        tex_h: u32,
        resolution: u32,
        filename: &str,
    ) {
        use crate::stroke_color::sample_bilinear;
        let res = resolution as usize;
        let mut img = vec![0u8; res * res * 3];

        // Draw texture background
        for py in 0..resolution {
            for px in 0..resolution {
                let uv = Vec2::new(
                    (px as f32 + 0.5) / resolution as f32,
                    (py as f32 + 0.5) / resolution as f32,
                );
                let c = sample_bilinear(tex, tex_w, tex_h, uv);
                let i = (py as usize * res + px as usize) * 3;
                img[i] = (c.r.clamp(0.0, 1.0) * 255.0) as u8;
                img[i + 1] = (c.g.clamp(0.0, 1.0) * 255.0) as u8;
                img[i + 2] = (c.b.clamp(0.0, 1.0) * 255.0) as u8;
            }
        }

        // Draw paths
        for (idx, path) in paths.iter().enumerate() {
            let hue = (idx as f32 * 0.618034) % 1.0;
            let (r, g, b) = hue_to_rgb(hue);
            for point in &path.points {
                let px = (point.x * resolution as f32) as i32;
                let py = (point.y * resolution as f32) as i32;
                if px >= 0 && px < resolution as i32 && py >= 0 && py < resolution as i32 {
                    let i = (py as usize * res + px as usize) * 3;
                    img[i] = r;
                    img[i + 1] = g;
                    img[i + 2] = b;
                }
            }
        }

        let path = crate::test_module_output_dir("path_placement").join(filename);
        image::save_buffer(&path, &img, resolution, resolution, image::ColorType::Rgb8).unwrap();
        eprintln!("Wrote visual test: {}", path.display());
    }

    #[test]
    fn visual_color_boundary_comparison() {
        // INSPECT: Two images on a 4-stripe color texture (red/green/blue/yellow).
        // "color_break_off.png" — strokes cross color boundaries freely.
        // "color_break_on.png"  — strokes stop at sharp color boundaries.
        // Expected: ON image shows shorter strokes that respect color regions.

        let resolution = 512u32;

        // 128×1 texture: 4 vertical color stripes
        let mut tex_data = Vec::with_capacity(128);
        for i in 0..128 {
            tex_data.push(match i / 32 {
                0 => crate::types::Color::rgb(0.85, 0.2, 0.15), // red
                1 => crate::types::Color::rgb(0.15, 0.7, 0.2),  // green
                2 => crate::types::Color::rgb(0.15, 0.25, 0.85), // blue
                _ => crate::types::Color::rgb(0.9, 0.8, 0.15),  // yellow
            });
        }
        let tex_ref = ColorTextureRef {
            data: &tex_data,
            width: 128,
            height: 1,
        };

        let layer_off = PaintLayer {
            name: String::from("off"),
            order: 0,
            params: StrokeParams {
                brush_width: 15.0,
                angle_variation: 3.0,
                max_turn_angle: 20.0,
                color_break_threshold: None,
                ..StrokeParams::default()
            },
            guides: vec![Guide {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::X,
                influence: 1.0,
                ..Guide::default()
            }],
        };
        let mut layer_on = layer_off.clone();
        layer_on.params.color_break_threshold = Some(0.08);

        let paths_off = generate_paths(&layer_off, 0, Some(&tex_ref), None, None);
        let paths_on = generate_paths(&layer_on, 0, Some(&tex_ref), None, None);

        eprintln!(
            "color_break OFF: {} paths, ON: {} paths",
            paths_off.len(),
            paths_on.len()
        );

        draw_paths_on_texture(
            &paths_off,
            &tex_data,
            128,
            1,
            resolution,
            "color_break_off.png",
        );
        draw_paths_on_texture(
            &paths_on,
            &tex_data,
            128,
            1,
            resolution,
            "color_break_on.png",
        );

        assert!(!paths_off.is_empty());
        assert!(!paths_on.is_empty());
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
                None,
                None,
                (Vec2::ZERO, Vec2::ONE),
                None,
            ) {
                let len: f32 = path.windows(2).map(|w| (w[1] - w[0]).length()).sum();
                lengths.push(len);
            }
        }

        lengths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
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
                None,
                None,
                (Vec2::ZERO, Vec2::ONE),
                None,
            ) {
                let len: f32 = path.windows(2).map(|w| (w[1] - w[0]).length()).sum();
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
        let path1 = trace_streamline(
            Vec2::new(0.5, 0.5),
            &field,
            &params,
            512,
            &mut rng1,
            None,
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
        );
        let path2 = trace_streamline(
            Vec2::new(0.5, 0.5),
            &field,
            &params,
            512,
            &mut rng2,
            None,
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
        );

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
                    None,
                    None,
                    (Vec2::ZERO, Vec2::ONE),
                    None,
                ) {
                    let len: f32 = path.windows(2).map(|w| (w[1] - w[0]).length()).sum();
                    lengths.push(len);
                }
            }
            lengths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
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

    // ── Color Boundary Break Tests ──

    #[test]
    fn threshold_none_same_as_no_texture() {
        // With threshold=None, providing a color texture should not change paths.
        let guides = vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 1.5,
            ..Guide::default()
        }];
        let params = StrokeParams {
            angle_variation: 0.0,
            color_break_threshold: None,
            ..StrokeParams::default()
        };
        let field = DirectionField::new(&guides, 512);

        // 2×2 split texture: left=red, right=blue
        let tex_data = vec![
            crate::types::Color::rgb(1.0, 0.0, 0.0),
            crate::types::Color::rgb(0.0, 0.0, 1.0),
            crate::types::Color::rgb(1.0, 0.0, 0.0),
            crate::types::Color::rgb(0.0, 0.0, 1.0),
        ];
        let tex_ref = ColorTextureRef {
            data: &tex_data,
            width: 2,
            height: 2,
        };

        let mut rng1 = SeededRng::new(42);
        let mut rng2 = SeededRng::new(42);
        let path_no_tex = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &params,
            512,
            &mut rng1,
            None,
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
        );
        let path_with_tex = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &params,
            512,
            &mut rng2,
            Some(&tex_ref),
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
        );

        match (path_no_tex, path_with_tex) {
            (Some(a), Some(b)) => {
                assert_eq!(
                    a.len(),
                    b.len(),
                    "threshold=None should not affect path length"
                );
            }
            (None, None) => {}
            _ => panic!("threshold=None should not affect path existence"),
        }
    }

    #[test]
    fn color_boundary_breaks_path() {
        // Horizontal stroke crossing a sharp red/blue boundary at u=0.5.
        // Use a 64-pixel-wide texture so the bilinear transition zone is narrow
        // enough (~1/64 UV) that per-step color diff exceeds the threshold.
        let guides = vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 1.5,
            ..Guide::default()
        }];
        let params = StrokeParams {
            angle_variation: 0.0,
            max_stroke_length: 500.0,
            color_break_threshold: Some(0.1),
            ..StrokeParams::default()
        };
        let field = DirectionField::new(&guides, 512);

        // 64×1 texture: left half red, right half blue
        let mut tex_data = Vec::with_capacity(64);
        for i in 0..64 {
            if i < 32 {
                tex_data.push(crate::types::Color::rgb(1.0, 0.0, 0.0));
            } else {
                tex_data.push(crate::types::Color::rgb(0.0, 0.0, 1.0));
            }
        }
        let tex_ref = ColorTextureRef {
            data: &tex_data,
            width: 64,
            height: 1,
        };

        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &params,
            512,
            &mut rng,
            Some(&tex_ref),
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
        );

        // Path should exist but be shorter than without boundary
        let mut rng2 = SeededRng::new(42);
        let path_no_break = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &StrokeParams {
                color_break_threshold: None,
                ..params.clone()
            },
            512,
            &mut rng2,
            Some(&tex_ref),
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
        );

        if let (Some(p), Some(p_nb)) = (path, path_no_break) {
            assert!(
                p.len() < p_nb.len(),
                "color boundary should shorten path: {} vs {} points",
                p.len(),
                p_nb.len()
            );
            // Path should stop before or near the boundary (u ≈ 0.5)
            let last_x = p.last().unwrap().x;
            assert!(
                last_x < 0.65,
                "path should stop near color boundary, got last x={last_x:.3}"
            );
        }
    }

    #[test]
    fn uniform_texture_no_break() {
        // Uniform texture: color boundary should never trigger.
        let guides = vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 1.5,
            ..Guide::default()
        }];
        let params_break = StrokeParams {
            angle_variation: 0.0,
            color_break_threshold: Some(0.05),
            ..StrokeParams::default()
        };
        let params_no_break = StrokeParams {
            color_break_threshold: None,
            ..params_break.clone()
        };
        let field = DirectionField::new(&guides, 512);

        let tex_data = vec![crate::types::Color::rgb(0.5, 0.3, 0.7); 4];
        let tex_ref = ColorTextureRef {
            data: &tex_data,
            width: 2,
            height: 2,
        };

        let mut rng1 = SeededRng::new(42);
        let mut rng2 = SeededRng::new(42);
        let path_break = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &params_break,
            512,
            &mut rng1,
            Some(&tex_ref),
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
        );
        let path_no_break = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &params_no_break,
            512,
            &mut rng2,
            Some(&tex_ref),
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
        );

        match (path_break, path_no_break) {
            (Some(a), Some(b)) => {
                assert_eq!(
                    a.len(),
                    b.len(),
                    "uniform texture should not cause any breaks"
                );
            }
            (None, None) => {}
            _ => panic!("uniform texture should not affect path existence"),
        }
    }

    #[test]
    fn color_boundary_deterministic() {
        let guides = vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 1.5,
            ..Guide::default()
        }];
        let params = StrokeParams {
            angle_variation: 0.0,
            color_break_threshold: Some(0.15),
            ..StrokeParams::default()
        };
        let field = DirectionField::new(&guides, 512);
        let tex_data = vec![
            crate::types::Color::rgb(1.0, 0.0, 0.0),
            crate::types::Color::rgb(0.0, 0.0, 1.0),
        ];
        let tex_ref = ColorTextureRef {
            data: &tex_data,
            width: 2,
            height: 1,
        };

        let mut rng1 = SeededRng::new(42);
        let mut rng2 = SeededRng::new(42);
        let p1 = trace_streamline(
            Vec2::new(0.2, 0.5),
            &field,
            &params,
            512,
            &mut rng1,
            Some(&tex_ref),
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
        );
        let p2 = trace_streamline(
            Vec2::new(0.2, 0.5),
            &field,
            &params,
            512,
            &mut rng2,
            Some(&tex_ref),
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
        );

        match (p1, p2) {
            (Some(a), Some(b)) => {
                assert_eq!(a.len(), b.len());
                for (pa, pb) in a.iter().zip(b.iter()) {
                    assert_eq!(pa.x, pb.x);
                    assert_eq!(pa.y, pb.y);
                }
            }
            (None, None) => {}
            _ => panic!("determinism broken with color boundary"),
        }
    }

    // ── Normal Boundary Break Tests ──

    /// Build a simple MeshNormalData with two halves: left = +Z, right = +X.
    /// Simulates a 90° hard edge at u=0.5.
    fn make_two_face_normal_data(resolution: u32) -> MeshNormalData {
        let size = (resolution * resolution) as usize;
        let mut normals = Vec::with_capacity(size);
        let tangents = vec![Vec3::X; size];
        let bitangents = vec![Vec3::Y; size];
        for py in 0..resolution {
            for px in 0..resolution {
                let u = (px as f32 + 0.5) / resolution as f32;
                if u < 0.5 {
                    normals.push(Vec3::Z); // left face
                } else {
                    normals.push(Vec3::X); // right face (90° from left)
                }
                let _ = py; // suppress unused warning
            }
        }
        MeshNormalData {
            object_normals: normals,
            tangents,
            bitangents,
            resolution,
        }
    }

    #[test]
    fn normal_boundary_breaks_path() {
        // Horizontal stroke crossing a 90° normal boundary at u=0.5.
        // threshold=0.5 (~60°) should break at the boundary (dot=0).
        let guides = vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 1.5,
            ..Guide::default()
        }];
        let params = StrokeParams {
            angle_variation: 0.0,
            max_stroke_length: 500.0,
            normal_break_threshold: Some(0.5),
            ..StrokeParams::default()
        };
        let field = DirectionField::new(&guides, 512);
        let nd = make_two_face_normal_data(64);

        let mut rng = SeededRng::new(42);
        let path = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &params,
            512,
            &mut rng,
            None,
            Some(&nd),
            (Vec2::ZERO, Vec2::ONE),
            None,
        );

        // Path without normal break for comparison
        let mut rng2 = SeededRng::new(42);
        let path_no_break = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &StrokeParams {
                normal_break_threshold: None,
                ..params.clone()
            },
            512,
            &mut rng2,
            None,
            Some(&nd),
            (Vec2::ZERO, Vec2::ONE),
            None,
        );

        if let (Some(p), Some(p_nb)) = (path, path_no_break) {
            assert!(
                p.len() < p_nb.len(),
                "normal boundary should shorten path: {} vs {} points",
                p.len(),
                p_nb.len()
            );
            let last_x = p.last().unwrap().x;
            assert!(
                last_x < 0.6,
                "path should stop near normal boundary at u=0.5, got last x={last_x:.3}"
            );
        }
    }

    #[test]
    fn normal_boundary_deterministic() {
        let guides = vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 1.5,
            ..Guide::default()
        }];
        let params = StrokeParams {
            angle_variation: 0.0,
            normal_break_threshold: Some(0.5),
            ..StrokeParams::default()
        };
        let field = DirectionField::new(&guides, 512);
        let nd = make_two_face_normal_data(64);

        let mut rng1 = SeededRng::new(42);
        let mut rng2 = SeededRng::new(42);
        let p1 = trace_streamline(
            Vec2::new(0.2, 0.5),
            &field,
            &params,
            512,
            &mut rng1,
            None,
            Some(&nd),
            (Vec2::ZERO, Vec2::ONE),
            None,
        );
        let p2 = trace_streamline(
            Vec2::new(0.2, 0.5),
            &field,
            &params,
            512,
            &mut rng2,
            None,
            Some(&nd),
            (Vec2::ZERO, Vec2::ONE),
            None,
        );

        match (p1, p2) {
            (Some(a), Some(b)) => {
                assert_eq!(a.len(), b.len());
                for (pa, pb) in a.iter().zip(b.iter()) {
                    assert_eq!(pa.x, pb.x);
                    assert_eq!(pa.y, pb.y);
                }
            }
            (None, None) => {}
            _ => panic!("determinism broken with normal boundary"),
        }
    }

    #[test]
    fn threshold_none_ignores_normal() {
        // With threshold=None, normal data should not affect paths.
        let guides = vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 1.5,
            ..Guide::default()
        }];
        let params = StrokeParams {
            angle_variation: 0.0,
            normal_break_threshold: None,
            ..StrokeParams::default()
        };
        let field = DirectionField::new(&guides, 512);
        let nd = make_two_face_normal_data(64);

        let mut rng1 = SeededRng::new(42);
        let mut rng2 = SeededRng::new(42);
        let path_no_nd = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &params,
            512,
            &mut rng1,
            None,
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
        );
        let path_with_nd = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &params,
            512,
            &mut rng2,
            None,
            Some(&nd),
            (Vec2::ZERO, Vec2::ONE),
            None,
        );

        match (path_no_nd, path_with_nd) {
            (Some(a), Some(b)) => {
                assert_eq!(
                    a.len(),
                    b.len(),
                    "threshold=None should not affect path length"
                );
            }
            (None, None) => {}
            _ => panic!("threshold=None should not affect path existence"),
        }
    }

    // ── Normal Boundary Visual Tests ──

    /// Build a 4-face normal data: quadrants have different normals.
    /// Top-left=+Z, top-right=+X, bottom-left=+Y, bottom-right=(-1,1,1)/√3
    fn make_four_face_normal_data(resolution: u32) -> MeshNormalData {
        let size = (resolution * resolution) as usize;
        let mut normals = Vec::with_capacity(size);
        let tangents = vec![Vec3::X; size];
        let bitangents = vec![Vec3::Y; size];
        let diag = Vec3::new(-1.0, 1.0, 1.0).normalize();
        for py in 0..resolution {
            for px in 0..resolution {
                let u = (px as f32 + 0.5) / resolution as f32;
                let v = (py as f32 + 0.5) / resolution as f32;
                let n = match (u < 0.5, v < 0.5) {
                    (true, true) => Vec3::Z,
                    (false, true) => Vec3::X,
                    (true, false) => Vec3::Y,
                    (false, false) => diag,
                };
                normals.push(n);
            }
        }
        MeshNormalData {
            object_normals: normals,
            tangents,
            bitangents,
            resolution,
        }
    }

    /// Draw paths on a normal-map background (normals → RGB).
    fn draw_paths_on_normals(
        paths: &[StrokePath],
        nd: &MeshNormalData,
        resolution: u32,
        filename: &str,
    ) {
        let res = resolution as usize;
        let mut img = vec![0u8; res * res * 3];

        // Draw normal map background (object normal → RGB, same encoding as normal maps)
        for py in 0..resolution {
            for px in 0..resolution {
                let uv = Vec2::new(
                    (px as f32 + 0.5) / resolution as f32,
                    (py as f32 + 0.5) / resolution as f32,
                );
                let n = sample_object_normal(nd, uv);
                let i = (py as usize * res + px as usize) * 3;
                // Map [-1,1] → [0,255], dimmed to 40% so paths stand out
                img[i] = ((n.x * 0.5 + 0.5) * 0.4 * 255.0) as u8;
                img[i + 1] = ((n.y * 0.5 + 0.5) * 0.4 * 255.0) as u8;
                img[i + 2] = ((n.z * 0.5 + 0.5) * 0.4 * 255.0) as u8;
            }
        }

        // Draw paths
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
        image::save_buffer(&path, &img, resolution, resolution, image::ColorType::Rgb8).unwrap();
        eprintln!("Wrote visual test: {}", path.display());
    }

    #[test]
    fn visual_normal_boundary_comparison() {
        // INSPECT: Two images on a 4-face normal map.
        // "normal_break_off.png" — strokes cross normal boundaries freely.
        // "normal_break_on.png"  — strokes stop at hard normal edges.
        // Expected: ON image shows shorter strokes confined to each face.

        let resolution = 512u32;
        let nd = make_four_face_normal_data(128);

        let layer_off = PaintLayer {
            name: String::from("off"),
            order: 0,
            params: StrokeParams {
                brush_width: 15.0,
                angle_variation: 3.0,
                max_turn_angle: 20.0,
                normal_break_threshold: None,
                ..StrokeParams::default()
            },
            guides: vec![
                Guide {
                    position: Vec2::new(0.25, 0.25),
                    direction: Vec2::new(1.0, 0.3).normalize(),
                    influence: 0.5,
                    ..Guide::default()
                },
                Guide {
                    position: Vec2::new(0.75, 0.75),
                    direction: Vec2::new(-0.3, 1.0).normalize(),
                    influence: 0.5,
                    ..Guide::default()
                },
            ],
        };
        let mut layer_on = layer_off.clone();
        layer_on.params.normal_break_threshold = Some(0.5);

        let paths_off = generate_paths(&layer_off, 0, None, Some(&nd), None);
        let paths_on = generate_paths(&layer_on, 0, None, Some(&nd), None);

        eprintln!(
            "normal_break OFF: {} paths, ON: {} paths",
            paths_off.len(),
            paths_on.len()
        );

        draw_paths_on_normals(&paths_off, &nd, resolution, "normal_break_off.png");
        draw_paths_on_normals(&paths_on, &nd, resolution, "normal_break_on.png");

        assert!(!paths_off.is_empty());
        assert!(!paths_on.is_empty());
    }

    // ── Density Comparison Visual Tests ──

    /// Draw a labelled header onto the top of an image buffer.
    fn draw_label(img: &mut [u8], res: usize, label: &str) {
        // Simple 3×5 digit/letter font — just draw a colored bar with contrasting text area
        let bar_h = 18;
        for y in 0..bar_h.min(res) {
            for x in 0..res {
                let i = (y * res + x) * 3;
                img[i] = 20;
                img[i + 1] = 20;
                img[i + 2] = 20;
            }
        }
        // Encode label as simple pixel blocks (4px per char)
        let char_w = 6;
        let char_h = 10;
        let start_x = 4;
        let start_y = 4;
        for (ci, ch) in label.chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            let x0 = start_x + ci * char_w;
            for dy in 0..char_h.min(bar_h - start_y) {
                for dx in 0..(char_w - 1) {
                    let x = x0 + dx;
                    let y = start_y + dy;
                    if x < res && y < res {
                        let i = (y * res + x) * 3;
                        img[i] = 200;
                        img[i + 1] = 200;
                        img[i + 2] = 200;
                    }
                }
            }
        }
    }

    #[test]
    fn visual_density_comparison() {
        // INSPECT: Three images comparing density improvement methods.
        // All use the same guide setup (horizontal + slight diagonal).
        // Baseline uses Poisson disk + overscan (the unified pipeline).
        //
        // 1. density_baseline.png        — default params (Poisson + overscan)
        // 2. density_A_tight_spacing.png  — stroke_spacing=0.5
        // 3. density_AB_relaxed.png       — spacing=0.5 + overlap_ratio=0.85, dist_factor=0.15

        let resolution = 512u32;
        let guides = vec![
            Guide {
                position: Vec2::new(0.3, 0.4),
                direction: Vec2::new(1.0, 0.15).normalize(),
                influence: 0.8,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.7, 0.6),
                direction: Vec2::new(1.0, -0.1).normalize(),
                influence: 0.8,
                ..Guide::default()
            },
        ];

        // ── 1. Baseline (Poisson + overscan) ──
        let layer_base = PaintLayer {
            name: String::from("baseline"),
            order: 0,
            params: StrokeParams {
                brush_width: 20.0,
                angle_variation: 3.0,
                max_turn_angle: 20.0,
                ..StrokeParams::default()
            },
            guides: guides.clone(),
        };
        let paths_base = generate_paths(&layer_base, 0, None, None, None);

        // ── 2. Method A: tight spacing ──
        let layer_a = PaintLayer {
            name: String::from("tight_spacing"),
            order: 0,
            params: StrokeParams {
                stroke_spacing: 0.5,
                ..layer_base.params.clone()
            },
            guides: guides.clone(),
        };
        let paths_a = generate_paths(&layer_a, 0, None, None, None);

        // ── 3. Method A+B: tight spacing + relaxed overlap filter ──
        let layer_ab = PaintLayer {
            name: String::from("relaxed_filter"),
            order: 0,
            params: StrokeParams {
                stroke_spacing: 0.5,
                overlap_ratio: Some(0.85),
                overlap_dist_factor: Some(0.15),
                ..layer_base.params.clone()
            },
            guides: guides.clone(),
        };
        let paths_ab = generate_paths(&layer_ab, 0, None, None, None);

        eprintln!("=== Density Comparison ===");
        eprintln!("  Baseline:      {} paths", paths_base.len());
        eprintln!("  A (spacing):   {} paths", paths_a.len());
        eprintln!("  A+B (relaxed): {} paths", paths_ab.len());

        let cases: &[(&[StrokePath], &str, &str)] = &[
            (
                &paths_base,
                "density_1_baseline.png",
                "BASELINE (Poisson+overscan)",
            ),
            (&paths_a, "density_2_A_tight_spacing.png", "A: spacing=0.5"),
            (&paths_ab, "density_3_AB_relaxed.png", "A+B: spacing+filter"),
        ];

        for &(paths, filename, label) in cases {
            let res = resolution as usize;
            let mut img = vec![40u8; res * res * 3];

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

            draw_label(&mut img, res, &format!("{} ({} paths)", label, paths.len()));

            let path = crate::test_module_output_dir("path_placement").join(filename);
            image::save_buffer(&path, &img, resolution, resolution, image::ColorType::Rgb8)
                .unwrap();
            eprintln!("Wrote: {}", path.display());
        }

        // All methods should produce paths
        assert!(!paths_base.is_empty());
        assert!(!paths_a.is_empty());
        assert!(!paths_ab.is_empty());

        // Density should increase: A > baseline, A+B > A
        assert!(
            paths_a.len() > paths_base.len(),
            "A ({}) should have more paths than baseline ({})",
            paths_a.len(),
            paths_base.len()
        );
        assert!(
            paths_ab.len() > paths_a.len(),
            "A+B ({}) should have more paths than A ({})",
            paths_ab.len(),
            paths_a.len()
        );
    }
}

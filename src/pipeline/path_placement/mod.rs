//! **Pipeline stage 2** — stroke path placement along the direction field.
//!
//! Seeds initial points via Poisson-disk sampling, then traces streamlines through
//! the direction field. Supports density control, overscan, color/normal boundary
//! breaks, and multi-pass refinement.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use glam::Vec2;
use log::{debug, info, warn};

#[cfg(test)]
use crate::mesh::object_normal::sample_object_normal;
use crate::mesh::object_normal::MeshNormalData;
use crate::mesh::stretch_map::StretchMap;
use crate::mesh::uv_mask::DistanceField;
use crate::pipeline::direction_field::DirectionField;
use crate::types::{PaintLayer, StrokeParams, StrokePath, BASE_RESOLUTION};
use crate::util::math::rotate_vec2;
use crate::util::rng::SeededRng;
use crate::util::stroke_color::{channel_max_diff, sample_bilinear, ColorTextureRef};

/// Clamp range for stretch factors in path placement.
/// Prevents degenerate meshes from causing extreme over/under-sampling.
const STRETCH_CLAMP_MIN: f32 = 0.5;
const STRETCH_CLAMP_MAX: f32 = 2.0;

/// Optional context for path generation.
///
/// Groups mesh-dependent data and a cancellation token that would otherwise
/// bloat the `generate_paths` parameter list.  All fields default to `None`.
#[derive(Default)]
pub struct PathContext<'a> {
    pub color_tex: Option<&'a ColorTextureRef<'a>>,
    pub normal_data: Option<&'a MeshNormalData>,
    pub dist_field: Option<&'a DistanceField>,
    pub stretch_map: Option<&'a StretchMap>,
    pub cancel: Option<&'a AtomicBool>,
}

/// Check cancellation token, returning early if set.
#[inline]
fn is_cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|c| c.load(Ordering::Relaxed))
}

/// Generate Poisson disk seeds within an arbitrary UV rectangle `[lo, hi]`.
///
/// Uses Bridson's algorithm to produce a blue-noise distribution with minimum
/// spacing guarantee. When a `stretch_map` is provided, the minimum spacing
/// adapts per-point: compressed UV regions (high stretch) get denser seeding,
/// stretched UV regions (low stretch) get sparser seeding.
pub(crate) fn generate_seeds_poisson_in(
    params: &StrokeParams,
    resolution: u32,
    lo: Vec2,
    hi: Vec2,
    stretch_map: Option<&StretchMap>,
    cancel: Option<&AtomicBool>,
) -> Vec<Vec2> {
    let base_min_dist = params.brush_width / resolution as f32 * params.stroke_spacing;
    if base_min_dist <= 0.0 || !base_min_dist.is_finite() {
        return Vec::new();
    }

    // With stretch adaptation, the smallest local min_dist determines the grid
    // cell size. Without stretch, use the uniform base distance.
    let has_stretch = stretch_map.is_some();
    let min_local_dist = if has_stretch {
        base_min_dist / STRETCH_CLAMP_MAX
    } else {
        base_min_dist
    };
    let cell_size = min_local_dist / std::f32::consts::SQRT_2;
    let extent = hi - lo;
    let grid_w = (extent.x / cell_size).ceil() as usize + 1;
    let grid_h = (extent.y / cell_size).ceil() as usize + 1;
    let mut grid: Vec<Option<usize>> = vec![None; grid_w * grid_h];
    let mut rng = SeededRng::new(params.seed);
    let mut points: Vec<Vec2> = Vec::new();
    let mut active: Vec<usize> = Vec::new();

    // Neighbor check radius: must cover the largest possible local min_dist.
    // max_local_dist / cell_size = (base / STRETCH_MIN) / (base / STRETCH_MAX / sqrt2)
    //                            = STRETCH_MAX / STRETCH_MIN * sqrt2
    let check_radius = if has_stretch {
        ((STRETCH_CLAMP_MAX / STRETCH_CLAMP_MIN) * std::f32::consts::SQRT_2).ceil() as i32 + 1
    } else {
        2
    };

    let k = 30; // candidates per active point

    /// Look up clamped stretch at a UV point.
    #[inline]
    fn local_min_dist(pos: Vec2, base: f32, sm: Option<&StretchMap>) -> f32 {
        match sm {
            Some(sm) => base / sm.sample(pos).clamp(STRETCH_CLAMP_MIN, STRETCH_CLAMP_MAX),
            None => base,
        }
    }

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
        let active_idx = rng.next_usize_below(active.len());
        let center = points[active[active_idx]];
        let center_min = local_min_dist(center, base_min_dist, stretch_map);

        let mut found = false;
        for _ in 0..k {
            let angle = rng.next_f32() * std::f32::consts::TAU;
            let dist = center_min + rng.next_f32() * center_min;
            let candidate = center + Vec2::new(angle.cos(), angle.sin()) * dist;

            if candidate.x < lo.x || candidate.x > hi.x || candidate.y < lo.y || candidate.y > hi.y
            {
                continue;
            }

            let ci = (((candidate.x - lo.x) / cell_size) as usize).min(grid_w - 1);
            let cj = (((candidate.y - lo.y) / cell_size) as usize).min(grid_h - 1);

            let cand_min = local_min_dist(candidate, base_min_dist, stretch_map);
            let cand_min_sq = cand_min * cand_min;

            // Check neighbors
            let mut too_close = false;
            'outer: for dy in -check_radius..=check_radius {
                for dx in -check_radius..=check_radius {
                    let ni = ci as i32 + dx;
                    let nj = cj as i32 + dy;
                    if ni >= 0 && ni < grid_w as i32 && nj >= 0 && nj < grid_h as i32 {
                        if let Some(idx) = grid[nj as usize * grid_w + ni as usize] {
                            if (candidate - points[idx]).length_squared() < cand_min_sq {
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
    _normal_data: Option<&MeshNormalData>,
    uv_bounds: (Vec2, Vec2),
    dist_field: Option<&DistanceField>,
    dist_threshold: f32,
    stretch_map: Option<&StretchMap>,
) -> Option<Vec<Vec2>> {
    let base_step = 1.0 / resolution as f32;

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

        // Stretch-adaptive step: in compressed UV regions (high stretch),
        // take smaller UV steps so path point density stays uniform in 3D.
        let stretch = stretch_map
            .map(|sm| sm.sample(pos).clamp(STRETCH_CLAMP_MIN, STRETCH_CLAMP_MAX))
            .unwrap_or(1.0);
        let step_size_uv = base_step / stretch;

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

        // Mask boundary check (distance field)
        if let Some(df) = dist_field {
            if !df.sample(next_pos, dist_threshold) {
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

        // Normal boundary: no longer terminates the path here.
        // Per-pixel normal clamp in compositing handles face boundaries
        // without creating empty gaps at edges.

        path.push(next_pos);
        prev_dir = dir;
        pos = next_pos;
        // Accumulate 3D-equivalent length: UV step × stretch factor
        length += step_size_uv * stretch;
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
///
/// Optional mesh data and a cancellation token are passed via [`PathContext`].
/// When the cancellation token is set, the function returns an empty Vec as
/// soon as possible.
pub fn generate_paths(
    layer: &PaintLayer,
    layer_index: u32,
    ctx: &PathContext<'_>,
) -> Vec<StrokePath> {
    let color_tex = ctx.color_tex;
    let normal_data = ctx.normal_data;
    let dist_field = ctx.dist_field;
    let stretch_map = ctx.stretch_map;
    let cancel = ctx.cancel;
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

    // Distance field threshold in pixels for seed filtering / trace boundary.
    // margin (UV) × df_resolution → pixel distance.
    let seed_threshold = dist_field
        .map(|df| margin * df.resolution as f32)
        .unwrap_or(0.0);

    // When a distance field is provided, seed within its AABB at the seed
    // threshold (expanded by overscan margin). Otherwise use the full [0,1]².
    let (seed_lo, seed_hi) = if let Some(df) = dist_field {
        let (mlo, mhi) = df.aabb(seed_threshold);
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

    let seeds =
        generate_seeds_poisson_in(&scaled, resolution, seed_lo, seed_hi, stretch_map, cancel);
    if is_cancelled(cancel) {
        info!("Path generation cancelled after seed generation");
        return Vec::new();
    }
    debug!("Generated {} seed points", seeds.len());

    // Filter seeds by distance field (overscan threshold)
    let filtered_seeds: Vec<Vec2> = if let Some(df) = dist_field {
        seeds
            .into_iter()
            .filter(|s| df.sample(*s, seed_threshold))
            .collect()
    } else {
        seeds
    };

    let mut rng = SeededRng::new(scaled.seed);
    let mut raw_paths: Vec<Vec<Vec2>> = Vec::new();
    for (i, seed_point) in filtered_seeds.iter().enumerate() {
        // Check cancellation every 256 seeds to avoid excessive atomic loads
        if i & 0xFF == 0 && is_cancelled(cancel) {
            warn!(
                "Path generation cancelled at seed {}/{}",
                i,
                filtered_seeds.len()
            );
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
            dist_field,
            seed_threshold,
            stretch_map,
        ) {
            if let Some(clipped) = clip_path_to_uv(path) {
                let length: f32 = clipped.windows(2).map(|w| (w[1] - w[0]).length()).sum();
                if length >= min_length {
                    raw_paths.push(clipped);
                }
            }
        }
    }

    // Sort by length descending so the overlap filter accepts longer paths first.
    // Prevents short boundary-clipped strokes from blocking longer interior ones.
    raw_paths.sort_by(|a, b| {
        let len_a: f32 = a.windows(2).map(|w| (w[1] - w[0]).length()).sum();
        let len_b: f32 = b.windows(2).map(|w| (w[1] - w[0]).length()).sum();
        len_b.total_cmp(&len_a)
    });

    filter_overlapping_paths(
        &mut raw_paths,
        brush_width_uv,
        overlap_ratio,
        overlap_dist_factor,
    );

    // Sort paths by seed point y-coordinate (top-to-bottom row order).
    // Critical for correct wet-on-wet compositing order in Phase 08.
    raw_paths.sort_by(|a, b| a[0].y.total_cmp(&b[0].y));

    info!(
        "Layer {}: {} paths generated from {} seeds",
        layer_index,
        raw_paths.len(),
        filtered_seeds.len()
    );

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
mod tests;

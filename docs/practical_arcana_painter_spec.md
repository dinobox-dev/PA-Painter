# Practical Arcana Painter — Implementation Specification

## 1. Overview

### 1-1. Purpose

This document defines the full pipeline for a tool that takes a 3D mesh, a color texture, and direction guide vertices as input to generate natural hand-painted style textures. It consolidates all implementation phase specifications into a single reference.

### 1-2. Deliverables

**Final Outputs**:

| Output | Format | Description |
|--------|--------|-------------|
| Color Map | PNG/EXR (sRGB) | Color texture with brush stroke artifacts |
| Height Map | PNG/EXR (Linear) | Density-based paint coverage (subtle convex brush texture) |
| Normal Map | PNG (Linear) | Tangent-space normals derived from stroke gradients |
| Stroke ID Map | PNG (Linear, optional) | Per-pixel stroke identification (debug/masking) |
| GLB Export | .glb | 3D preview with baked paint textures |

### 1-3. Pipeline

```
Input
  ├─ 3D Mesh (.obj, .glTF/.glb)
  │    └─ Mesh groups (OBJ g/o, glTF primitives)
  ├─ Color Texture (base color map)
  └─ User Edits
       ├─ Paint slots (bind mesh group → stroke/pattern settings)
       ├─ UV masks (rasterized from mesh group triangles)
       └─ Direction guide vertices (per-slot stroke direction + params)
            │
            ▼
 ┌──────────────────────────────────────┐
 │  Stage 1. Direction Field Generation │
 │    Guide vertices → interpolation    │
 │    → continuous vector field          │
 └──────────────┬───────────────────────┘
                │
                ▼
 ┌──────────────────────────────────────┐
 │  Stage 2. Stroke Path Placement      │
 │    Direction field + UV mask         │
 │    → Poisson disk seeds → streamline │
 │    → path list                       │
 └──────────────┬───────────────────────┘
                │
                ▼
 ┌──────────────────────────────────────┐
 │  Stage 3. Per-Stroke Density Gen     │
 │    Brush model + pressure curve      │
 │    → individual density profiles     │
 │    → per-stroke density + gradients  │
 └──────────────┬───────────────────────┘
                │
                ▼
 ┌──────────────────────────────────────┐
 │  Stage 4. Compositing                │
 │    Gather-based per-segment composite│
 │    → density max (densest wins)      │
 │    → color blending + gradient accum │
 └──────────────┬───────────────────────┘
                │
                ▼
 ┌──────────────────────────────────────┐
 │  Stage 5. Final Texture Output       │
 │    Gradient → Normal Map conversion  │
 │    Color + Height + Normal export    │
 └──────────────────────────────────────┘
```

### 1-4. Core Design Principles

**Deterministic Reproduction**: Identical paint slot parameters and seed always produce identical results. All randomness derives from a seed-based PRNG (ChaCha8).

**Density-Based Compositing**: Paint density does not accumulate between strokes. At each pixel the maximum density across all strokes is kept (densest wins). The final density map preserves the most prominent bristle coverage, producing natural paint texture detail.

**Group-Based Painting**: Each mesh group maps to a paint slot with its own stroke parameter set. Strokes are clipped at UV mask boundaries derived from mesh group triangle rasterization. Slots composite in user-specified order.

**Non-Destructive Editing**: All strokes are stored as individual records. When parameters change, only the affected slot's strokes are regenerated. The original color texture is never modified.

### 1-5. Implementation Status

| Area | Status | Notes |
|------|--------|-------|
| CPU Pipeline (Stages 1-5) | **Complete** | All modules implemented and tested |
| Asset I/O | **Complete** | OBJ/glTF/glb mesh, PNG/TGA/EXR texture |
| GLB Export | **Complete** | 3D preview with baked textures |
| Vertex Group Painting | **Complete** | UV masks, paint slots, preset system |
| Project File (.pap) | **Complete** | Zip-based save/load |
| GPU Pipeline | **Deferred** | Performance bottleneck in per-stroke submit and CPU prep; batch dispatch and GPU transform optimizations designed but not fully validated |
| GUI | **Deferred** | Planning insufficient for current phase; spec exists but not implemented |

---

## 2. Input Data

### 2-1. 3D Mesh

| Item | Requirement |
|------|-------------|
| Format | .obj, .glTF/.glb (triangle/quad mesh) |
| UV | Non-overlapping single UV channel required (0-1 normalized) |
| Groups | OBJ `g`/`o` names or glTF primitive/material names parsed into `MeshGroup` entries |
| Usage | UV unwrap visualization, vertex group painting, curvature-based auto-parameters (future), 3D preview |

The tool performs all computations in UV space, so the mesh's 3D geometry is used for UV mask generation, object-space normal extraction, and preview. Mesh groups (sub-meshes) drive the vertex group painting system: each group's triangles are rasterized into a UV-space boolean mask that confines stroke placement.

### 2-2. Color Texture (Base Color Map)

| Item | Spec |
|------|------|
| Format | PNG, TGA, EXR |
| Color Space | sRGB (PNG/TGA -- converted to linear on load), Linear (EXR -- used as-is) |
| Resolution | Unrestricted (independent of output resolution) |
| Required | **Optional**. If not provided, a solid color is used (user-specified, default white) |
| Usage | Determines each stroke's base color. Samples the texel at the stroke path midpoint |

### 2-3. Output Resolution

User-specified, independent of input texture resolution. All internal computations run in UV space at this resolution.

| Preset | Resolution | Use |
|--------|-----------|-----|
| Preview | 512 x 512 | Fast iteration |
| Standard | 1024 x 1024 | General assets |
| High | 2048 x 2048 | High-resolution assets |
| Ultra | 4096 x 4096 | Close-up / cinematic |

---

## 3. Paint Slot & Vertex Group

### 3-1. Mesh Groups

A `MeshGroup` is a sub-region of the loaded mesh identified by name. OBJ files produce groups from `g` or `o` declarations; glTF files produce groups from primitives (named by material or auto-numbered). Each group references a contiguous range of triangle indices.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshGroup {
    /// Group name (OBJ `g` name or glTF primitive/material name).
    pub name: String,
    /// Start offset into the mesh's indices array.
    pub index_offset: u32,
    /// Number of indices belonging to this group (always a multiple of 3).
    pub index_count: u32,
}
```

When a mesh has no explicit groups (e.g., a single-object OBJ), the entire mesh is treated as one group under the reserved name `__full_uv__`.

### 3-2. Paint Slot

A `PaintSlot` binds a mesh group to stroke and pattern settings. This is the primary user-facing editing unit, replacing the former Region concept. Each paint slot has a 1:1 relationship with a mesh group.

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaintSlot {
    /// Name of the corresponding MeshGroup (or `"__full_uv__"` for whole UV).
    pub group_name: String,
    /// Compositing order (lower = painted first).
    pub order: i32,
    /// Stroke settings (brush physics).
    pub stroke: StrokeValues,
    /// Pattern settings (placement strategy).
    pub pattern: PatternValues,
    /// Random seed.
    pub seed: u32,
}
```

`PaintSlot.to_paint_layer()` converts to the internal `PaintLayer` representation for backward compatibility with the downstream pipeline (path placement, compositing). `PaintSlot.from_paint_layer()` provides v2-to-v3 migration.

### 3-3. Stroke Parameters

The unified `StrokeParams` struct holds all per-slot parameters for the internal pipeline:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrokeParams {
    pub brush_width: f32,
    pub load: f32,
    pub body_wiggle: f32,
    pub stroke_spacing: f32,
    pub pressure_preset: PressurePreset,
    pub color_variation: f32,
    pub max_stroke_length: f32,
    pub angle_variation: f32,
    pub max_turn_angle: f32,
    pub color_break_threshold: Option<f32>,
    pub normal_break_threshold: Option<f32>,
    pub overlap_ratio: Option<f32>,
    pub overlap_dist_factor: Option<f32>,
    pub seed: u32,
}
```

| Parameter | Default | Description |
|-----------|---------|-------------|
| `brush_width` | 30.0 | Brush width in UV pixels |
| `load` | 0.8 | Paint amount. 1.0 = enough for a clean stroke; low = dry brush |
| `body_wiggle` | 0.15 | Lateral body sway amplitude (brush width multiples). Low-freq Perlin noise shifts the active region per-column, simulating hand tremor |
| `stroke_spacing` | 1.0 | Spacing between adjacent strokes (brush width multiples) |
| `pressure_preset` | FadeOut | Pressure curve preset |
| `color_variation` | 0.1 | Per-stroke color deviation (HSV) |
| `max_stroke_length` | 240.0 | Maximum stroke length in pixels. Stroke lengths follow a power distribution biased toward longer strokes |
| `angle_variation` | 5.0 | Stroke direction random deviation (degrees) |
| `max_turn_angle` | 15.0 | Max allowed rotation between consecutive steps. Path terminates if exceeded |
| `color_break_threshold` | None | Per-step color difference threshold; strokes terminate when max channel diff exceeds this value. None = disabled |
| `normal_break_threshold` | None | Cumulative object-space normal deviation floor (dot product). Strokes terminate when `dot(n_start, n_current) < threshold`. None = disabled |
| `overlap_ratio` | None (0.7) | Fraction of points that must be "too close" to reject a path. Raise toward 1.0 to allow more overlap |
| `overlap_dist_factor` | None (0.3) | Distance factor multiplied by `brush_width_uv` for the "too close" zone |
| `seed` | 42 | Random seed for this slot (reproducibility) |

`StrokeParams::from_values(stroke, pattern, seed)` reconstructs from the split value types.

### 3-4. Split Value Types

The paint slot separates stroke parameters into two ergonomic subsets:

**StrokeValues** (brush physics):

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrokeValues {
    pub brush_width: f32,
    pub load: f32,
    pub body_wiggle: f32,
    pub pressure_preset: PressurePreset,
}
```

**PatternValues** (placement strategy):

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternValues {
    pub guides: Vec<GuideVertex>,
    pub stroke_spacing: f32,
    pub max_stroke_length: f32,
    pub angle_variation: f32,
    pub max_turn_angle: f32,
    pub color_break_threshold: Option<f32>,
    pub normal_break_threshold: Option<f32>,
    pub overlap_ratio: Option<f32>,
    pub overlap_dist_factor: Option<f32>,
    pub color_variation: f32,
}
```

### 3-5. Pressure Curve Presets

```rust
pub enum PressurePreset {
    Uniform,   // p(t) = 1.0
    FadeOut,   // p(t) = 1.0 - t^2         (most natural default)
    FadeIn,    // p(t) = t^0.5
    Bell,      // p(t) = sin(pi * t)
    Taper,     // p(t) = sin(pi * t)^0.5
}
```

### 3-6. Preset System

Named presets provide template (copy) semantics for stroke and pattern values. Applying a preset copies its values into the paint slot; subsequent edits do not affect the preset. Matching is value-equality based.

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrokePreset {
    pub name: String,
    pub values: StrokeValues,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternPreset {
    pub name: String,
    pub values: PatternValues,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PresetLibrary {
    pub strokes: Vec<StrokePreset>,
    pub patterns: Vec<PatternPreset>,
}
```

`PresetLibrary` methods:
- `matching_stroke_preset(&self, values) -> Option<&str>`: Find a preset whose values match exactly.
- `matching_pattern_preset(&self, values) -> Option<&str>`: Same for patterns.
- `try_add_stroke_preset(&mut self, preset) -> Result<(), String>`: Add if no duplicate exists; returns `Err` with the existing name.
- `try_add_pattern_preset(&mut self, preset) -> Result<(), String>`: Same for patterns.

**Built-in Stroke Presets**:

| Name | brush_width | load | body_wiggle | pressure_preset |
|------|-------------|------|-------------|-----------------|
| `flat_wide` | 40.0 | 0.8 | 0.15 | FadeOut |
| `round_thin` | 15.0 | 0.9 | 0.1 | Taper |
| `dry_brush` | 50.0 | 0.3 | 0.2 | FadeOut |
| `impasto` | 30.0 | 1.0 | 0.1 | Bell |
| `glaze` | 35.0 | 0.5 | 0.1 | Uniform |

**Built-in Pattern Presets**:

| Name | Key characteristics |
|------|-------------------|
| `uniform_horizontal` | Single horizontal guide (influence=1.0), spacing=1.0, no angle variation |
| `crosshatch` | Two guides at oblique angles, spacing=0.8, angle_variation=5.0 |
| `loose_organic` | Single guide, spacing=1.2, angle_variation=15.0, max_turn=30.0 |
| `tight_fill` | Single guide, spacing=0.6, overlap_ratio=0.8, overlap_dist_factor=0.2 |

Module: `src/types.rs`

---

## 4. Direction Field (Stage 1)

### 4-1. Guide Vertices

Direction hint points placed by the user on the UV unwrap. Each vertex has a position, direction vector, and influence radius.

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuideVertex {
    pub position: Vec2,    // UV coordinate (0-1)
    pub direction: Vec2,   // Normalized direction vector
    pub influence: f32,    // Influence radius in UV units (default 0.2)
}
```

### 4-2. Direction Field Interpolation

Generates a continuous direction field across the entire paint slot from guide vertices. The field returns a stroke direction at any UV coordinate.

**Algorithm: Smoothstep-Weighted Blend with Canonicalized Reference Alignment**

Direction vectors are headless -- a stroke going left-to-right is the same as right-to-left. This creates a 180-degree symmetry that must be handled carefully during interpolation.

**Why not doubled-angle (2-theta) circular mean?** Guides differing by exactly 90 degrees map to antipodal points in 2-theta space (0 degrees maps to 0 degrees, 90 degrees maps to 180 degrees). Since `sin(0) = sin(pi) = 0`, the sin-component is always zero regardless of weights, making the blend between 0 degrees and 90 degrees jump discontinuously instead of passing through 45 degrees. This is a common edge case (horizontal/vertical guide pairs).

**Smoothstep weight function**:

```rust
fn guide_weight(d: f32, influence: f32) -> f32 {
    let t = (d / influence).clamp(0.0, 1.0);
    let s = 1.0 - t;
    s * s * (3.0 - 2.0 * s)  // smoothstep falloff
}
```

This provides smooth, C1-continuous falloff: weight is 1.0 at d=0 and 0.0 at d=influence, with zero derivative at both endpoints. Unlike 1/(d+epsilon)^2, there is no singularity at d=0 and the influence boundary is crisp.

```
direction_at(uv, guides):
    If no guide vertices:
        return (1, 0)                      # Default horizontal

    If only 1 guide vertex:
        return normalize(guide[0].direction)

    # Canonicalize: map direction to upper half-plane
    canonicalize(d):
        if d.y < 0 or (d.y == 0 and d.x < 0): return -d
        else: return d

    # Collect (weight, canonicalized direction) for guides within influence
    weighted = []
    for each guide g:
        d = distance(uv, g.position)
        if d > g.influence: continue
        w = guide_weight(d, g.influence)
        dir = canonicalize(normalize(g.direction))
        weighted.append((w, dir))

    # Nearest-neighbor fallback
    if weighted is empty:
        nearest = guide with min distance to uv
        return normalize(nearest.direction)

    # Reference-based sign alignment
    ref = direction of highest-weight entry
    sum = (0, 0)
    for (w, dir) in weighted:
        if dot(dir, ref) < 0: dir = -dir
        sum += w * dir

    return normalize(sum)
```

**Key Properties**:
1. 180-degree symmetry: `(1, 0)` and `(-1, 0)` both canonicalize to `(1, 0)`, so they never cancel
2. Smooth 90-degree interpolation: Unlike the 2-theta approach, blending passes smoothly through 45 degrees
3. Smoothstep falloff: Weight decreases smoothly from 1.0 to 0.0 over the influence radius, with crisp boundary
4. Nearest-neighbor fallback: Points outside all influence radii get the nearest guide's direction

### 4-3. Direction Field Generation

```rust
/// Compute direction at a UV coordinate given guide vertices.
pub fn direction_at(uv: Vec2, guides: &[GuideVertex]) -> Vec2

/// Generate a full direction field texture (resolution x resolution, row-major).
pub fn generate_direction_field(guides: &[GuideVertex], resolution: u32) -> Vec<Vec2>
```

**Precomputed Field** (`DirectionField`): For fast lookup during streamline tracing, a grid is built at 1/4 output resolution (clamped to [64, 2048]). Lookups use bilinear interpolation with hemisphere alignment (all four texel samples aligned to the same hemisphere before interpolation) to prevent sign-flip artifacts.

```rust
pub struct DirectionField {
    data: Vec<Vec2>,
    resolution: u32,
}

impl DirectionField {
    pub fn new(guides: &[GuideVertex], resolution: u32) -> Self
    pub fn sample(&self, uv: Vec2) -> Vec2
}
```

Module: `src/direction_field.rs`

---

## 5. UV Mask

### 5-1. Role of UV Masks

A UV mask is a boolean bitmap in UV space that indicates which pixels belong to a mesh group's triangles. It replaces the former polygon-based region mask system. Masks are generated automatically from mesh geometry rather than user-drawn polygons, enabling group-based painting where stroke placement is confined to the UV footprint of each mesh sub-group.

### 5-2. UvMask Structure

```rust
pub struct UvMask {
    pub data: Vec<bool>,
    pub resolution: u32,
}
```

`data` is a row-major boolean grid of size `resolution * resolution`. `true` pixels indicate UV coverage by the mesh group's triangles.

### 5-3. Mask Generation

`UvMask::from_mesh_group(mesh, group, resolution)` rasterizes a mesh group's triangles into UV space:

1. For each triangle in the group's index range (`index_offset..index_offset + index_count`), compute the triangle's AABB in pixel coordinates.
2. For each pixel within the AABB, test whether the pixel center lies inside the triangle using a barycentric point-in-triangle test.
3. Set `data[py * resolution + px] = true` for pixels that pass the test.

**Barycentric point-in-triangle test**:

```
point_in_triangle(p, a, b, c):
    v0 = b - a
    v1 = c - a
    v2 = p - a

    d00 = dot(v0, v0)
    d01 = dot(v0, v1)
    d11 = dot(v1, v1)
    d20 = dot(v2, v0)
    d21 = dot(v2, v1)

    denom = d00 * d11 - d01 * d01
    if |denom| < 1e-12: return false

    inv_denom = 1 / denom
    v = (d11 * d20 - d01 * d21) * inv_denom
    w = (d00 * d21 - d01 * d20) * inv_denom
    u = 1 - v - w

    EDGE_EPS = -1e-4
    return u >= EDGE_EPS and v >= EDGE_EPS and w >= EDGE_EPS
```

The negative edge epsilon provides a slight expansion to avoid hairline gaps along shared triangle edges.

### 5-4. Mask Operations

**Dilation**: `dilate(radius)` expands the mask by `radius` pixels in all directions (circular kernel). This prevents boundary artifacts where strokes terminate prematurely due to sub-pixel alignment between mask edges and seed positions.

**AABB Computation**: `aabb()` returns the bounding box of true pixels in UV coordinates `(min, max)`. Used to restrict seed generation to the relevant area.

**Point Query**: `sample(uv) -> bool` converts UV to pixel coordinates and returns the mask value. Used for seed filtering and streamline tracing termination.

**Full UV Mask**: `UvMask::full(resolution)` creates an all-true mask for `__full_uv__` groups (meshes without explicit groups).

### 5-5. Mask Application

UV masks are used at three points in the pipeline:

1. **Seed generation**: The mask's AABB (plus overscan margin) bounds the Poisson disk sampling region. Seeds outside the mask are filtered before tracing.
2. **Streamline tracing**: At each step, `mask.sample(next_pos)` is checked. If the next position falls outside the mask, the streamline terminates.
3. **Compositing**: No additional mask check needed -- strokes are already confined by tracing termination.

Module: `src/uv_mask.rs`

---

## 6. Stroke Path Placement (Stage 2)

### 6-1. Seed Point Distribution (Poisson Disk)

Starting points are distributed using Poisson disk sampling within the mask's AABB (expanded by an overscan margin). This produces blue-noise distribution with a guaranteed minimum spacing, yielding more natural and uniform coverage than a jittered grid.

**Algorithm**: Bridson's algorithm with minimum distance derived from stroke parameters:

```
min_dist = brush_width / resolution * stroke_spacing
```

**Overscan margin**: The sampling region extends beyond the mask AABB by `brush_width_uv * 3.0` to produce strokes that start right at boundaries and fill edge gaps.

```rust
fn generate_seeds_poisson_in(
    params: &StrokeParams,
    resolution: u32,
    lo: Vec2,
    hi: Vec2,
) -> Vec<Vec2>
```

**Properties**:
- Minimum spacing guarantee: No two seeds are closer than `min_dist`.
- Blue-noise spectrum: Visually uniform without grid artifacts.
- Deterministic: Seeded from `params.seed` via the project's `SeededRng`.
- Candidate count: 30 candidates per active point (Bridson standard).

Seeds are then filtered by the UV mask (`mask.sample(seed)`) to remove any that fall outside the group's UV footprint.

### 6-2. Streamline Tracing

Trace a path from each seed point following the precomputed direction field.

**Stroke Length Distribution**: Target length uses a single-RNG-call power distribution:

```
max_length_uv = max_stroke_length / resolution
target_length = max_length_uv * sqrt(U),   U ~ Uniform(0,1)
```

This produces naturally varied lengths biased toward longer strokes, with a smooth tail of shorter ones. Median is approximately `max_stroke_length * 0.707`.

**Tracing Phase**: Follow the direction field with angle variation, curvature limit, and boundary checks.

```
trace_streamline(seed, field, params, resolution, rng, color_tex, normal_data,
                 uv_bounds, mask):
    step_size_uv = 1.0 / resolution

    # Target length: consume RNG FIRST to ensure
    # identical RNG sequence regardless of path outcome.
    max_length_uv = params.max_stroke_length / resolution
    target_length = max_length_uv * sqrt(rng.next_f32())

    pos = seed
    prev_dir = field.sample(pos)

    # ── Tracing phase ─────────────────────────────────
    path = [pos]
    length = 0.0

    while length < target_length:
        dir = field.sample(pos)

        # Direction alignment (critical correctness fix):
        # direction_at() returns headless 180-degree-symmetric vectors.
        # Without aligning to prev_dir, the path reverses direction
        # mid-trace, producing zigzag paths instead of smooth strokes.
        if dir.dot(prev_dir) < 0: dir = -dir

        # Apply angle deviation (gradual)
        angle_offset = (rng.next_f32() - 0.5) * angle_variation_rad * 2
        dir = rotate(dir, angle_offset * 0.1)
        dir = dir.normalize()
        if dir == ZERO: break

        # Curvature limit
        turn = acos(clamp(prev_dir.dot(dir), -1, 1))
        if turn > max_turn_angle_rad: break

        next_pos = pos + dir * step_size_uv

        # UV boundary check
        if next_pos outside uv_bounds: break

        # Mask boundary check
        if mask is Some and !mask.sample(next_pos): break

        # Color boundary check
        if color_break_threshold is Some:
            if channel_max_diff(color_at(pos), color_at(next_pos)) > threshold:
                break

        # Normal boundary check (cumulative from stroke start)
        if normal_break_threshold is Some:
            if dot(start_normal, normal_at(next_pos)) < threshold:
                break

        path.push(next_pos)
        prev_dir = dir
        pos = next_pos
        length += step_size_uv

    # Minimum length filter
    if length < brush_width / resolution: return None

    return path
```

**Direction Alignment** (correctness-critical): `field.sample()` returns headless (180-degree-symmetric) vectors. Without `if dir.dot(prev_dir) < 0 { dir = -dir }`, the streamline can reverse direction mid-trace, producing zigzag paths instead of smooth strokes.

**Termination conditions** (summary):

| Condition | Parameter | Default |
|-----------|-----------|---------|
| Target length reached | `max_stroke_length` | 240px |
| Cumulative turn angle | `max_turn_angle` | 15 degrees |
| UV boundary | -- | `[0,1]^2` or mask AABB |
| Mask boundary | UV mask | Group triangles |
| Color boundary | `color_break_threshold` | None (disabled) |
| Normal boundary | `normal_break_threshold` | None (disabled) |

### 6-3. UV Clipping

Paths traced with overscan may have initial or trailing points outside `[0,1]^2`. `clip_path_to_uv()` trims these. Paths with fewer than 2 points after clipping are discarded.

### 6-4. Overlap Filter

Paths excessively overlapping existing paths are removed. A path is rejected if >= `overlap_ratio` of its points are within `brush_width_uv * overlap_dist_factor` of any accepted path's centerline points.

```rust
pub fn filter_overlapping_paths(
    paths: &mut Vec<Vec<Vec2>>,
    brush_width_uv: f32,
    overlap_ratio: f32,      // default 0.7
    overlap_dist_factor: f32, // default 0.3
)
```

The overlap filter is parameterizable via `StrokeParams`. The defaults (`overlap_ratio=0.7`, `overlap_dist_factor=0.3`) remove near-duplicates while preserving natural stroke density. Raising `overlap_ratio` toward 1.0 and lowering `overlap_dist_factor` allows denser packing.

Uses a spatial hash grid for O(n * m * k) performance instead of brute-force O(n^2 * m^2), where k is the average number of points per grid cell.

### 6-5. Stroke IDs

Stroke IDs use a plain sequential counter within each layer. The layer index is available separately via `StrokePath.layer_index`.

```rust
StrokePath::new(path, layer_index, i as u32)
```

### 6-6. Full Pipeline

```rust
/// Generate all stroke paths for a layer.
/// Returns paths in paint order (sorted by seed y-coordinate, top to bottom).
pub fn generate_paths(
    layer: &PaintLayer,
    layer_index: u32,
    resolution: u32,
    color_tex: Option<&ColorTextureRef<'_>>,
    normal_data: Option<&MeshNormalData>,
    mask: Option<&UvMask>,
) -> Vec<StrokePath>
```

**Coverage Target**: >= 90% of paint slot area covered with default parameters.

Module: `src/path_placement.rs`

### 6-7. Density Control Analysis

Stroke density is determined by a 4-stage pipeline:

**Stage 1 -- Seed Generation**: `spacing = brush_width / resolution * stroke_spacing`. The `stroke_spacing` parameter is the primary density control. Halving spacing quadruples seed count.

**Stage 2 -- Streamline Tracing**: Each seed traces a path that may terminate early due to curvature, boundary, or color/normal breaks.

**Stage 3 -- Minimum Length Filter**: Paths shorter than `brush_width / resolution` (one brush width in UV units) are discarded.

**Stage 4 -- Overlap Filter**: Near-duplicate paths are removed based on `overlap_ratio` and `overlap_dist_factor`.

**Parameter Impact Ranking**:

| Rank | Parameter | Impact Path | Notes |
|------|-----------|-------------|-------|
| 1 | `stroke_spacing` | Seed grid spacing directly | Most direct |
| 2 | `brush_width` | Grid spacing + overlap filter distance + minimum length | Triple effect |
| 3 | `max_stroke_length` | Average stroke length affects coverage | Indirect |
| 4 | `max_turn_angle` | Early termination frequency | Direction-field dependent |
| 5 | `color/normal_break_threshold` | Path breaks at boundaries | Selective |

**Tuning Strategies**:
- Lower `stroke_spacing` to 0.5-0.6 for denser packing, combined with:
- Raise `overlap_ratio` to 0.8-0.9 and lower `overlap_dist_factor` to 0.15-0.2 to relax the overlap filter.
- This combination effectively removes the density ceiling imposed by the default overlap filter thresholds.

### 6-8. Constants

| Name | Value | Description |
|------|-------|-------------|
| Overscan margin | `brush_width_uv * 3.0` | Seed region expansion beyond mask AABB |
| Min length | `brush_width / resolution` | Minimum path length (1 brush width) |
| Bridson k | 30 | Candidates per active Poisson disk point |
| Default overlap_ratio | 0.7 | Fraction of points triggering rejection |
| Default overlap_dist_factor | 0.3 | Distance threshold (brush_width multiples) |

---

## 7. Stroke Height Generation (Stage 3)

### 7-1. Brush Profile Generation

A 1D density array representing the bristle pattern across the brush width.

**Step 1: fBm-based bristle pattern**

```
for j in 0..width:
    density[j] = fbm_4octaves(j * 0.3, seed)
normalize density to [0, 1]
```

| Constant | Value | Description |
|----------|-------|-------------|
| `FBM_FREQ` | 0.3 | Base frequency for fBm |
| `FBM_LACUNARITY` | 2.0 | Frequency multiplier per octave |
| `FBM_GAIN` | 0.5 | Amplitude decay per octave |
| `FBM_OCTAVES` | 4 | Number of fBm octaves |
| `GAP_DENSITY` | 15 | Average pixels per bristle gap |

**Step 2: Bristle gap insertion**

V-shaped dips at random positions drive density near zero, representing gaps between bristle bundles.

```rust
pub fn generate_brush_profile(width: usize, seed: u32) -> Vec<f32>
```

**Noise crate usage notes**: The `noise` crate provides 2D/3D/4D noise functions only, not 1D. To evaluate 1D noise:

- **fBm for brush profile**: Use `Fbm<Perlin>` and evaluate at `[j as f64 * FBM_FREQ as f64, seed as f64 * 1000.0]` (2D with seed as second coordinate).
- **1D Perlin for body wiggle**: Use `Perlin::new(seed + 2)` and evaluate at `[t as f64, 0.0]` to decorrelate from other noise.
- All noise functions return `f64`; cast to `f32` after evaluation.

Module: `src/brush_profile.rs`

### 7-2. Stroke Density Map Generation

The stroke height module generates a 2D density map in local coordinates (along-stroke x across-stroke). Values represent bristle coverage density in the range 0.0-1.0, used for normal map detail and opacity blending. There is no physical paint height or impasto ridge.

```rust
pub struct StrokeHeightResult {
    /// Density map in local coordinates.
    /// Dimensions: height = brush_width_px, width = stroke_length_px
    /// Stored row-major: data[y * width + x]
    pub data: Vec<f32>,
    pub width: usize,   // stroke_length_px
    pub height: usize,  // brush_width_px
}

pub fn generate_stroke_height(
    brush_profile: &[f32],
    stroke_length_px: usize,
    params: &StrokeParams,
    seed: u32,
) -> StrokeHeightResult
```

**Stroke Length Computation** (critical sync point): The `stroke_length_px` must be computed identically across stroke height generation and compositing:

```rust
let stroke_length_px = (path.arc_length() * resolution as f32).ceil() as usize;
```

#### Body Computation

For each column x in `[0, stroke_length_px)`:

```
t = x / stroke_length_px
p = evaluate_pressure(pressure_preset, t)

# Effective width from pressure
active_width = brush_width_px * (MIN_WIDTH_RATIO + (1.0 - MIN_WIDTH_RATIO) * p)

# Body wiggle: low-freq Perlin noise shifts center laterally
wiggle_offset = perlin_1d(t, seed + 2) * body_wiggle * brush_width_px

# Paint depletion
remaining = load * lerp(1.0, DEPLETION_FLOOR, t^DEPLETION_EXPONENT)

# Resample brush profile into active width, compute effective density
center = brush_width_px / 2 + wiggle_offset
active_start = floor(center - active_width / 2).clamp(0, brush_width_px)
active_end = ceil(center + active_width / 2).min(brush_width_px)

for j in 0..active_count:
    source_idx = j * (brush_width_px / active_count)
    rd = interpolate_array(brush_profile, source_idx)
    effective_density = p^(5 * (1 - rd) + 1)
    data[y][x] = effective_density * remaining
```

The power-law density formula `p^(5 * (1 - rd) + 1)` maps bristle density (`rd`) and pressure (`p`) non-linearly:
- At high bristle density (`rd` near 1.0): exponent approaches 1, so density tracks pressure linearly.
- At low bristle density (`rd` near 0.0): exponent approaches 6, so low pressure produces dramatically lower density -- bristle gaps become more pronounced.
- This creates the characteristic dry-brush effect where sparse bristle regions lose paint first.

### 7-3. Pre-Composited Stroke Gradients

Sobel gradients are computed per-stroke in local coordinates before compositing, enabling fine-grained normal map detail from individual bristle patterns. The compositing stage rotates these local gradients into UV space when transferring the winning stroke's detail.

```rust
pub struct StrokeGradientResult {
    /// Gradient in local-X direction (along stroke). Row-major.
    pub gx: Vec<f32>,
    /// Gradient in local-Y direction (across stroke). Row-major.
    pub gy: Vec<f32>,
    pub width: usize,
    pub height: usize,
}

pub fn compute_stroke_gradients(height: &StrokeHeightResult) -> StrokeGradientResult
```

The Sobel kernel operates on the local density map. Zero-density pixels (outside the active brush region) are replaced with the center pixel's value during the convolution, so stroke boundaries produce no spurious gradient. Edge pixels of the buffer are left at zero.

### 7-4. Visual Effect Reference

| Parameters | Visual Effect |
|------------|--------------|
| load=1.0, Uniform pressure | Maximum density, full coverage. Peak near 1.0 |
| load=0.3, Uniform pressure | Dry brush. Max density capped at ~0.3, bristle gaps prominent |
| load=1.0, FadeOut pressure | Density narrows and fades toward stroke end |
| body_wiggle=0.3 | Lateral sway in stroke body, simulating hand tremor |
| body_wiggle=0.0, Uniform | Perfectly centered, no lateral shift |

### 7-5. Constants

| Constant | Value | Description |
|----------|-------|-------------|
| `MIN_WIDTH_RATIO` | 0.3 | Minimum brush width ratio at pressure 0 |
| `DEPLETION_FLOOR` | 0.15 | Remaining paint ratio at stroke end |
| `DEPLETION_EXPONENT` | 0.7 | Depletion curve shape |

Module: `src/stroke_height.rs`

---
## 8. Stroke Color

### 8-1. Base Color Sampling

Each stroke's base color is sampled from the Base Color Map at the path midpoint using bilinear interpolation.

```rust
pub fn sample_bilinear(texture: &[Color], tex_width: u32, tex_height: u32, uv: Vec2) -> Color
```

The compositing pipeline wraps texture data through `BaseColorSource`, which groups the recurring `(texture, tex_width, tex_height, solid_color)` tuple:

```rust
pub struct BaseColorSource<'a> {
    pub texture: Option<&'a [Color]>,
    pub tex_width: u32,
    pub tex_height: u32,
    pub solid_color: Color,
}
```

When a texture reference is needed for path generation (color-break termination), it is passed as a lightweight `ColorTextureRef`:

```rust
pub struct ColorTextureRef<'a> {
    pub data: &'a [Color],
    pub width: u32,
    pub height: u32,
}
```

### 8-2. Color Variation

Subtle per-stroke color shifts in HSV space for a natural hand-painted feel.

```
hsv = rgb_to_hsv(stroke_base_color)
hsv.h += (random - 0.5) * color_variation * 0.5
hsv.s += (random - 0.5) * color_variation
hsv.v += (random - 0.5) * color_variation * 0.7
stroke_color = hsv_to_rgb(clamp(hsv))
```

### 8-3. Intra-Stroke Color

Color is uniform within a single stroke -- a key characteristic of hand-painted style.

```rust
pub fn rgb_to_hsv(c: Color) -> HsvColor
pub fn hsv_to_rgb(hsv: HsvColor) -> Color

pub fn compute_stroke_color(
    path: &StrokePath,
    color_texture: Option<&[Color]>,
    tex_width: u32, tex_height: u32,
    solid_color: Color,
    color_variation: f32,
    rng: &mut SeededRng,
) -> Color
```

### 8-4. Color Difference

A utility function is provided for per-step color-break termination during path tracing. It computes the maximum per-channel absolute difference between two colors (RGB only, ignores alpha):

```rust
pub fn channel_max_diff(a: Color, b: Color) -> f32
```

Module: `src/stroke_color.rs`

---

## 9. Compositing (Stage 4)

### 9-1. Global Maps

```rust
pub struct GlobalMaps {
    /// Height map. 0.0 = no paint. Row-major, size = resolution * resolution.
    pub height: Vec<f32>,
    /// Color map. Row-major, size = resolution * resolution.
    pub color: Vec<Color>,
    /// Stroke ID map. 0 = no stroke. Row-major.
    pub stroke_id: Vec<u32>,
    /// Object-space normal per pixel (composited from strokes).
    /// Empty in SurfacePaint mode.
    pub object_normal: Vec<[f32; 3]>,
    /// Paint detail gradient (X component in UV space), composited per-stroke.
    pub gradient_x: Vec<f32>,
    /// Paint detail gradient (Y component in UV space), composited per-stroke.
    pub gradient_y: Vec<f32>,
    pub resolution: u32,
}
```

**Initialization** depends on `BackgroundMode`:

- **Opaque**: `height` = zeros, `stroke_id` = zeros, `color` = resampled base color texture (bilinear upsampling to output resolution) or solid color. `gradient_x`, `gradient_y` = zeros.
- **Transparent**: `color` = fully transparent `(0, 0, 0, 0)` regardless of texture or solid color. All other buffers = zeros.

In both modes, `object_normal` is allocated at full resolution when `NormalMode::DepictedForm` is active, and is left empty (zero-length) in `SurfacePaint` mode.

### 9-2. Compositing Order

PaintSlot order (ascending `order` value) determines which paint goes down first. Within each slot, strokes are composited in their path order (sorted by seed y-coordinate, top-to-bottom). This ordering is deterministic for identical parameters and seed.

### 9-3. Single Stroke Compositing -- Gather-Based

The compositing engine uses a gather approach: for each path segment, it computes a tight bounding box and evaluates every global pixel inside it. Each pixel is projected onto the segment in O(1) (dot products), converted to local-frame coordinates, and bilinearly sampled from the local density map. This guarantees every destination pixel is explicitly evaluated with no scatter-write gaps, while keeping the per-pixel cost constant (no full-path scan).

**Per-stroke appearance** is encapsulated in:

```rust
pub struct StrokeAppearance {
    pub color: Color,
    pub id: u32,
    pub normal: Option<[f32; 3]>,
    pub transparent: bool,
}
```

**Compositing rules per pixel**:

| Channel | Rule |
|---------|------|
| Height | `global.height[idx] = max(h, prev_h)` -- densest wins, no accumulation |
| Gradient | Winner-takes-all: if `h >= prev_h`, rotate local gradients to UV space and overwrite |
| Opacity | `smoothstep(0.0, DENSITY_OPACITY_THRESHOLD, density)` where `DENSITY_OPACITY_THRESHOLD = 0.7` |
| Color (Opaque) | `lerp(global.color, stroke_color, opacity)` |
| Color (Transparent, first paint) | Set color directly with `alpha = opacity` |
| Color (Transparent, over-paint) | `lerp` RGB channels with opacity; `alpha = max(prev_alpha, opacity)` |
| Stroke ID | Last stroke wins (overwrite) |
| Object Normal | Overwrite with stroke normal (DepictedForm mode only) |

**Color opacity behavior**:
- Body (density near 1.0): opacity saturates to 1.0, full cover
- Dry brush (low density): low opacity, underlying color shows through
- Bristle gaps (density near 0): opacity near 0, underlying exposed

```rust
pub fn composite_stroke(
    local_height: &StrokeHeightResult,
    local_gradient: &StrokeGradientResult,
    path: &StrokePath,
    resolution: u32,
    appearance: &StrokeAppearance,
    global: &mut GlobalMaps,
)
```

### 9-4. Full Pipeline

```rust
pub fn composite_all(
    layers: &[PaintLayer],
    resolution: u32,
    base_color: &BaseColorSource,
    settings: &OutputSettings,
    normal_data: Option<&MeshNormalData>,
    masks: &[Option<&UvMask>],
) -> GlobalMaps
```

Steps:

1. **Sort layers** by `order` (ascending), preserving original index for stroke ID encoding.
2. **Generate paths** in parallel (rayon `par_iter`) across layers. Each layer calls `generate_paths()` with its optional UV mask and color texture reference.
3. **Composite sequentially** (preserves deterministic blending order). For each layer in order:
   a. Generate brush profile once per layer (same seed for all strokes in layer).
   b. Pre-compute stroke colors sequentially (preserves RNG determinism).
   c. Pre-compute stroke normals (midpoint sampling from `MeshNormalData`).
   d. Build density maps and gradients in parallel (rayon `par_iter` over strokes).
   e. Composite each stroke sequentially using the gather-based approach.

An extended version supports reusing pre-generated paths:

```rust
pub fn composite_all_with_paths(
    layers: &[PaintLayer],
    resolution: u32,
    base_color: &BaseColorSource,
    settings: &OutputSettings,
    cached_paths: Option<&[Vec<StrokePath>]>,
    normal_data: Option<&MeshNormalData>,
    masks: &[Option<&UvMask>],
) -> GlobalMaps
```

### 9-5. Single-Layer Compositing

```rust
pub fn composite_layer(
    layer: &PaintLayer,
    layer_index: u32,
    global: &mut GlobalMaps,
    settings: &OutputSettings,
    base_color: &BaseColorSource,
    cached_paths: Option<&[StrokePath]>,
    normal_data: Option<&MeshNormalData>,
    mask: Option<&UvMask>,
)
```

Extracted inner loop for single-layer preview regeneration. The caller is responsible for clearing/resetting pixels belonging to this layer before calling (if needed).

### 9-6. Normal Map Modes

| Mode | Description |
|------|-------------|
| **SurfacePaint** | Height-only normals. Pre-composited gradients are converted to tangent-space normals using `N = normalize(-gx * strength, -gy * strength, 1)`. Flat = (0.5, 0.5, 1.0). No mesh geometry needed. |
| **DepictedForm** | Object-space normals from mesh geometry, perturbed by paint gradients. Each pixel: (1) read composited gradient, (2) look up composited stroke normal + TBN basis from mesh, (3) perturb: `N_perturbed = normalize(N_obj + strength * (-gx * T + -gy * B))`, (4) convert to tangent space. |

### 9-7. Background Modes

| Mode | Description |
|------|-------------|
| **Opaque** | Strokes blend with the base color/texture. Unpainted areas retain the original base color. |
| **Transparent** | Unpainted areas are fully transparent (alpha = 0). First stroke paint on a virgin pixel sets color directly. Subsequent over-paint blends paint-on-paint only, never with the background. Alpha = max of all contributing stroke opacities. |

Module: `src/compositing.rs`

---

## 10. Final Texture Output (Stage 5)

### 10-1. Height Map Normalization

Density values are already in [0, 1] range (effective_density * remaining). Normalization clamps to [0, 1]:

```rust
pub fn normalize_height_map(height: &[f32]) -> Vec<f32>
```

No ridge normalization is needed since the density model produces values directly in the representable range.

### 10-2. Normal Map Generation

Two generation paths depending on `NormalMode`:

**SurfacePaint** -- Pre-composited gradients to tangent-space normals:

```rust
pub fn generate_normal_map(
    gradient_x: &[f32],
    gradient_y: &[f32],
    resolution: u32,
    normal_strength: f32,
) -> Vec<[f32; 3]>
```

Flat surface = `(0.5, 0.5, 1.0)`. Gradients are computed per-stroke in local space (Sobel filter on the local density map) and composited into global UV space during `composite_stroke()`, so no global Sobel pass is needed here.

**DepictedForm** -- Object-space normals perturbed by paint gradients:

```rust
pub fn generate_normal_map_depicted_form(
    gradient_x: &[f32],
    gradient_y: &[f32],
    normal_data: &MeshNormalData,
    global_object_normals: &[[f32; 3]],
    resolution: u32,
    normal_strength: f32,
) -> Vec<[f32; 3]>
```

For each pixel:
1. Read pre-composited gradient `(gx, gy)` from global gradient buffers.
2. Look up the composited object-space normal `N_obj`, tangent `T`, and bitangent `B` from `MeshNormalData`.
3. Perturb: `perturbed = normalize(N_obj + strength * (-gx * T + -gy * B))`.
4. Convert to tangent space: `ts = (perturbed.dot(T), perturbed.dot(B), perturbed.dot(N_geom))`.
5. Encode to `[0, 1]`.

Where mesh coverage is absent, falls back to SurfacePaint normals.

`normal_strength` (default 1.0) controls visual depth of impasto.

### 10-3. Export Functions

| Map | PNG Export | EXR Export |
|-----|-----------|------------|
| Color Map | sRGB (apply `linear_to_srgb()`), RGB8 or RGBA8 | Linear float, RGB or RGBA |
| Height Map | Linear grayscale (no gamma), L8 | Linear float (RGB with R=G=B=height) |
| Normal Map | Linear RGB. Flat = (128, 128, 255) | N/A (PNG only) |
| Stroke ID Map | RGB PNG with golden-angle hue spacing | N/A (PNG only) |

When `BackgroundMode::Transparent`, color maps are exported with alpha (RGBA8 for PNG, RGBA for EXR).

```rust
pub fn export_all(
    global: &GlobalMaps,
    settings: &OutputSettings,
    output_dir: &Path,
    format: ExportFormat,
    normal_data: Option<&MeshNormalData>,
) -> Result<(), OutputError>
```

### 10-4. Global Output Settings

```rust
pub struct OutputSettings {
    pub resolution_preset: ResolutionPreset,
    pub normal_strength: f32,
    pub normal_mode: NormalMode,
    pub background_mode: BackgroundMode,
}
```

| Field | Default | Description |
|-------|---------|-------------|
| `resolution_preset` | Standard (1024) | Output resolution tier |
| `normal_strength` | 1.0 | Visual depth of impasto (0.1 -- 5.0) |
| `normal_mode` | DepictedForm | Normal map generation mode |
| `background_mode` | Opaque | Background compositing mode |

Module: `src/output.rs`

---

## 11. Asset I/O

### 11-1. Mesh Loading

```rust
pub struct LoadedMesh {
    pub positions: Vec<Vec3>,
    pub uvs: Vec<Vec2>,
    pub indices: Vec<u32>,
    pub groups: Vec<MeshGroup>,
}
```

`MeshGroup` identifies a sub-group within a loaded mesh (vertex group, submesh, or OBJ object):

```rust
pub struct MeshGroup {
    pub name: String,
    pub index_offset: u32,
    pub index_count: u32,
}
```

**Format handling**:

| Format | Source | Group Name |
|--------|--------|-----------|
| OBJ | tobj `Model.name` | OBJ `g` or `o` name |
| glTF/glb | gltf primitive | Material name, or `primitive_N` if unnamed |

Meshes with no named groups are treated as a single `__full_uv__` group covering the entire UV space.

Quads are automatically triangulated (split along shortest diagonal). All indices are always triangulated (multiples of 3).

```rust
pub fn load_mesh(path: &Path) -> Result<LoadedMesh, MeshError>
```

### 11-2. GLB Export

3D preview export as a binary glTF (.glb) file with color texture and normal map baked onto a subdivided mesh.

```rust
pub fn export_preview_glb(
    mesh: &LoadedMesh,
    color_map: &[Color],
    height_map: &[f32],
    normal_map: &[[f32; 3]],
    resolution: u32,
    displacement_scale: f32,
    path: &Path,
) -> Result<(), OutputError>
```

A transparent variant is also available for `BackgroundMode::Transparent`:

```rust
pub fn export_preview_glb_transparent(
    mesh: &LoadedMesh,
    color_map: &[Color],
    height_map: &[f32],
    normal_map: &[[f32; 3]],
    resolution: u32,
    displacement_scale: f32,
    path: &Path,
) -> Result<(), OutputError>
```

**Implementation details**:
- Barycentric triangle subdivision (configurable level, default 8) for smooth preview.
- Optional height-based vertex displacement along interpolated vertex normals.
- In-memory PNG encoding for color (RGBA8) and normal (RGB8) textures.
- Manual GLB binary packing (no external glTF writer crate): `serde_json` for the JSON chunk, manual binary packing for the BIN chunk.
- PBR material: metallic = 0.0, roughness = 0.8, base color texture + normal texture.
- Alpha mode set to `BLEND` for transparent export, `OPAQUE` otherwise.

Module: `src/glb_export.rs`

### 11-3. Texture Loading

```rust
pub struct LoadedTexture {
    pub pixels: Vec<[f32; 4]>,   // Linear float RGBA [0, 1]
    pub width: u32,
    pub height: u32,
}

pub fn load_texture(path: &Path) -> Result<LoadedTexture, TextureError>
```

| Format | Color Space | Handling |
|--------|------------|---------|
| PNG, TGA | sRGB | Converted to linear on load via `srgb_to_linear()` |
| EXR | Linear | Used as-is (pass-through) |

### 11-4. Color Space Conversion

```rust
pub fn srgb_to_linear(s: f32) -> f32    // Input loading
pub fn linear_to_srgb(l: f32) -> f32    // Output export
```

Standard IEC 61966-2-1 transfer functions with the 0.04045 / 0.0031308 breakpoint.

### 11-5. UV Edge Extraction

```rust
pub fn extract_uv_edges(mesh: &LoadedMesh) -> Vec<(Vec2, Vec2)>
```

Returns deduplicated UV-space edges for wireframe visualization.

Module: `src/asset_io.rs`

---

## 12. Project File (.pap)

### 12-1. Storage Format (.pap v3)

Projects are saved as `.pap` files -- zip archives with renamed extension.

```
project.pap (zip)
├── manifest.json         # Version "3.0.0", metadata
├── mesh_ref.json         # External mesh file path
├── color_ref.json        # External color texture path
├── slots.json            # PaintSlot list (v3)
├── presets.json           # PresetLibrary snapshot
├── settings.json         # Global output settings
├── cache/
│   ├── height_map.bin    # Bincode cached density map (optional)
│   └── color_map.bin     # Bincode cached color map (optional)
└── thumbnails/
    └── preview.png       # 256x256 thumbnail (optional)
```

### 12-2. Data Structures

```rust
pub struct Manifest {
    pub version: String,
    pub app_name: String,
    pub created_at: String,       // ISO 8601
    pub modified_at: String,      // ISO 8601
}

pub struct MeshRef {
    pub path: String,
    pub format: String,           // "obj", "gltf", "glb"
}

pub struct ColorRef {
    pub path: Option<String>,     // None = solid color
    pub solid_color: [f32; 3],
}

pub struct Project {
    pub manifest: Manifest,
    pub mesh_ref: MeshRef,
    pub color_ref: ColorRef,
    pub slots: Vec<PaintSlot>,
    pub presets: PresetLibrary,
    pub settings: OutputSettings,
    pub cached_height: Option<Vec<f32>>,
    pub cached_color: Option<Vec<[f32; 4]>>,
    pub cached_paths: Option<Vec<Vec<StrokePath>>>,
}
```

`Project` provides adapter methods for downstream pipeline compatibility:

- `paint_layers()` -- converts all `PaintSlot`s to `PaintLayer`s.
- `build_masks()` -- builds `UvMask`s for each slot from the loaded mesh.
- `cached_paths_if_valid()` -- returns cached paths if they match current state.
- `set_cached_paths()` / `invalidate_path_cache()` -- runtime path cache management.

### 12-3. Backward Compatibility

| Source Format | Migration Path |
|---------------|----------------|
| v1 `regions.json` | Deserialized as `PaintLayer` (ignoring `id`/`mask` fields), then migrated via `PaintSlot::from_paint_layer()`. Group name set to `__full_uv__`. |
| v2 `layers.json` | Deserialized as `Vec<PaintLayer>`, migrated to `Vec<PaintSlot>` via `from_paint_layer()`. Built-in presets + migrated values auto-generated. |
| v3 `slots.json` | Direct load. `presets.json` loaded alongside. |

### 12-4. Non-Destructive Principle

The project file does not modify input data (mesh, color texture); only external references are stored. All edit state is fully contained within the project file, so identical results can be reproduced with the same inputs.

```rust
pub fn save_project(project: &Project, path: &Path) -> Result<(), ProjectError>
pub fn load_project(path: &Path) -> Result<Project, ProjectError>
```

### 12-5. Thumbnail Generation

When `cached_color` is present, a 256x256 thumbnail PNG is generated and stored in `thumbnails/preview.png`. The thumbnail is a nearest-neighbor downsample of the cached color map with sRGB conversion.

Module: `src/project.rs`

---

## 13. Foundation (Types & Utilities)

### 13-1. Color Types

```rust
pub struct Color {
    pub r: f32, pub g: f32, pub b: f32, pub a: f32,
}

pub struct HsvColor {
    pub h: f32, pub s: f32, pub v: f32,    // All [0, 1]
}
```

`Color` provides associated constants `WHITE` and `BLACK`, constructors `new(r, g, b, a)` and `rgb(r, g, b)`, and an `approx_eq()` method for testing.

A conversion helper is provided for bridging `LoadedTexture` pixel data to the internal color type:

```rust
pub fn pixels_to_colors(pixels: &[[f32; 4]]) -> Vec<Color>
```

### 13-2. Base Color Source

Groups the recurring `(texture, tex_width, tex_height, solid_color)` tuple passed through the compositing pipeline:

```rust
pub struct BaseColorSource<'a> {
    pub texture: Option<&'a [Color]>,
    pub tex_width: u32,
    pub tex_height: u32,
    pub solid_color: Color,
}
```

Constructors: `BaseColorSource::solid(color)` and `BaseColorSource::textured(data, width, height, fallback)`.

### 13-3. Stroke Path

```rust
pub struct StrokePath {
    pub points: Vec<Vec2>,
    pub layer_index: u32,
    pub stroke_id: u32,
    cumulative_lengths: Vec<f32>,    // Cached, one entry per point
    total_length: f32,               // Cached
}
```

Methods (all arc-length parameterized):
- `arc_length()` -- total path length (O(1), cached).
- `cumulative_lengths()` -- cached cumulative arc lengths.
- `midpoint()` -- point at the midpoint of the path by arc length.
- `sample(t)` -- position at parameter t (0.0 = start, 1.0 = end).
- `tangent(t)` -- normalized tangent direction at parameter t.

The `stroke_id` is a plain sequential counter assigned during path generation. No bit-encoding of layer information.

### 13-4. Stroke Appearance

Per-stroke appearance parameters for compositing:

```rust
pub struct StrokeAppearance {
    pub color: Color,
    pub id: u32,
    pub normal: Option<[f32; 3]>,
    pub transparent: bool,
}
```

### 13-5. Math Utilities

```rust
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32
pub fn lerp(a: f32, b: f32, t: f32) -> f32
pub fn rotate_vec2(v: Vec2, angle_rad: f32) -> Vec2
pub fn perpendicular(v: Vec2) -> Vec2              // 90 deg CCW
pub fn lerp_color(a: Color, b: Color, t: f32) -> Color
pub fn interpolate_array(arr: &[f32], index: f32) -> f32
```

**Convention**: `smoothstep(edge0, edge1, x)` follows the GLSL argument order. All call sites (including compositing opacity: `smoothstep(0.0, DENSITY_OPACITY_THRESHOLD, h)`) follow this convention.

**Convention**: `f32` to `usize` conversions use `value.round() as usize` for pixel counts (e.g., `brush_width.round() as usize`). Using `.floor()` or `.ceil()` instead causes subtle dimension mismatches between density maps and local frames.

Module: `src/math.rs`

### 13-6. Pressure Curves

```rust
pub fn evaluate_pressure(preset: PressurePreset, t: f32) -> f32
```

| Preset | Formula |
|--------|---------|
| Uniform | `p(t) = 1.0` |
| FadeOut | `p(t) = 1.0 - t^2` |
| FadeIn | `p(t) = t^0.5` |
| Bell | `p(t) = sin(pi * t)` |
| Taper | `p(t) = sin(pi * t)^0.5` |

Module: `src/pressure.rs`

### 13-7. Enums

**NormalMode**:

```rust
pub enum NormalMode {
    SurfacePaint,    // Height-only Sobel normals (original)
    DepictedForm,    // Object-space normals from mesh, perturbed by paint
}
```

Default: `DepictedForm`.

**BackgroundMode**:

```rust
pub enum BackgroundMode {
    Opaque,       // Strokes blend with base color/texture
    Transparent,  // Unpainted areas fully transparent, paint-only blending
}
```

Default: `Opaque`.

**ResolutionPreset**:

```rust
pub enum ResolutionPreset {
    Preview,    // 512
    Standard,   // 1024
    High,       // 2048
    Ultra,      // 4096
}
```

### 13-8. Seeded RNG

```rust
pub struct SeededRng {
    rng: ChaCha8Rng,    // Deterministic
}
```

Methods: `new(seed)`, `next_f32()`, `next_f32_range(min, max)`, `next_i32_range(min, max)`, `random_in_circle(radius)`.

Module: `src/rng.rs`

Module: `src/types.rs`

---

## 14. Architecture

### 14-1. Module Structure

```
src/
├── lib.rs
├── main.rs
├── error.rs              Error types (PainterError via thiserror)
├── asset_io.rs           S 11  Asset I/O
├── types.rs              S 13  Foundation types
├── math.rs               S 13  Math utilities
├── pressure.rs           S 13  Pressure curves
├── rng.rs                S 13  Seeded RNG
├── brush_profile.rs      S 7   Brush profile generation
├── stroke_height.rs      S 7   Stroke density map
├── direction_field.rs    S 4   Direction field
├── uv_mask.rs            S 5   UV mask for vertex groups
├── path_placement.rs     S 6   Path placement
├── stroke_color.rs       S 8   Stroke color
├── object_normal.rs      Mesh object-space normals
├── compositing.rs        S 9   Compositing
├── output.rs             S 10  Final output
├── glb_export.rs         S 11  GLB 3D preview export
├── project.rs            S 12  Project file
└── test_util.rs          (test only) Shared test helpers
```

Note: `local_frame.rs` is removed. `region.rs` is removed.

### 14-2. Dependency Graph

```
Asset I/O (S11) ────────────────────────────────────────┐
Foundation (S13) ──┬──> Stroke Height (S7)               │
                   ├──> Direction Field (S4)              │
                   ├──> UV Mask (S5)                      │
                   ├──> Stroke Color (S8)                 │
                   ├──> Object Normal                     │
                   ├──> Project File (S12)                │
                   │                                      │
                   │    Path Placement (S6) <── S4, S5    │
                   │         │                            │
                   │    Compositing (S9) <── S6, S7, S8   │
                   │         │                            │
                   │    Final Output (S10) <────── S9 ────┘
                   │    GLB Export (S11) <── S10, Asset I/O
                   │
                   └──> [GPU Pipeline] (deferred)
                   └──> [GUI] (deferred)
```

### 14-3. Key Design Constraints

1. **Stateless function interfaces**: All CPU modules have clean, stateless APIs enabling unit testing without GUI or GPU.
2. **Deterministic**: Same seed + same params = identical output (ChaCha8 PRNG).
3. **Linear float color space**: sRGB conversion only at I/O boundaries (load and export).

### 14-4. Tech Stack

| Role | Crate | Notes |
|------|-------|-------|
| Language | **Rust** | stable, edition 2021 |
| Math | **glam** 0.29 | vec2, vec3 operations (with serde feature) |
| RNG | **rand** 0.8 + **rand_chacha** 0.3 | Deterministic ChaCha8 PRNG |
| Noise | **noise** 0.9 | fBm (brush profile), 1D Perlin (wiggle only) |
| Serialization | **serde** + **serde_json** + **bincode** | JSON (metadata) / binary (cache) |
| Image I/O | **image** 0.25 | PNG, TGA read/write |
| EXR I/O | **exr** 1.7 | HDR height/color map output |
| OBJ Loading | **tobj** 4 | |
| glTF Loading | **gltf** 1.4 | Includes .glb binary |
| Archive | **zip** 2.1 | .pap project file |
| Error Handling | **thiserror** 2 | Derive macro for error types |
| Parallelism | **rayon** 1.10 | Parallel path generation and density map computation |

### 14-5. Build & Distribution

| Item | Spec |
|------|------|
| Minimum Rust Version | stable (edition 2021) |
| Target Platforms | Windows, macOS, Linux |
| Packaging | Single binary |

---

## 15. GPU Pipeline (Deferred)

The GPU pipeline is designed but deferred due to performance bottlenecks in the initial per-stroke submit architecture.

### 15-1. Architecture Overview

The GPU pipeline replaces the CPU compositing path using wgpu compute shaders (WGSL). All GPU computations target cross-platform Vulkan / Metal / DX12 via wgpu.

| Shader | Purpose |
|--------|---------|
| `direction_field.wgsl` | IDW interpolation on GPU |
| `streamline.wgsl` | Streamline tracing (parallel per seed) |
| `stroke_height.wgsl` | Per-stroke height generation |
| `composite_atomic.wgsl` | Compositing with atomicMax for height |
| `normal_map.wgsl` | Sobel normal generation |

### 15-2. Known Bottlenecks

1. **Per-stroke sequential submit**: 919 strokes x individual command submit = ~565ms overhead. Solved in design by batch dispatch.
2. **CPU prep bottleneck**: `build_local_frame()` at ~80ms + transform flatten/upload at ~32ms. Solved in design by GPU transform generation.
3. **Overlap filter** (CPU, ~200ms at 2048px): Spatial hash grid helps but remains the single largest bottleneck.

### 15-3. Designed Optimizations (Not Implemented)

- **Batch dispatch**: Flatten all stroke data into concatenated buffers, dispatch in 2 passes (stroke_height + composite). Removes wet-on-wet dependency.
- **GPU transform**: Port local frame transform to GPU compute shader. Replaces ~62MB CPU-to-GPU upload with ~360KB path points buffer.

---

## 16. GUI (Deferred)

The GUI is designed as a standalone egui + eframe application with wgpu-backed UV viewport, but deferred due to insufficient planning.

### 16-1. Planned Layout

```
+-----------------------------------------------------------------+
|  Menu Bar                                                       |
+------------+-------------------------------+--------------------+
|  Paint     |     UV Viewport               |   Properties       |
|  Slot      |     (main workspace)          |   Panel            |
|  List      |                               |                    |
+------------+-------------------------------+--------------------+
|  3D Preview (toggle)  | Height Map Preview | Status / Progress  |
+-----------------------------------------------------------------+
```

### 16-2. Planned Tool Modes

| Mode | Action |
|------|--------|
| Navigate | UV/3D view pan, zoom, rotate |
| Paint Slot Draw | Define paint slot boundaries on UV |
| Guide Place | Place direction guides (click+drag) |
| Guide Edit | Select, move, reorient, delete existing guides |
| Select | Select paint slots/guides |

---

## 17. Verification

### 17-1. Test Strategy

- **Unit tests**: All CPU modules include unit tests runnable with `cargo test`.
- **Visual tests**: Multiple modules produce PNG images for visual inspection (brush profile, stroke height, compositing, color variation, normal maps, GLB export).
- **Coverage test**: Path placement includes quantitative >= 90% area coverage test.
- **Benchmark tests**: 3 tests marked `#[ignore]` for `cargo test --release -- --ignored`.
- **Determinism**: All modules tested for identical output with same seed/params.

### 17-2. Test Count

246 tests total (243 pass + 3 ignored). Ignored tests are high-resolution visual benchmarks.

### 17-3. Key Test Scenarios

| Stage | Scenario | Expected |
|-------|----------|----------|
| Direction Field | Two guides 90 deg apart | Smooth rotation through 45 deg at midpoint |
| Direction Field | 180 deg opposing guides | No cancellation (same direction) |
| Path Placement | Default params, full UV | >= 90% area coverage |
| Path Placement | Boundary seeds (overscan) | Entry-edge strokes start at boundary |
| Path Placement | Poisson disk distribution | Blue-noise spacing, no clustering |
| Stroke Height | load=1.0, Uniform pressure | Max density approximately 1.0 |
| Stroke Height | Depletion check | Density at stroke end approximately 15% of start |
| Stroke Height | Pressure narrowing | Active width decreases with FadeOut |
| Stroke Height | Sobel gradients | Correct slope detection in X and Y |
| Compositing | 3 strokes at same position | Final height = max (not sum) |
| Compositing | Layer order | Higher order paints on top |
| Compositing | Zero height pixels | Base color preserved, stroke_id remains 0 |
| Compositing | High density | Full opacity, stroke color dominates |
| Compositing | Low density (bristle gap) | Underlying color shows through |
| Normal Map | Flat surface | All normals = (0.5, 0.5, 1.0) |
| Normal Map | Slope right | nx < 0.5 for positive gradient |
| Normal Map | Strength scaling | Higher strength = more tilt |
| Normal Map | Unit length | All decoded normals are unit length |
| Object Normal | Flat quad | All normals = +Z |
| Object Normal | TBN orthogonality | T, B, N mutually orthogonal |
| GLB Export | Roundtrip | File starts with glTF magic, parseable by gltf crate |
| Project | v3 round-trip | All fields preserved across save/load |
| Project | v2 migration | layers.json migrated to PaintSlot with __full_uv__ |
| Project | v1 migration | regions.json loaded and migrated |

---

## 18. Future Expansion

| Area | Expansion Direction |
|------|---------------------|
| Curvature Adaptation | Auto-adjust brush_width, spacing based on 3D curvature |
| Multiple Brushes | Mix brushes within paint slot (fan, round, palette knife) |
| Speed Modulation | Velocity map during streamline tracing, pressure/height coupling |
| Canvas Texture | Base heightmap with woven/linen pattern tiling |
| Manual Stroke Editing | Select, move, delete individual strokes |
| Specular Map | Derive roughness output from height map |
| Layers | Photoshop-style layers (dry between layers) |
| GPU Pipeline | Complete batch dispatch + GPU transform implementation |
| GUI Application | Full egui implementation per planned spec |

---

## Appendix A. Stroke Height Generation Interface

```
generate_stroke_height(
    brush_profile:    &[f32]         Brush profile
    stroke_length_px: usize          Stroke length in pixels
    params:           &StrokeParams  Stroke parameters
    seed:             u32            Noise seed
) -> StrokeHeightResult {
    data:    Vec<f32>    Density map (0.0-1.0), row-major
    width:   usize       stroke_length_px
    height:  usize       brush_width_px
}
```

The `StrokeParams` struct supplies all per-stroke parameters: `brush_width`, `load`, `body_wiggle`, `pressure_preset`. No separate `base_height`, `ridge_height`, or `ridge_width` parameters -- the density model produces values directly from brush profile, pressure, and paint depletion.

Pre-computed Sobel gradients for each stroke are computed immediately after generation:

```
compute_stroke_gradients(
    height: &StrokeHeightResult
) -> StrokeGradientResult {
    gx:     Vec<f32>    Gradient in local-X (along stroke), row-major
    gy:     Vec<f32>    Gradient in local-Y (across stroke), row-major
    width:  usize
    height: usize
}
```

---

## Appendix B. Constants Reference

| Constant | Value | Module | Description |
|----------|-------|--------|-------------|
| `FBM_FREQ` | 0.3 | brush_profile | Bristle fBm base frequency |
| `FBM_LACUNARITY` | 2.0 | brush_profile | fBm frequency multiplier |
| `FBM_GAIN` | 0.5 | brush_profile | fBm amplitude decay |
| `FBM_OCTAVES` | 4 | brush_profile | fBm octave count |
| `GAP_DENSITY` | 15 | brush_profile | Average pixels per bristle gap |
| `MIN_WIDTH_RATIO` | 0.3 | stroke_height | Min brush width at pressure 0 |
| `DEPLETION_FLOOR` | 0.15 | stroke_height | Paint remaining at stroke end |
| `DEPLETION_EXPONENT` | 0.7 | stroke_height | Depletion curve shape |
| `DENSITY_OPACITY_THRESHOLD` | 0.7 | compositing | Density at which stroke becomes fully opaque |
| `SEED_EXPANSION` | 2.0 | path_placement | Boundary seed expansion factor |
| `SEEK_MAX_MULTIPLIER` | 1.5 | path_placement | Max seek distance = expansion x spacing x 1.5 |
| `OVERLAP_THRESHOLD_DIST` | 0.3 | path_placement | Overlap distance threshold (brush_width multiples) |
| `OVERLAP_RATIO` | 0.7 | path_placement | Overlap ratio to discard path |
| `MIN_LENGTH_FACTOR` | 2.0 | path_placement | Minimum path length (brush_width multiples) |
| `JITTER_FACTOR` | 0.2 | path_placement | Seed jitter amount (spacing multiples) |
| `IDW_EPSILON` | 0.001 | direction_field | Singularity prevention |

---

## Appendix C. Known Issues

From code review (2026-02-23), the following issues remain unresolved:

1. **Bilinear sampling duplicated in 3 locations.** `compositing.rs` (`bilinear_sample` for `&[f32]`), `stroke_color.rs` (`sample_bilinear` for `&[Color]`), and `glb_export.rs` (`sample_map_bilinear` for `&[f32]`). The two `f32` versions are near-identical and should be consolidated.

2. **StrokeParams lacks input validation.** All fields are unvalidated after deserialization. Negative `brush_width` causes negative spacing which can trigger infinite loops in seed generation. Zero `stroke_spacing` similarly causes infinite loops. Values like `load > 1.0` or negative `max_stroke_length` produce nonsensical results. Since `.pap` project files are external input, post-deserialization validation is required.

3. **Transparent mode alpha/color blending inconsistency.** In over-paint scenarios, low-opacity strokes modify RGB channels via lerp but alpha is set to `max(prev_alpha, opacity)`. This means a thin paint stroke on top of a thick one changes the color but not the transparency, which can produce non-physical results.

4. **Path cache has no version stamp.** `PathCacheKey` validates resolution, slot parameters, and color texture path, but has no mechanism to detect code-level changes (e.g., modifications to the streamline tracing algorithm). Stale cached paths persist across code updates.

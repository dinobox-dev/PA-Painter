# Practical Arcana Painter — Implementation Specification

## 1. Overview

### 1-1. Purpose

This document defines the full pipeline for a tool that takes a 3D mesh, a color texture, and direction guide vertices as input to generate natural hand-painted style textures. It consolidates all implementation phase specifications into a single reference.

### 1-2. Deliverables

**Final Outputs**:

| Output | Format | Description |
|--------|--------|-------------|
| Color Map | PNG/EXR (sRGB) | Color texture with brush stroke artifacts |
| Height Map | PNG/EXR (Linear) | Impasto height information (subtle convex brush texture) |
| Normal Map | PNG (Linear) | Tangent-space normals derived from Height Map |
| Stroke ID Map | PNG (Linear, optional) | Per-pixel stroke identification (debug/masking) |

### 1-3. Pipeline

```
Input
  ├─ 3D Mesh (.obj, .glTF)
  ├─ Color Texture (base color map)
  └─ User Edits
       ├─ Region definitions (polygon masks on UV)
       └─ Direction guide vertices (per-region stroke direction + params)
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
 │    Direction field + region mask      │
 │    → seed distribution → streamline  │
 │    → path list                       │
 └──────────────┬───────────────────────┘
                │
                ▼
 ┌──────────────────────────────────────┐
 │  Stage 3. Per-Stroke Height Gen      │
 │    Map path to local straight frame  │
 │    → brush model + pressure curve    │
 │    → individual height profiles      │
 │    → local height map + color        │
 └──────────────┬───────────────────────┘
                │
                ▼
 ┌──────────────────────────────────────┐
 │  Stage 4. Compositing                │
 │    Composite into UV space in order  │
 │    → height max (tallest wins)       │
 │    → color blending                  │
 └──────────────┬───────────────────────┘
                │
                ▼
 ┌──────────────────────────────────────┐
 │  Stage 5. Final Texture Output       │
 │    Height → Normal Map conversion    │
 │    Color + Height + Normal export    │
 └──────────────────────────────────────┘
```

### 1-4. Core Design Principles

**Deterministic Reproduction**: Identical region parameters and seed always produce identical results. All randomness derives from a seed-based PRNG (ChaCha8).

**No Height Stacking**: Height does not accumulate between strokes. At each pixel the maximum height across all strokes is kept (tallest wins). The final height map preserves the most prominent paint relief, producing natural impasto texture.

**Region Independence**: Each region has its own stroke parameter set. Strokes are clipped at region boundaries, and new strokes composite on top of existing height maps from adjacent regions.

**Non-Destructive Editing**: All strokes are stored as individual records. When parameters change, only the affected region's strokes are regenerated. The original color texture is never modified.

### 1-5. Implementation Status

| Area | Status | Notes |
|------|--------|-------|
| CPU Pipeline (Stages 1–5) | **Complete** | All modules implemented and tested |
| Asset I/O | **Complete** | OBJ/glTF/glb mesh, PNG/TGA/EXR texture |
| Project File (.pap) | **Complete** | Zip-based save/load |
| GPU Pipeline | **Deferred** | Performance bottleneck in per-stroke submit and CPU prep; batch dispatch and GPU transform optimizations designed but not fully validated |
| GUI | **Deferred** | Planning insufficient for current phase; spec exists but not implemented |

---

## 2. Input Data

### 2-1. 3D Mesh

| Item | Requirement |
|------|-------------|
| Format | .obj, .glTF/.glb (triangle/quad mesh) |
| UV | Non-overlapping single UV channel required (0–1 normalized) |
| Usage | UV unwrap visualization, curvature-based auto-parameters (future), 3D preview |

The tool performs all computations in UV space, so the mesh's 3D geometry is only used for preview and future curvature analysis.

### 2-2. Color Texture (Base Color Map)

| Item | Spec |
|------|------|
| Format | PNG, TGA, EXR |
| Color Space | sRGB (PNG/TGA — converted to linear on load), Linear (EXR — used as-is) |
| Resolution | Unrestricted (independent of output resolution) |
| Required | **Optional**. If not provided, a solid color is used (user-specified, default white) |
| Usage | Determines each stroke's base color. Samples the texel at the stroke path midpoint |

### 2-3. Output Resolution

User-specified, independent of input texture resolution. All internal computations run in UV space at this resolution.

| Preset | Resolution | Use |
|--------|-----------|-----|
| Preview | 512 × 512 | Fast iteration |
| Standard | 1024 × 1024 | General assets |
| High | 2048 × 2048 | High-resolution assets |
| Ultra | 4096 × 4096 | Close-up / cinematic |

---

## 3. Region

### 3-1. Role of Regions

A region is a UV-space area where the same stroke style applies. For example, a character's skin, armor, and cloth may require different brush sizes, impasto intensity, and direction patterns, so they are separated into distinct regions.

### 3-2. Region Definition

The user draws polygons on the UV unwrap to define regions. Region masks may overlap; in overlapping areas, strokes from the higher `order` (painted later) region overwrite those of the lower.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    pub id: u32,
    pub name: String,
    pub mask: Vec<Polygon>,          // Union of polygons
    pub order: i32,                  // Render order (lower = painted first)
    pub params: StrokeParams,
    pub guides: Vec<GuideVertex>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Polygon {
    pub vertices: Vec<Vec2>,  // Closed polygon (last connects to first)
}
```

### 3-3. Per-Region Stroke Parameters

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrokeParams {
    pub brush_width: f32,        // 4.0 – 120.0 (UV px)
    pub load: f32,               // 0.0 – 1.0
    pub base_height: f32,        // 0.0 – 1.0
    pub ridge_height: f32,       // 0.0 – 1.0
    pub ridge_width: f32,        // 2.0 – 15.0 (px)
    pub ridge_variation: f32,    // 0.0 – 0.2
    pub body_wiggle: f32,        // 0.0 – 0.5 (brush width multiples)
    pub stroke_spacing: f32,     // 0.3 – 2.0 (brush width multiples)
    pub pressure_preset: PressurePreset,
    pub color_variation: f32,    // 0.0 – 0.3
    pub max_stroke_length: f32,  // Maximum stroke length in pixels (default 240.0)
    pub angle_variation: f32,    // 0.0 – 15.0 (degrees)
    pub max_turn_angle: f32,     // 5.0 – 45.0 (degrees)
    pub seed: u32,
}
```

| Parameter | Default | Description |
|-----------|---------|-------------|
| `brush_width` | 30.0 | Brush width in UV pixels |
| `load` | 0.8 | Paint amount. 1.0 = enough for a clean stroke; low = dry brush |
| `base_height` | 0.5 | Base height of the stroke body |
| `ridge_height` | 0.3 | Impasto ridge height. Added on top of base_height |
| `ridge_width` | 5.0 | Ridge slope width in pixels |
| `ridge_variation` | 0.1 | Per-stroke ridge variation. 0 = all identical; higher = subtly different ridges per stroke |
| `body_wiggle` | 0.15 | Lateral body sway amplitude (brush width multiples). Low-freq Perlin noise shifts the active region per-column, simulating hand tremor |
| `stroke_spacing` | 1.0 | Spacing between adjacent strokes (brush width multiples) |
| `pressure_preset` | FadeOut | Pressure curve preset |
| `color_variation` | 0.1 | Per-stroke color deviation (HSV) |
| `max_stroke_length` | 240.0 | Maximum stroke length in pixels. Stroke lengths follow a power distribution biased toward longer strokes |
| `angle_variation` | 5.0 | Stroke direction random deviation (degrees) |
| `max_turn_angle` | 15.0 | Max allowed rotation between consecutive steps. Path terminates if exceeded |
| `seed` | 42 | Random seed for this region (reproducibility) |

**Pressure Curve Presets**:

```rust
pub enum PressurePreset {
    Uniform,   // p(t) = 1.0
    FadeOut,   // p(t) = 1.0 - t²         (most natural default)
    FadeIn,    // p(t) = t^0.5
    Bell,      // p(t) = sin(π × t)
    Taper,     // p(t) = sin(π × t)^0.5
}
```

---

## 4. Direction Field (Stage 1)

### 4-1. Guide Vertices

Direction hint points placed by the user on the UV unwrap. Each vertex has a position and direction vector.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuideVertex {
    pub position: Vec2,    // UV coordinate (0–1)
    pub direction: Vec2,   // Normalized direction vector
    pub influence: f32,    // Influence radius in UV units (default 0.2)
}
```

### 4-2. Direction Field Interpolation

Generates a continuous direction field across the entire region from guide vertices. The field returns a stroke direction at any UV coordinate.

**Algorithm: IDW with Canonicalized Reference Alignment**

Direction vectors are headless — a stroke going left-to-right is the same as right-to-left. This creates a 180° symmetry that must be handled carefully during interpolation.

**Why not doubled-angle (2θ) circular mean?** Guides differing by exactly 90° map to antipodal points in 2θ space (0° → 0°, 90° → 180°). Since `sin(0) = sin(π) = 0`, the sin-component is always zero regardless of weights, making the blend between 0° and 90° jump discontinuously instead of passing through 45°. This is a common edge case (horizontal/vertical guide pairs).

```
direction_at(uv, guides):
    If no guide vertices:
        return (1, 0)                      # Default horizontal

    If only 1 guide vertex:
        return normalize(guide[0].direction)

    EPSILON = 0.001

    # Canonicalize: map direction to upper half-plane
    canonicalize(d):
        if d.y < 0 or (d.y == 0 and d.x < 0): return -d
        else: return d

    # Collect (weight, canonicalized direction) for guides within influence
    weighted = []
    for each guide g:
        d = distance(uv, g.position)
        if d > g.influence: continue
        w = 1 / (d + EPSILON)²
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
        sum += w × dir

    return normalize(sum)
```

**Key Properties**:
1. 180° symmetry: `(1, 0)` and `(-1, 0)` both canonicalize to `(1, 0)`, so they never cancel
2. Smooth 90° interpolation: Unlike the 2θ approach, blending passes smoothly through 45°
3. IDW falloff: Weight decreases as 1/d²
4. Nearest-neighbor fallback: Points outside all influence radii get the nearest guide's direction

### 4-3. Direction Field Generation

```rust
/// Compute direction at a UV coordinate given guide vertices.
pub fn direction_at(uv: Vec2, guides: &[GuideVertex]) -> Vec2

/// Generate a full direction field texture (resolution × resolution, row-major).
pub fn generate_direction_field(guides: &[GuideVertex], resolution: u32) -> Vec<Vec2>
```

Module: `src/direction_field.rs`

---

## 5. Region Mask & Polygon Operations

### 5-1. Point-in-Polygon (Ray Casting)

Standard ray casting algorithm. Cast a horizontal ray from the query point to +X infinity and count edge crossings. Odd count = inside.

```
point_in_polygon(point, polygon):
    inside = false
    n = polygon.vertices.len()
    j = n - 1
    for i in 0..n:
        vi = polygon.vertices[i]
        vj = polygon.vertices[j]
        if (vi.y > point.y) != (vj.y > point.y):
            x_intersect = vj.x + (point.y - vj.y) / (vi.y - vj.y) * (vi.x - vj.x)
            if point.x < x_intersect:
                inside = !inside
        j = i
    return inside
```

### 5-2. Point-in-Region

A point is inside a region if it's inside ANY polygon in the mask (union of polygons).

### 5-3. Region Bounding Box

```rust
pub struct BBox {
    pub min: Vec2,
    pub max: Vec2,
}

pub fn region_bbox(region: &Region) -> BBox
```

### 5-4. Mask Rasterization

Rasterize a region mask to a boolean grid at a given resolution for O(1) per-pixel lookup.

```rust
pub struct RasterMask {
    pub data: Vec<bool>,
    pub width: u32,
    pub height: u32,
}

pub fn rasterize_mask(region: &Region, resolution: u32) -> RasterMask
```

`RasterMask` provides `contains(uv: Vec2) -> bool` and `contains_px(x: i32, y: i32) -> bool` methods.

Module: `src/region.rs`

---

## 6. Stroke Path Placement (Stage 2)

### 6-1. Seed Point Distribution

Distribute starting points on a uniform grid within an expanded bounding box, then apply jitter.

**Boundary Seed Expansion**: The seed grid extends beyond the region bounding box by `SEED_EXPANSION * spacing` (default `SEED_EXPANSION = 2.0`). Seeds outside the region mask trace forward along the direction field ("seek phase") until they enter the region, producing strokes that start right at the boundary and fill edge gaps.

```
generate_seeds(bbox, mask, params):
    spacing = brush_width / mask.width × stroke_spacing
    jitter_amount = spacing × 0.2

    # Expand bounding box, clamp to UV space
    expansion = spacing × SEED_EXPANSION
    exp_min = max(bbox.min - expansion, 0.0)
    exp_max = min(bbox.max + expansion, 1.0)

    seeds = []
    for each grid point in expanded bbox:
        pos = grid_point + random_in_circle(jitter_amount)
        if pos within UV [0,1]: seeds.append(pos)
    return seeds
```

### 6-2. Streamline Tracing

Trace a path from each seed point following the direction field.

**Stroke Length Distribution**: Target length uses a single-RNG-call power distribution:

```
max_length_uv = max_stroke_length / resolution
target_length = max_length_uv × √(U),   U ~ Uniform(0,1)
```

This produces naturally varied lengths biased toward longer strokes, with a smooth tail of shorter ones. Median ≈ `max_stroke_length × 0.707`.

**Seek Phase** (for outside seeds): If the seed is outside the mask, trace forward deterministically along the direction field until entering the region. No angle variation or curvature limit during seek. Path recording starts from the mask entry point.

**Normal Tracing Phase**: Follow the direction field with angle variation, curvature limit, and boundary checks.

```
trace_streamline(seed, guides, mask, params, resolution, rng):
    step_size_uv = 1.0 / resolution

    # Target length: consume RNG FIRST (before seek) to ensure
    # identical RNG sequence regardless of whether seek phase runs.
    max_length_uv = params.max_stroke_length / resolution
    target_length = max_length_uv × √(rng.next_f32())

    pos = seed
    prev_dir = direction_at(pos, guides)

    # ── Seek phase (outside seeds only) ──────────────────────
    if !mask.contains(pos):
        spacing = brush_width / resolution × stroke_spacing
        max_seek = SEED_EXPANSION × spacing × 1.5   # diagonal tolerance
        seek_length = 0.0

        while seek_length < max_seek:
            dir = direction_at(pos, guides)

            # Direction alignment (critical correctness fix):
            # direction_at() returns headless 180°-symmetric vectors.
            # Without aligning to prev_dir, the path reverses direction
            # mid-trace, producing zigzag paths instead of smooth strokes.
            if dir.dot(prev_dir) < 0: dir = -dir
            dir = dir.normalize()
            if dir == ZERO: return None

            next_pos = pos + dir × step_size_uv
            if next_pos outside UV [0,1]: return None

            prev_dir = dir
            pos = next_pos
            seek_length += step_size_uv

            if mask.contains(pos): break

        if !mask.contains(pos): return None   # Failed to enter region

    # ── Normal tracing phase ─────────────────────────────────
    path = [pos]
    length = 0.0

    while length < target_length:
        dir = direction_at(pos, guides)

        # Direction alignment (same as seek phase)
        if dir.dot(prev_dir) < 0: dir = -dir

        # Apply angle deviation (gradual)
        angle_offset = (rng.next_f32() - 0.5) × angle_variation_rad × 2
        dir = rotate(dir, angle_offset × 0.1)
        dir = dir.normalize()

        # Curvature limit
        turn = acos(clamp(prev_dir.dot(dir), -1, 1))
        if turn > max_turn_angle_rad: break

        next_pos = pos + dir × step_size_uv
        if !mask.contains(next_pos): break
        if next_pos outside UV [0,1]: break

        path.push(next_pos)
        prev_dir = dir
        pos = next_pos
        length += step_size_uv

    # Minimum length filter
    if length < brush_width / resolution × 2: return None
    return path
```

**Direction Alignment** (correctness-critical): `direction_at()` returns headless (180°-symmetric) vectors. Without `if dir.dot(prev_dir) < 0 { dir = -dir }`, the streamline can reverse direction mid-trace, producing zigzag paths instead of smooth strokes. This alignment is applied in both seek and normal tracing phases.

### 6-3. Path Quality Filter

Paths shorter than `brush_width × 2` are removed. Paths excessively overlapping existing paths (≥ 70% of points within `brush_width × 0.3` of an existing centerline) are also removed.

### 6-4. Full Pipeline

```rust
/// Generate all stroke paths for a region.
/// Returns paths in paint order (sorted by seed y-coordinate, top to bottom).
pub fn generate_paths(region: &Region, resolution: u32) -> Vec<StrokePath>
```

Stroke IDs are encoded as `(region_id << 16) | stroke_index` for cross-region uniqueness.

**Coverage Target**: ≥ 90% of region area covered with default parameters.

Module: `src/path_placement.rs`

### 6-5. Constants

| Name | Value | Description |
|------|-------|-------------|
| `SEED_EXPANSION` | 2.0 | Grid expansion in units of spacing |
| Max seek multiplier | 1.5 | Safety margin on seek distance |

---

## 7. Local Frame Mapping

### 7-1. Concept

Paths from Stage 2 are curves in UV space. The height generation module operates on a straight local frame. This module builds a transform table mapping every pixel in the local frame to a UV coordinate.

```
Local frame (straight)        UV space (curved path)
┌──────────────────┐         ╭───╮
│ margin | body    │  ←→    ╱     ╲
│        | region  │       ╱       ╲
└──────────────────┘      ╱         ╲
```

### 7-2. Local Frame Layout

```
x-axis: [0, margin + stroke_length)
         [0, margin): front ridge zone
         [margin, margin + stroke_length): body zone

y-axis: [0, margin + brush_width + margin)
         [0, margin): top side ridge zone
         [margin, margin + brush_width): body zone
         [margin + brush_width, ...): bottom side ridge zone
```

Where `margin = ridge_width_px`.

### 7-3. Stroke Length Computation (Critical Sync Point)

The `stroke_length_px` MUST be computed identically across local frame (§7), stroke height (§8), and compositing (§10):

```rust
let stroke_length_px = (path.arc_length() * resolution as f32).ceil() as usize;
```

**Buffer overrun margin**: When allocating the stroke height buffer, add `+2` to `stroke_length_px` to account for tracing loop step overshoot. This applies in both `render_all_optimized` and `render_all_pooled` paths. Without this margin, long strokes at high resolution can write past the buffer end.

### 7-4. API

```rust
pub struct LocalFrameTransform {
    pub uv_map: Vec<Vec2>,    // UV coordinate for each local pixel (NAN = out of bounds)
    pub width: usize,          // stroke_length_px + margin
    pub height: usize,         // brush_width_px + margin * 2
    pub margin: usize,         // ridge_width_px
}

pub fn build_local_frame(
    path: &StrokePath,
    brush_width_px: usize,
    ridge_width_px: usize,
    resolution: u32,
) -> LocalFrameTransform
```

### 7-5. Algorithm

For each local pixel `(lx, ly)`:
1. Map `lx` to path parameter `t = (lx - margin) / stroke_length_px`
2. Sample `center = path.sample(t.clamp(0, 1))` and `tangent = path.tangent(t.clamp(0, 1))`
3. Compute `normal = perpendicular(tangent)` (90° CCW)
4. Apply perpendicular offset: `uv = center + normal × offset_uv`
5. Handle front ridge zone (t < 0): backtrack along reverse tangent

Module: `src/local_frame.rs`

---

## 8. Stroke Height Generation (Stage 3)

### 8-1. Brush Profile Generation

A 1D density array representing the bristle pattern across the brush width.

**Step 1: fBm-based bristle pattern**

```
for j in 0..width:
    density[j] = fbm_4octaves(j × 0.3, seed)
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
- **1D Perlin for ridge variation**: Use `Perlin::new(seed)` and evaluate at `[x as f64 / stroke_length_px as f64, 0.0]`. For width noise, use `[x / stroke_length_px, 1000.0]` to decorrelate from height noise.
- **1D Perlin for body wiggle**: Use `Perlin::new(seed + 2)` and evaluate at `[t as f64, 0.0]` to decorrelate from ridge noise.
- All noise functions return `f64`; cast to `f32` after evaluation.

Module: `src/brush_profile.rs`

### 8-2. Stroke Height Map Generation

```rust
pub struct StrokeHeightResult {
    pub data: Vec<f32>,      // Height map, row-major: data[y * width + x]
    pub width: usize,         // stroke_length_px + ridge_width_px
    pub height: usize,        // brush_width_px + ridge_width_px * 2
    pub margin: usize,        // ridge_width_px
}

pub fn generate_stroke_height(
    brush_profile: &[f32],
    brush_width_px: usize,
    stroke_length_px: usize,
    load: f32,
    base_height: f32,
    ridge_height: f32,
    ridge_width_px: usize,
    ridge_variation: f32,
    body_wiggle: f32,
    pressure_preset: PressurePreset,
    seed: u32,
) -> StrokeHeightResult
```

#### Step 1: Body Height

For each column x in `[0, stroke_length_px)`:

```
t = x / stroke_length_px
p = evaluate_pressure(pressure_preset, t)

# Effective width from pressure
active_width = brush_width_px × (MIN_WIDTH_RATIO + (1.0 - MIN_WIDTH_RATIO) × p)

# Body wiggle: low-freq Perlin noise shifts center laterally
wiggle_offset = perlin_1d(t, seed + 2) × body_wiggle × brush_width_px

# Paint depletion
remaining = load × lerp(1.0, DEPLETION_FLOOR, t^DEPLETION_EXPONENT)

# Resample brush profile into active width, compute effective density
for j in active range:
    source_idx = j × (brush_width_px / active_count)
    rd = interpolate_array(brush_profile, source_idx)
    effective_density = p^(5 × (1 - rd) + 1)
    body[y][x] = effective_density × remaining × base_height
```

| Constant | Value | Description |
|----------|-------|-------------|
| `MIN_WIDTH_RATIO` | 0.3 | Minimum brush width ratio at pressure 0 |
| `DEPLETION_FLOOR` | 0.15 | Remaining paint ratio at stroke end |
| `DEPLETION_EXPONENT` | 0.7 | Depletion curve shape |

#### Step 2: Impasto Ridge

Reproduces the phenomenon in oil painting where the brush pushes paint to form ridges at the front and both sides.

**Ridge profile function**: Smooth inverse smoothstep falloff.

```rust
ridge_profile(d, w) = (1 - d/w)² × (1 + 2 × d/w)    // 1.0 at d=0, 0.0 at d=w
```

**Ridge noise** (per-stroke variation): 1D Perlin noise with frequency `RIDGE_NOISE_FREQ = 1.0`.

```
effective_ridge_height(x) = ridge_height × (1 + noise_h(x) × ridge_variation)
effective_ridge_width(x)  = ridge_width × (1 + noise_w(x) × ridge_variation × 0.5)
```

**Side ridges**: Placed outside the top/bottom boundaries of the active range. Ridge peaks overlap with body boundary (d=0), matching physical impasto.

**Front ridge**: Placed before the stroke start point. Matches active width at x=0 for smooth join with side ridges.

**Final**: `local_height_map = body + ridge`

### 8-3. Visual Effect Reference

| Parameters | Visual Effect |
|------------|--------------|
| base_height=0.5, ridge_height=0 | Flat stroke, no impasto |
| base_height=0.3, ridge_height=0.5 | Prominent edge ridges, low body |
| base_height=0.5, ridge_height=0.3, ridge_width=3 | Narrow, sharp ridges |
| base_height=0.5, ridge_height=0.3, ridge_width=10 | Wide, gentle ridges |
| ridge_variation=0 | Ridge height/width uniform (mechanical) |
| ridge_variation=0.15 | Subtly different ridges per stroke |
| load=0.3 | Overall lower, ridges weaker. Nearly gone at stroke end |
| body_wiggle=0.3 | Lateral sway in stroke body, simulating hand tremor |

Module: `src/stroke_height.rs`

---

## 9. Stroke Color

### 9-1. Base Color Sampling

Each stroke's base color is sampled from the Base Color Map at the path midpoint using bilinear interpolation.

```rust
pub fn sample_bilinear(
    texture: &[Color], tex_width: u32, tex_height: u32, uv: Vec2,
) -> Color
```

### 9-2. Color Variation

Subtle per-stroke color shifts in HSV space for a natural hand-painted feel.

```
hsv = rgb_to_hsv(stroke_base_color)
hsv.h += (random - 0.5) × color_variation × 0.5
hsv.s += (random - 0.5) × color_variation
hsv.v += (random - 0.5) × color_variation × 0.7
stroke_color = hsv_to_rgb(clamp(hsv))
```

### 9-3. Intra-Stroke Color

Color is uniform within a single stroke — a key characteristic of hand-painted style.

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

Module: `src/stroke_color.rs`

---

## 10. Compositing (Stage 4)

### 10-1. Global Maps

```rust
pub struct GlobalMaps {
    pub height: Vec<f32>,          // 0.0 = no paint
    pub color: Vec<Color>,
    pub stroke_id: Vec<u32>,       // 0 = no stroke
    pub resolution: u32,
}
```

**Initialization**: `height` = zeros, `stroke_id` = zeros, `color` = resampled base color texture (or solid color).

### 10-2. Compositing Order

Strokes are composited in region order (ascending `order` value) → row order within each region (sorted by seed y-coordinate, top-to-bottom).

### 10-3. Single Stroke Compositing

```
composite_stroke(local_height, transform, stroke_color, stroke_id, base_height, global):
    for each local pixel (ly, lx):
        h = local_height[ly][lx]
        if h <= 0: continue

        uv = transform.local_to_uv(lx, ly)
        if uv is None: continue

        (px, py) = uv_to_pixel(uv, resolution)
        if out of bounds: continue

        # Height: MAX (tallest wins, no accumulation)
        global.height[idx] = max(h, global.height[idx])

        # Color: height-based opacity blending
        opacity = smoothstep(h, 0.0, base_height × 0.7)
        global.color[idx] = lerp(global.color[idx], stroke_color, opacity)

        # Stroke ID: record last stroke
        global.stroke_id[idx] = stroke_id
```

**Color opacity rules**:
- Body (h ≈ base_height): opacity ≈ 1.0, full cover
- Dry brush (low h): low opacity, underlying color shows through
- Bristle gaps (h ≈ 0): opacity ≈ 0, underlying exposed
- Ridge (h > base_height): opacity saturates to 1.0

### 10-4. Full Pipeline

```rust
pub fn composite_all(
    regions: &[Region],
    resolution: u32,
    base_color_texture: Option<&[Color]>,
    tex_width: u32, tex_height: u32,
    solid_color: Color,
    settings: &OutputSettings,
) -> GlobalMaps
```

For each region (sorted by order):
1. Generate brush profile (once per region, same seed for all strokes)
2. Generate paths via `generate_paths()`
3. For each stroke path:
   - Build local frame (`build_local_frame()`)
   - Generate height map (`generate_stroke_height()`, seed = `region.seed + stroke_index`)
   - Compute stroke color (`compute_stroke_color()`)
   - Composite into global maps

### 10-5. Single-Region Compositing

```rust
pub fn composite_region(
    region: &Region,
    resolution: u32,
    global: &mut GlobalMaps,
    settings: &OutputSettings,
    base_color_texture: Option<&[Color]>,
    tex_width: u32, tex_height: u32,
    solid_color: Color,
)
```

Extracted inner loop for single-region preview regeneration.

Module: `src/compositing.rs`

---

## 11. Final Texture Output (Stage 5)

### 11-1. Height Map Normalization

```
max_possible = max(base_height + ridge_height) across all regions
display_cap = 2 × max_possible
normalized = clamp(global_height / display_cap, 0, 1)
```

### 11-2. Normal Map Generation (Sobel Filter)

Tangent-space normals via 3×3 Sobel kernels. Flat = (0.5, 0.5, 1.0).

```
gx = sobel_x(normalized_height)
gy = sobel_y(normalized_height)
N = normalize(-gx × normal_strength, -gy × normal_strength, 1)
```

`normal_strength` (default 1.0) controls visual depth of impasto.

### 11-3. Export Functions

| Map | PNG Export | EXR Export |
|-----|-----------|------------|
| Color Map | sRGB (apply `linear_to_srgb()`) | Linear float |
| Height Map | Linear grayscale (no gamma) | Linear float |
| Normal Map | Linear RGB. Flat = (128, 128, 255) | N/A (PNG only) |
| Stroke ID Map | Grayscale PNG | N/A (PNG only) |

```rust
pub fn export_all(
    global: &GlobalMaps,
    regions: &[Region],
    settings: &OutputSettings,
    output_dir: &Path,
    format: ExportFormat,
) -> Result<(), OutputError>
```

Module: `src/output.rs`

### 11-4. Global Output Settings

```rust
pub struct OutputSettings {
    pub resolution_preset: ResolutionPreset,
    pub output_resolution: u32,
    pub normal_strength: f32,        // 0.1 – 5.0 (default 1.0)
}
```

---

## 12. Asset I/O

### 12-1. Mesh Loading

```rust
pub struct LoadedMesh {
    pub positions: Vec<Vec3>,
    pub uvs: Vec<Vec2>,
    pub indices: Vec<u32>,       // Always triangulated
}

/// Load a mesh from .obj, .gltf, or .glb.
/// Quads are automatically triangulated (split along shortest diagonal).
pub fn load_mesh(path: &Path) -> Result<LoadedMesh, MeshError>
```

### 12-2. Texture Loading

```rust
pub struct LoadedTexture {
    pub pixels: Vec<[f32; 4]>,   // Linear float RGBA [0, 1]
    pub width: u32,
    pub height: u32,
}

/// Load a color texture (PNG/TGA → sRGB to linear, EXR → pass-through).
pub fn load_texture(path: &Path) -> Result<LoadedTexture, TextureError>
```

### 12-3. Color Space Conversion

```rust
pub fn srgb_to_linear(s: f32) -> f32    // Input loading
pub fn linear_to_srgb(l: f32) -> f32    // Output export
```

### 12-4. UV Edge Extraction

```rust
/// Extract UV-space edges for wireframe visualization (deduplicated).
pub fn extract_uv_edges(mesh: &LoadedMesh) -> Vec<(Vec2, Vec2)>
```

Module: `src/asset_io.rs`

---

## 13. Project File (.pap)

### 13-1. Storage Format

Projects are saved as `.pap` files — zip archives with renamed extension.

```
project.pap (zip)
├── manifest.json         # Version, metadata
├── mesh_ref.json         # External mesh file path
├── color_ref.json        # External color texture path
├── regions.json          # All regions with parameters and guides
├── settings.json         # Global output settings
├── cache/
│   ├── height_map.bin    # Last generated height map (bincode, optional)
│   └── color_map.bin     # Last generated color map (bincode, optional)
└── thumbnails/
    └── preview.png       # 256×256 project thumbnail (optional)
```

### 13-2. Data Structures

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
    pub regions: Vec<Region>,
    pub settings: OutputSettings,
    pub cached_height: Option<Vec<f32>>,
    pub cached_color: Option<Vec<[f32; 4]>>,
}
```

### 13-3. Non-Destructive Principle

The project file does not modify input data (mesh, color texture); only external references are stored. All edit state is fully contained within the project file, so identical results can be reproduced with the same inputs.

```rust
pub fn save_project(project: &Project, path: &Path) -> Result<(), ProjectError>
pub fn load_project(path: &Path) -> Result<Project, ProjectError>
```

Module: `src/project.rs`

---

## 14. Foundation (Types & Utilities)

### 14-1. Color Types

```rust
#[repr(C)]
pub struct Color {
    pub r: f32, pub g: f32, pub b: f32, pub a: f32,
}

pub struct HsvColor {
    pub h: f32, pub s: f32, pub v: f32,    // All [0, 1]
}
```

### 14-2. Stroke Path

```rust
pub struct StrokePath {
    pub points: Vec<Vec2>,
    pub region_id: u32,
    pub stroke_id: u32,
}
```

Methods: `arc_length()`, `midpoint()`, `sample(t)`, `tangent(t)` — all arc-length parameterized.

### 14-3. Math Utilities

```rust
pub fn smoothstep(x: f32, edge0: f32, edge1: f32) -> f32  // Note: value-first arg order
pub fn lerp(a: f32, b: f32, t: f32) -> f32
pub fn rotate_vec2(v: Vec2, angle_rad: f32) -> Vec2
pub fn perpendicular(v: Vec2) -> Vec2              // 90° CCW
pub fn lerp_color(a: Color, b: Color, t: f32) -> Color
pub fn interpolate_array(arr: &[f32], index: f32) -> f32
```

**Convention**: `smoothstep(value, edge0, edge1)` — value first, unlike GLSL convention `smoothstep(edge0, edge1, x)`. All modules (especially compositing opacity: `smoothstep(h, 0.0, base_height * 0.7)`) follow this order.

**Convention**: f32→usize conversions use `value.round() as usize` for pixel counts (e.g., `brush_width.round() as usize`, `ridge_width.round() as usize`). Using `.floor()` or `.ceil()` instead causes subtle dimension mismatches between height maps and local frames.

Module: `src/math.rs`

### 14-4. Pressure Curves

```rust
pub fn evaluate_pressure(preset: PressurePreset, t: f32) -> f32
```

Module: `src/pressure.rs`

### 14-5. Seeded RNG

```rust
pub struct SeededRng {
    rng: ChaCha8Rng,    // Deterministic
}
```

Methods: `new(seed)`, `next_f32()`, `next_f32_range(min, max)`, `next_i32_range(min, max)`, `random_in_circle(radius)`.

Module: `src/rng.rs`

Module: `src/types.rs`

---

## 15. Architecture

### 15-1. Module Structure

```
src/
├── lib.rs
├── main.rs
├── asset_io.rs           § 12  Asset I/O
├── types.rs              § 14  Foundation types
├── math.rs               § 14  Math utilities
├── pressure.rs           § 14  Pressure curves
├── rng.rs                § 14  Seeded RNG
├── brush_profile.rs      § 8   Brush profile generation
├── stroke_height.rs      § 8   Stroke height map
├── direction_field.rs    § 4   Direction field
├── region.rs             § 5   Region mask & polygons
├── path_placement.rs     § 6   Path placement
├── local_frame.rs        § 7   Local frame transform
├── stroke_color.rs       § 9   Stroke color
├── compositing.rs        § 10  Compositing
├── output.rs             § 11  Final output
└── project.rs            § 13  Project file
```

### 15-2. Dependency Graph

```
Asset I/O (§12) ─────────────────────────────────────┐
Foundation (§14) ─┬──→ Stroke Height (§8)             │
                  ├──→ Direction Field (§4)            │
                  ├──→ Region Mask (§5)                │
                  ├──→ Local Frame (§7)                │
                  ├──→ Stroke Color (§9)               │
                  ├──→ Project File (§13)              │
                  │                                    │
                  │    Path Placement (§6) ←── §4, §5  │
                  │         │                          │
                  │    Compositing (§10) ←── §6,§7,§8  │
                  │         │                          │
                  │    Final Output (§11) ←──── §10 ───┘
                  │
                  └──→ [GPU Pipeline] (deferred)
                  └──→ [GUI] (deferred)
```

### 15-3. Key Design Constraints

1. **Stateless function interfaces**: All CPU modules have clean, stateless APIs enabling unit testing without GUI or GPU
2. **Deterministic**: Same seed + same params = identical output (ChaCha8 PRNG)
3. **Linear float color space**: sRGB conversion only at I/O boundaries (load and export)

### 15-4. Tech Stack

| Role | Crate | Notes |
|------|-------|-------|
| Language | **Rust** | stable, edition 2021 |
| Math | **glam** 0.29 | vec2, vec3 operations (with serde feature) |
| RNG | **rand** 0.8 + **rand_chacha** 0.3 | Deterministic ChaCha8 PRNG |
| Noise | **noise** 0.9 | fBm (brush profile), 1D Perlin (ridge/wiggle) |
| Serialization | **serde** + **serde_json** + **bincode** | JSON (metadata) / binary (cache) |
| Image I/O | **image** 0.25 | PNG, TGA read/write |
| EXR I/O | **exr** 1.7 | HDR height/color map output |
| OBJ Loading | **tobj** 4 | |
| glTF Loading | **gltf** 1.4 | Includes .glb binary |
| Archive | **zip** 2.1 | .pap project file |

### 15-5. Build & Distribution

| Item | Spec |
|------|------|
| Minimum Rust Version | stable (edition 2021) |
| Target Platforms | Windows, macOS, Linux |
| Packaging | Single binary |

---

## 16. GPU Pipeline (Deferred)

The GPU pipeline is designed but deferred due to performance bottlenecks in the initial per-stroke submit architecture.

### 16-1. Architecture Overview

The GPU pipeline replaces the CPU compositing path using wgpu compute shaders (WGSL). All GPU computations target cross-platform Vulkan / Metal / DX12 via wgpu.

| Shader | Purpose |
|--------|---------|
| `direction_field.wgsl` | IDW interpolation on GPU |
| `streamline.wgsl` | Streamline tracing (parallel per seed) |
| `stroke_height.wgsl` | Per-stroke height generation |
| `composite_atomic.wgsl` | Compositing with atomicMax for height |
| `normal_map.wgsl` | Sobel normal generation |

### 16-2. Known Bottlenecks

1. **Per-stroke sequential submit** (Phase 11): 919 strokes × individual command submit = ~565ms overhead. Solved in design by batch dispatch (Phase 13).
2. **CPU prep bottleneck** (Phase 13): `build_local_frame()` at ~80ms + transform flatten/upload at ~32ms. Solved in design by GPU transform generation (Phase 14).
3. **Overlap filter** (CPU, ~200ms at 2048px): Spatial hash grid helps but remains the single largest bottleneck.

### 16-3. Designed Optimizations (Not Implemented)

- **Batch dispatch** (Phase 13): Flatten all stroke data into concatenated buffers, dispatch in 2 passes (stroke_height + composite). Removes wet-on-wet dependency.
- **GPU transform** (Phase 14): Port `build_local_frame()` to GPU compute shader. Replaces ~62MB CPU→GPU upload with ~360KB path points buffer.

---

## 17. GUI (Deferred)

The GUI is designed as a standalone egui + eframe application with wgpu-backed UV viewport, but deferred due to insufficient planning.

### 17-1. Planned Layout

```
┌─────────────────────────────────────────────────────────────┐
│  Menu Bar                                                   │
├────────────┬──────────────────────────────┬─────────────────┤
│  Region    │     UV Viewport              │   Properties    │
│  List      │     (main workspace)         │   Panel         │
├────────────┴──────────────────────────────┴─────────────────┤
│  3D Preview (toggle) │ Height Map Preview │ Status / Progress│
└─────────────────────────────────────────────────────────────┘
```

### 17-2. Planned Tool Modes

| Mode | Action |
|------|--------|
| Navigate | UV/3D view pan, zoom, rotate |
| Region Draw | Draw region polygons on UV |
| Guide Place | Place direction guides (click+drag) |
| Guide Edit | Select, move, reorient, delete existing guides |
| Select | Select regions/guides |

---

## 18. Verification

### 18-1. Test Strategy

- **Phases 00–10**: Pure logic modules, each includes unit tests runnable with `cargo test`
- **Visual tests**: Phases 02, 05, 08, 09 produce PNG images for visual inspection
- **Coverage test**: Phase 05 includes quantitative ≥90% area coverage test
- **Benchmark tests**: 7 tests marked `#[ignore]` for `cargo test --release -- --ignored`
- **Determinism**: All modules tested for identical output with same seed/params

### 18-2. Key Test Scenarios

| Stage | Scenario | Expected |
|-------|----------|----------|
| Direction Field | Two guides 90° apart | Smooth rotation through 45° at midpoint |
| Direction Field | 180° opposing guides | No cancellation (same direction) |
| Path Placement | Default params, square region | ≥ 90% area coverage |
| Path Placement | Boundary seeds | Entry-edge strokes start at region boundary |
| Stroke Height | load=1.0, Uniform pressure | Max body ≈ base_height |
| Stroke Height | ridge_height=0 | No pixels outside body zone |
| Stroke Height | Depletion check | Height at stroke end ≈ 15% of start |
| Compositing | 3 strokes at same position | Final height = last stroke (not sum) |
| Compositing | Region order | Higher order paints on top |
| Normal Map | Flat surface | All normals = (0.5, 0.5, 1.0) |

---

## 19. Future Expansion

| Area | Current State | Expansion Direction |
|------|---------------|---------------------|
| Curvature Adaptation | None | Auto-adjust brush_width, spacing based on 3D curvature |
| Multiple Brushes | 1 per region | Mix brushes within region (fan, round, palette knife) |
| Speed Modulation | None | Velocity map during streamline tracing → pressure/height coupling |
| Canvas Texture | None (flat) | Base heightmap with woven/linen pattern tiling |
| Manual Stroke Editing | None | Select, move, delete individual strokes |
| Specular Map | None | Derive roughness output from Height Map |
| Layers | None | Photoshop-style layers (dry between layers) |
| GPU Pipeline | Designed | Complete batch dispatch + GPU transform implementation |
| GUI Application | Designed | Full egui implementation per Phase 12 spec |

---

## Appendix A. Stroke Height Generation Interface

```
generate_stroke_height(
    brush_density:    float[N]      Brush profile
    brush_width_px:   int           Brush width
    stroke_length_px: int           Stroke length in local frame
    load:             float         Paint amount (0.0 – 1.0)
    base_height:      float         Stroke body base height
    ridge_height:     float         Ridge height
    ridge_width:      int           Ridge slope width (px)
    ridge_variation:  float         Ridge irregularity
    body_wiggle:      float         Lateral sway amplitude (0.0 – 0.5)
    min_width_ratio:  float         Minimum width ratio
    pressure_fn:      t → float     Pressure curve function
    seed:             uint32        Ridge noise seed
) → {
    height_map:   float[H][W]       H = brush_width + ridge_width × 2
                                    W = stroke_length + ridge_width
}
```

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
| `RIDGE_NOISE_FREQ` | 1.0 | stroke_height | Ridge variation noise frequency |
| `SEED_EXPANSION` | 2.0 | path_placement | Boundary seed expansion factor |
| `SEEK_MAX_MULTIPLIER` | 1.5 | path_placement | Max seek distance = expansion × spacing × 1.5 |
| `OVERLAP_THRESHOLD_DIST` | 0.3 | path_placement | Overlap distance threshold (brush_width multiples) |
| `OVERLAP_RATIO` | 0.7 | path_placement | Overlap ratio to discard path |
| `MIN_LENGTH_FACTOR` | 2.0 | path_placement | Minimum path length (brush_width multiples) |
| `JITTER_FACTOR` | 0.2 | path_placement | Seed jitter amount (spacing multiples) |
| `IDW_EPSILON` | 0.001 | direction_field | Singularity prevention |

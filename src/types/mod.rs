//! Core data structures shared across the pipeline.
//!
//! Defines [`Color`], [`PaintValues`], [`StrokeParams`], [`Layer`], [`Guide`],
//! pressure curves, preset libraries, and all supporting types.

use glam::Vec2;
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

// ── Color Types ──

/// RGBA color in linear float [0, 1] per channel.
/// This is the canonical color type for the entire project.
#[derive(Debug, Clone, Copy)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const WHITE: Color = Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };
    pub const BLACK: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };

    pub fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    /// Check if two colors are approximately equal within the given tolerance.
    pub fn approx_eq(&self, other: &Color, tolerance: f32) -> bool {
        (self.r - other.r).abs() < tolerance
            && (self.g - other.g).abs() < tolerance
            && (self.b - other.b).abs() < tolerance
            && (self.a - other.a).abs() < tolerance
    }
}

impl From<[f32; 4]> for Color {
    fn from(arr: [f32; 4]) -> Self {
        Self {
            r: arr[0],
            g: arr[1],
            b: arr[2],
            a: arr[3],
        }
    }
}

impl From<[f32; 3]> for Color {
    fn from(arr: [f32; 3]) -> Self {
        Self {
            r: arr[0],
            g: arr[1],
            b: arr[2],
            a: 1.0,
        }
    }
}

impl From<Color> for [f32; 4] {
    fn from(c: Color) -> [f32; 4] {
        [c.r, c.g, c.b, c.a]
    }
}

/// HSV color representation. H, S, V all in [0, 1].
#[derive(Debug, Clone, Copy)]
pub struct HsvColor {
    pub h: f32,
    pub s: f32,
    pub v: f32,
}

// ── Conversion Helpers ──

/// Convert LoadedTexture pixels (`Vec<[f32; 4]>` from Phase 00) to `Vec<Color>`.
pub fn pixels_to_colors(pixels: &[[f32; 4]]) -> Vec<Color> {
    pixels.iter().map(|&p| Color::from(p)).collect()
}

// ── Base Color Source ──

/// Reference to the base color for compositing — either a texture or solid color.
///
/// Groups the recurring `(texture, tex_width, tex_height, solid_color)` tuple
/// passed through the compositing pipeline.
pub struct BaseColorSource<'a> {
    pub texture: Option<&'a [Color]>,
    pub tex_width: u32,
    pub tex_height: u32,
    pub solid_color: Color,
}

impl<'a> BaseColorSource<'a> {
    /// Solid color with no texture.
    pub fn solid(color: Color) -> Self {
        Self {
            texture: None,
            tex_width: 0,
            tex_height: 0,
            solid_color: color,
        }
    }

    /// Texture with fallback solid color.
    pub fn textured(data: &'a [Color], width: u32, height: u32, fallback: Color) -> Self {
        Self {
            texture: Some(data),
            tex_width: width,
            tex_height: height,
            solid_color: fallback,
        }
    }
}

/// Owned per-layer base color data for compositing.
/// Thread-safe (`Send + 'static`) for use in worker threads.
pub struct LayerBaseColor {
    pub solid_color: Color,
    pub texture: Option<Vec<Color>>,
    pub tex_width: u32,
    pub tex_height: u32,
}

impl LayerBaseColor {
    /// Solid color with no texture.
    pub fn solid(color: Color) -> Self {
        Self {
            solid_color: color,
            texture: None,
            tex_width: 0,
            tex_height: 0,
        }
    }

    /// Create a [`BaseColorSource`] reference borrowing from this owned data.
    pub fn as_source(&self) -> BaseColorSource<'_> {
        BaseColorSource {
            texture: self.texture.as_deref(),
            tex_width: self.tex_width,
            tex_height: self.tex_height,
            solid_color: self.solid_color,
        }
    }
}

/// Owned per-layer base normal data for UDN blending.
pub struct LayerBaseNormal {
    pub pixels: Option<Vec<[f32; 4]>>,
    pub width: u32,
    pub height: u32,
}

impl LayerBaseNormal {
    /// No base normal (identity blending).
    pub fn none() -> Self {
        Self {
            pixels: None,
            width: 0,
            height: 0,
        }
    }
}

// ── Texture Source ──

/// Embedded texture data stored inside a project (in-memory representation).
///
/// `pixels` is wrapped in `Arc` so that undo snapshots (which clone `Layer`) only
/// copy the reference count, not the image data.  `#[serde(skip)]` excludes
/// the heavy pixel buffer from JSON; actual persistence uses PNG inside the ZIP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddedTexture {
    /// Display name (typically the original file name).
    pub label: String,
    /// Linear RGBA pixels.
    #[serde(skip)]
    pub pixels: Arc<Vec<[f32; 4]>>,
    pub width: u32,
    pub height: u32,
    /// Hash of pixel data, computed on load. Not persisted — always
    /// recomputed from the decoded pixel buffer when a project is loaded.
    #[serde(skip)]
    pub content_hash: u64,
}

impl EmbeddedTexture {
    /// Compute a hash over the pixel data.
    pub fn compute_content_hash(pixels: &[[f32; 4]]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for px in pixels {
            for ch in px {
                ch.to_bits().hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

impl PartialEq for EmbeddedTexture {
    fn eq(&self, other: &Self) -> bool {
        self.content_hash == other.content_hash
            && self.width == other.width
            && self.height == other.height
    }
}

/// Per-layer texture source for base color or base normal.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum TextureSource {
    /// No texture assigned.  Color default → solid gray; Normal default → unused.
    #[default]
    None,
    /// Solid color (color layers only).
    Solid([f32; 3]),
    /// Reference to a GLB embedded material texture by index.
    MeshMaterial(usize),
    /// External file.  `Some` = loaded; `None` = slot reserved but file not yet chosen
    /// (rendered as a checkerboard warning pattern).
    File(Option<EmbeddedTexture>),
}

/// Generate a 256×256 magenta/black checkerboard warning texture (16px tiles).
/// Used as a visual warning for `TextureSource::File(None)` (unassigned file slot).
/// Higher resolution avoids bilinear-interpolation blur at tile edges.
pub fn checkerboard_warning_texture() -> EmbeddedTexture {
    let size = 256u32;
    let tile = 16u32;
    let mut pixels = Vec::with_capacity((size * size) as usize);
    let magenta = [1.0, 0.0, 1.0, 1.0];
    let black = [0.15, 0.15, 0.15, 1.0];
    for y in 0..size {
        for x in 0..size {
            let tx = x / tile;
            let ty = y / tile;
            pixels.push(if (tx + ty).is_multiple_of(2) {
                magenta
            } else {
                black
            });
        }
    }
    let content_hash = EmbeddedTexture::compute_content_hash(&pixels);
    EmbeddedTexture {
        label: "__checkerboard__".to_string(),
        pixels: Arc::new(pixels),
        width: size,
        height: size,
        content_hash,
    }
}

// ── Enums ──

/// Pressure curve preset determining how brush pressure varies along a stroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PressurePreset {
    Uniform,
    FadeOut,
    FadeIn,
    Bell,
    Taper,
}

/// A control point on a custom pressure curve with Bézier handles.
///
/// Each knot has an on-curve position and two handle positions (incoming
/// and outgoing) that control the tangent at this point.  Handles are
/// stored as absolute `[x, y]` coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CurveKnot {
    pub pos: [f32; 2],
    pub handle_in: [f32; 2],
    pub handle_out: [f32; 2],
}

impl CurveKnot {
    /// Create a knot with automatically computed smooth handles.
    ///
    /// `prev` / `next` are positions of neighboring knots.  Handle
    /// direction follows the tangent from prev→next through this point,
    /// with x-length = 1/3 of the span to the neighbor.
    pub fn smooth(pos: [f32; 2], prev: Option<[f32; 2]>, next: Option<[f32; 2]>) -> Self {
        match (prev, next) {
            (Some(p), Some(n)) => {
                let dx = n[0] - p[0];
                let slope = if dx.abs() > 1e-6 {
                    (n[1] - p[1]) / dx
                } else {
                    0.0
                };
                let in_dx = (pos[0] - p[0]) / 3.0;
                let out_dx = (n[0] - pos[0]) / 3.0;
                CurveKnot {
                    pos,
                    handle_in: [pos[0] - in_dx, pos[1] - in_dx * slope],
                    handle_out: [pos[0] + out_dx, pos[1] + out_dx * slope],
                }
            }
            (None, Some(n)) => {
                let out_dx = (n[0] - pos[0]) / 3.0;
                let slope = if (n[0] - pos[0]).abs() > 1e-6 {
                    (n[1] - pos[1]) / (n[0] - pos[0])
                } else {
                    0.0
                };
                CurveKnot {
                    pos,
                    handle_in: pos,
                    handle_out: [pos[0] + out_dx, pos[1] + out_dx * slope],
                }
            }
            (Some(p), None) => {
                let in_dx = (pos[0] - p[0]) / 3.0;
                let slope = if (pos[0] - p[0]).abs() > 1e-6 {
                    (pos[1] - p[1]) / (pos[0] - p[0])
                } else {
                    0.0
                };
                CurveKnot {
                    pos,
                    handle_in: [pos[0] - in_dx, pos[1] - in_dx * slope],
                    handle_out: pos,
                }
            }
            (None, None) => CurveKnot {
                pos,
                handle_in: pos,
                handle_out: pos,
            },
        }
    }
}

/// Pressure curve — either a named preset or a custom Bézier spline.
///
/// For `Custom`, the curve is a piecewise cubic Bézier through a sorted
/// list of [`CurveKnot`]s.  Each knot has an on-curve position and two
/// handles controlling the tangent.  The first knot must have x=0 and
/// the last x=1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PressureCurve {
    Preset(PressurePreset),
    Custom(Vec<CurveKnot>),
}

impl Default for PressureCurve {
    fn default() -> Self {
        PressureCurve::Preset(PressurePreset::FadeOut)
    }
}

impl PressureCurve {
    /// Returns `true` if this is a `Custom` curve.
    pub fn is_custom(&self) -> bool {
        matches!(self, PressureCurve::Custom(_))
    }
}

/// Per-layer stroke parameters (pipeline type — used by rendering engine).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrokeParams {
    pub brush_width: f32,
    pub load: f32,
    pub body_wiggle: f32,
    pub stroke_spacing: f32,
    pub pressure_curve: PressureCurve,
    pub color_variation: f32,
    pub max_stroke_length: f32,
    pub angle_variation: f32,
    pub max_turn_angle: f32,
    /// If set, strokes terminate when the per-step color difference (max channel)
    /// exceeds this threshold.  `None` = disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_break_threshold: Option<f32>,
    /// If set, strokes terminate when the cumulative object-space normal
    /// deviation from the stroke start exceeds the threshold.
    /// Value is a dot-product floor: break if `dot(n_start, n_current) < threshold`.
    /// Typical: 0.9 (~25°), 0.5 (~60°), 0.0 (~90°).  `None` = disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_break_threshold: Option<f32>,
    /// Overlap ratio threshold for the duplicate-path filter.
    /// A path is rejected if this fraction of its points are within
    /// `overlap_dist_factor × brush_width_uv` of an already-accepted path.
    /// Default (None) = 0.7.  Raise toward 1.0 to allow more overlap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlap_ratio: Option<f32>,
    /// Distance factor for the duplicate-path filter, multiplied by `brush_width_uv`.
    /// Default (None) = 0.3.  Lower values narrow the "too close" zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlap_dist_factor: Option<f32>,
    /// Paint viscosity (0.0–1.0). Higher values spread the bristle pattern
    /// even when paint is abundant, simulating thick impasto or acrylic media.
    /// 0.0 = no effect (default, backward compatible).
    #[serde(default)]
    pub viscosity: f32,
    pub seed: u32,
}

impl Default for StrokeParams {
    fn default() -> Self {
        Self {
            brush_width: 30.0,
            load: 0.8,
            body_wiggle: 0.15,
            stroke_spacing: 1.0,
            pressure_curve: PressureCurve::default(),
            color_variation: 0.1,
            max_stroke_length: 240.0,
            angle_variation: 5.0,
            max_turn_angle: 15.0,
            color_break_threshold: None,
            normal_break_threshold: None,
            overlap_ratio: None,
            overlap_dist_factor: None,
            viscosity: 0.0,
            seed: 42,
        }
    }
}

/// Reference resolution at which pixel-unit parameters (brush_width,
/// max_stroke_length) are authored.  Changing output resolution scales
/// these values so the painting retains the same proportions.
pub const BASE_RESOLUTION: u32 = 1024;

impl StrokeParams {
    /// Reconstruct StrokeParams from unified PaintValues, clamping to safe ranges.
    pub fn from_paint_values(paint: &PaintValues, seed: u32) -> StrokeParams {
        StrokeParams {
            brush_width: paint.brush_width.max(1.0),
            load: paint.load.clamp(0.0, 2.0),
            body_wiggle: paint.body_wiggle.max(0.0),
            pressure_curve: paint.pressure_curve.clone(),
            stroke_spacing: paint.stroke_spacing.max(0.01),
            max_stroke_length: paint.max_stroke_length.max(1.0),
            angle_variation: paint.angle_variation.max(0.0),
            max_turn_angle: paint.max_turn_angle.max(0.0),
            color_break_threshold: paint.color_break_threshold.map(|v| v.max(0.0)),
            normal_break_threshold: paint.normal_break_threshold.map(|v| v.max(0.0)),
            overlap_ratio: paint.overlap_ratio.map(|v| v.clamp(0.0, 1.0)),
            overlap_dist_factor: paint.overlap_dist_factor.map(|v| v.max(0.01)),
            color_variation: paint.color_variation.clamp(0.0, 1.0),
            viscosity: paint.viscosity.clamp(0.0, 1.0),
            seed,
        }
    }

    /// Return a copy with pixel-unit fields scaled for the given output resolution.
    /// At `BASE_RESOLUTION` the values are unchanged; at 2× the resolution,
    /// brush_width and max_stroke_length double so UV-space proportions stay fixed.
    pub fn scaled_for_resolution(&self, resolution: u32) -> StrokeParams {
        let scale = resolution as f32 / BASE_RESOLUTION as f32;
        StrokeParams {
            brush_width: self.brush_width * scale,
            max_stroke_length: self.max_stroke_length * scale,
            ..self.clone()
        }
    }
}

// ── Guide ──

/// Direction guide type controlling how the guide influences stroke direction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GuideType {
    /// Linear flow in a specified direction (original behavior).
    #[default]
    Directional,
    /// Radial outward flow from center.
    Source,
    /// Radial inward flow toward center.
    Sink,
    /// Rotational flow around center.
    Vortex,
}

fn default_strength() -> f32 {
    1.0
}

/// Direction guide placed by the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guide {
    #[serde(default)]
    pub guide_type: GuideType,
    pub position: Vec2,
    pub direction: Vec2,
    pub influence: f32,
    #[serde(default = "default_strength")]
    pub strength: f32,
}

impl Default for Guide {
    fn default() -> Self {
        Self {
            guide_type: GuideType::Directional,
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 0.2,
            strength: 1.0,
        }
    }
}

/// A paint layer defining stroke parameters and direction guides.
/// Pipeline type — covers the full UV space \[0,1\]², consumed by rendering engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaintLayer {
    pub name: String,
    pub order: i32,
    pub params: StrokeParams,
    pub guides: Vec<Guide>,
}

// ── Mesh Groups ──

/// A sub-group within a loaded mesh (vertex group / submesh / OBJ object).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshGroup {
    /// Group name (OBJ `g` name or glTF primitive/material name).
    pub name: String,
    /// Start offset into the mesh's indices array.
    pub index_offset: u32,
    /// Number of indices belonging to this group (always a multiple of 3).
    pub index_count: u32,
}

// ── PaintValues (unified brush + layout settings) ──

/// Unified paint settings combining brush physics and layout strategy.
/// This is the preset unit — guides are NOT included (UV-space dependent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaintValues {
    // Brush physics
    pub brush_width: f32,
    pub load: f32,
    pub body_wiggle: f32,
    pub pressure_curve: PressureCurve,

    // Layout strategy
    pub stroke_spacing: f32,
    pub max_stroke_length: f32,
    pub angle_variation: f32,
    pub max_turn_angle: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_break_threshold: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_break_threshold: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlap_ratio: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlap_dist_factor: Option<f32>,
    pub color_variation: f32,
    /// Paint viscosity (0.0–1.0). See [`StrokeParams::viscosity`].
    #[serde(default)]
    pub viscosity: f32,
}

impl Default for PaintValues {
    fn default() -> Self {
        // Match built-in "heavy_load" preset so new layers start with a known preset.
        PresetLibrary::built_in()
            .presets
            .into_iter()
            .find(|p| p.name == "heavy_load")
            .expect("built-in 'heavy_load' preset must exist")
            .values
    }
}

// IMPORTANT: When adding fields to PaintValues, update this Hash impl too.
// Forgetting a field will silently break preview cache invalidation.
impl Hash for PaintValues {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.brush_width.to_bits().hash(state);
        self.load.to_bits().hash(state);
        self.body_wiggle.to_bits().hash(state);
        match &self.pressure_curve {
            PressureCurve::Preset(p) => {
                0u8.hash(state);
                p.hash(state);
            }
            PressureCurve::Custom(knots) => {
                1u8.hash(state);
                knots.len().hash(state);
                for k in knots {
                    k.pos[0].to_bits().hash(state);
                    k.pos[1].to_bits().hash(state);
                    k.handle_in[0].to_bits().hash(state);
                    k.handle_in[1].to_bits().hash(state);
                    k.handle_out[0].to_bits().hash(state);
                    k.handle_out[1].to_bits().hash(state);
                }
            }
        }
        self.stroke_spacing.to_bits().hash(state);
        self.max_stroke_length.to_bits().hash(state);
        self.angle_variation.to_bits().hash(state);
        self.max_turn_angle.to_bits().hash(state);
        self.color_break_threshold.map(|v| v.to_bits()).hash(state);
        self.normal_break_threshold.map(|v| v.to_bits()).hash(state);
        self.overlap_ratio.map(|v| v.to_bits()).hash(state);
        self.overlap_dist_factor.map(|v| v.to_bits()).hash(state);
        self.color_variation.to_bits().hash(state);
        self.viscosity.to_bits().hash(state);
    }
}

// ── Layer ──

fn default_dry() -> f32 {
    1.0
}

fn default_visible() -> bool {
    true
}

fn default_base_color_source() -> TextureSource {
    TextureSource::Solid([1.0, 1.0, 1.0])
}

/// A paint layer binding a mesh group to paint settings and guides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub name: String,
    #[serde(default = "default_visible")]
    pub visible: bool,
    /// Compositing order (lower = painted first).
    pub order: i32,
    /// Name of the corresponding MeshGroup (or `"__all__"` for whole UV).
    pub group_name: String,
    /// Unified paint settings (brush + layout).
    pub paint: PaintValues,
    /// Direction guides for this layer.
    pub guides: Vec<Guide>,
    /// Per-layer base color source.
    #[serde(default = "default_base_color_source")]
    pub base_color: TextureSource,
    /// Per-layer base normal source.
    #[serde(default)]
    pub base_normal: TextureSource,
    /// How dry the surface below was when this layer was painted (0.0–1.0).
    /// dry=0 → painted on wet surface (subtractive mix + max height),
    /// dry=1 → painted on dry surface (opaque over + height accumulation).
    #[serde(default = "default_dry")]
    pub dry: f32,
    /// Per-layer random seed for path placement and stroke variation.
    /// Stable across reordering — not derived from array position.
    #[serde(default)]
    pub seed: u32,
}

impl Layer {
    /// Convert to PaintLayer for downstream pipeline compatibility.
    pub fn to_paint_layer(&self) -> PaintLayer {
        PaintLayer {
            name: self.group_name.clone(),
            order: self.order,
            params: StrokeParams::from_paint_values(&self.paint, self.seed),
            guides: self.guides.clone(),
        }
    }

    /// Hash of the fields that affect `render_layer()` output.
    ///
    /// Covers `paint`, `guides`, `group_name`, `base_color`, and `seed`.
    /// Fields that only affect merge/output stages (`dry`, `visible`, `name`,
    /// `base_normal`, `order`) are excluded so that changing them does not
    /// invalidate the per-layer render cache.
    pub fn render_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        if let Ok(bytes) = serde_json::to_vec(&(
            &self.paint,
            &self.guides,
            &self.group_name,
            &self.base_color,
        )) {
            bytes.hash(&mut hasher);
        }
        self.seed.hash(&mut hasher);
        hasher.finish()
    }

    /// Hash of the fields that affect path generation only.
    ///
    /// Subset of `render_hash()`. Type C parameters (`load`, `body_wiggle`,
    /// `pressure_curve`, `color_variation`, `viscosity`, `normal_break_threshold`)
    /// are excluded — changing them allows cached paths to be reused while
    /// only re-running stroke height and compositing.
    pub fn path_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // Path-affecting PaintValues fields
        let p = &self.paint;
        p.brush_width.to_bits().hash(&mut hasher);
        p.stroke_spacing.to_bits().hash(&mut hasher);
        p.max_stroke_length.to_bits().hash(&mut hasher);
        p.angle_variation.to_bits().hash(&mut hasher);
        p.max_turn_angle.to_bits().hash(&mut hasher);
        p.color_break_threshold
            .map(|v| v.to_bits())
            .hash(&mut hasher);
        p.overlap_ratio.map(|v| v.to_bits()).hash(&mut hasher);
        p.overlap_dist_factor.map(|v| v.to_bits()).hash(&mut hasher);
        // Non-PaintValues path-affecting fields
        if let Ok(bytes) = serde_json::to_vec(&(&self.guides, &self.group_name, &self.base_color)) {
            bytes.hash(&mut hasher);
        }
        self.seed.hash(&mut hasher);
        hasher.finish()
    }
}

// ── Preset System ──

/// A named paint preset (brush + layout template). Guides not included.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaintPreset {
    pub name: String,
    #[serde(flatten)]
    pub values: PaintValues,
}

/// Global library of reusable presets.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PresetLibrary {
    pub presets: Vec<PaintPreset>,
}

impl PresetLibrary {
    /// Find a preset whose values match exactly.
    pub fn matching_preset(&self, values: &PaintValues) -> Option<&str> {
        self.presets
            .iter()
            .find(|p| &p.values == values)
            .map(|p| p.name.as_str())
    }

    /// Add a preset if no duplicate exists. Returns Err with the existing name.
    pub fn try_add_preset(&mut self, preset: PaintPreset) -> Result<(), String> {
        if let Some(name) = self.matching_preset(&preset.values) {
            return Err(name.to_string());
        }
        self.presets.push(preset);
        Ok(())
    }
}

/// A stroke path: ordered list of UV-space points with cached arc-length table.
#[derive(Debug, Clone)]
pub struct StrokePath {
    pub points: Vec<Vec2>,
    pub layer_index: u32,
    pub stroke_id: u32,
    cumulative_lengths: Vec<f32>,
    total_length: f32,
}

impl StrokePath {
    pub fn new(points: Vec<Vec2>, layer_index: u32, stroke_id: u32) -> Self {
        let mut cumulative_lengths = Vec::with_capacity(points.len());
        let mut accum = 0.0f32;
        cumulative_lengths.push(0.0);
        for w in points.windows(2) {
            accum += (w[1] - w[0]).length();
            cumulative_lengths.push(accum);
        }
        Self {
            points,
            layer_index,
            stroke_id,
            cumulative_lengths,
            total_length: accum,
        }
    }

    /// Total arc length of the path (O(1), cached).
    pub fn arc_length(&self) -> f32 {
        self.total_length
    }

    /// Cached cumulative arc lengths (one entry per point, starting at 0.0).
    pub fn cumulative_lengths(&self) -> &[f32] {
        &self.cumulative_lengths
    }

    /// Find segment index and local fraction for a given distance along the path.
    fn find_segment(&self, distance: f32) -> (usize, f32) {
        let last_seg = self.points.len() - 2;
        let idx = match self.cumulative_lengths.binary_search_by(|v| {
            v.partial_cmp(&distance)
                .unwrap_or(std::cmp::Ordering::Equal)
        }) {
            Ok(i) => i.min(last_seg),
            Err(i) => i.saturating_sub(1).min(last_seg),
        };
        let seg_len = self.cumulative_lengths[idx + 1] - self.cumulative_lengths[idx];
        let frac = if seg_len > 0.0 {
            ((distance - self.cumulative_lengths[idx]) / seg_len).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (idx, frac)
    }

    /// Point at the midpoint of the path (by arc length).
    pub fn midpoint(&self) -> Vec2 {
        if self.points.is_empty() {
            return Vec2::ZERO;
        }
        if self.points.len() == 1 {
            return self.points[0];
        }
        let (seg, frac) = self.find_segment(self.total_length * 0.5);
        self.points[seg].lerp(self.points[seg + 1], frac)
    }

    /// Sample position at parameter t (0.0 = start, 1.0 = end) by arc length.
    pub fn sample(&self, t: f32) -> Vec2 {
        if self.points.is_empty() {
            return Vec2::ZERO;
        }
        if self.points.len() == 1 || t <= 0.0 {
            return self.points[0];
        }
        if t >= 1.0 {
            // Safety: len >= 1 checked above, so last() is always Some.
            return *self.points.last().expect("non-empty path");
        }
        let (seg, frac) = self.find_segment(self.total_length * t);
        self.points[seg].lerp(self.points[seg + 1], frac)
    }

    /// Tangent direction at parameter t (normalized).
    pub fn tangent(&self, t: f32) -> Vec2 {
        if self.points.len() < 2 {
            return Vec2::X;
        }
        let (seg, _) = self.find_segment(self.total_length * t.clamp(0.0, 1.0));
        let dir = self.points[seg + 1] - self.points[seg];
        if dir.length() > 1e-6 {
            dir.normalize()
        } else {
            Vec2::X
        }
    }
}

/// Normal map Y-axis convention for export.
///
/// Controls the green channel direction in exported normal maps.
/// The internal renderer always uses its own consistent convention;
/// this setting only affects exported PNGs and GLB files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NormalYConvention {
    /// +Y = up (Blender, Godot, glTF, Maya).
    #[default]
    OpenGL,
    /// +Y = down (Unreal Engine, Unity default, 3ds Max).
    DirectX,
}

/// Normal map generation mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NormalMode {
    /// Height-only Sobel normals (original behavior).
    SurfacePaint,
    /// Object-space normals from mesh geometry, perturbed by paint height.
    #[default]
    DepictedForm,
}

/// Background compositing mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackgroundMode {
    /// Strokes blend with the base color / texture (original behavior).
    #[default]
    Opaque,
    /// Paint-only blending: unpainted areas are fully transparent,
    /// low-height edges are semi-transparent. Strokes blend only with
    /// other strokes, never with the background.
    Transparent,
}

/// Output resolution preset with named tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolutionPreset {
    Preview,
    Standard,
    High,
    Ultra,
}

impl ResolutionPreset {
    pub fn resolution(&self) -> u32 {
        match self {
            Self::Preview => 512,
            Self::Standard => 1024,
            Self::High => 2048,
            Self::Ultra => 4096,
        }
    }
}

/// Per-layer compositing settings for [`merge_layers()`](crate::pipeline::compositing::merge_layers).
///
/// Controls how a layer blends with accumulated layers below it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerCompositeSettings {
    /// Layer-wide opacity multiplier (0.0–1.0).
    pub opacity: f32,
}

impl Default for LayerCompositeSettings {
    fn default() -> Self {
        Self { opacity: 1.0 }
    }
}

/// Global output settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputSettings {
    pub resolution_preset: ResolutionPreset,
    pub normal_strength: f32,
    #[serde(default)]
    pub normal_mode: NormalMode,
    #[serde(default)]
    pub background_mode: BackgroundMode,
}

impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            resolution_preset: ResolutionPreset::Standard,
            normal_strength: 0.3,
            normal_mode: NormalMode::default(),
            background_mode: BackgroundMode::default(),
        }
    }
}

/// Export settings — controls what to export and in what format.
///
/// Two independent axes: texture map images and 3D model file.
/// Stored per-project; changes do NOT trigger re-generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportSettings {
    // ── Texture Maps ──
    /// Export texture map images.
    #[serde(default = "default_true")]
    pub export_maps: bool,
    /// Image format for texture maps.
    #[serde(default)]
    pub format: crate::pipeline::output::ExportFormat,
    /// Include color map.
    #[serde(default = "default_true")]
    pub include_color: bool,
    /// Include normal map.
    #[serde(default = "default_true")]
    pub include_normal: bool,
    /// Include height map.
    #[serde(default = "default_true")]
    pub include_height: bool,
    /// Include stroke ID map.
    #[serde(default)]
    pub include_stroke_id: bool,
    /// Include stroke time map.
    #[serde(default)]
    pub include_time_map: bool,
    /// Export each layer as separate texture files (in addition to composite).
    #[serde(default)]
    pub per_layer: bool,

    // ── 3D Model ──
    /// Export 3D model file.
    #[serde(default)]
    pub export_model: bool,
    /// Embed color texture in the 3D model.
    #[serde(default = "default_true")]
    pub embed_color: bool,
    /// Embed normal map in the 3D model.
    #[serde(default = "default_true")]
    pub embed_normal: bool,

    // ── Normal Convention ──
    /// Y-axis convention for exported normal maps.
    #[serde(default)]
    pub normal_y: NormalYConvention,
}

fn default_true() -> bool {
    true
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            export_maps: true,
            format: crate::pipeline::output::ExportFormat::Png,
            include_color: true,
            include_normal: true,
            include_height: true,
            include_stroke_id: false,
            include_time_map: false,
            per_layer: false,
            export_model: false,
            embed_color: true,
            embed_normal: true,
            normal_y: NormalYConvention::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-5;

    fn make_path(pts: Vec<Vec2>) -> StrokePath {
        StrokePath::new(pts, 0, 0)
    }

    #[test]
    fn arc_length_straight() {
        let path = make_path(vec![Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0)]);
        assert!((path.arc_length() - 1.0).abs() < EPS);
    }

    #[test]
    fn arc_length_l_shape() {
        let path = make_path(vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
        ]);
        assert!((path.arc_length() - 2.0).abs() < EPS);
    }

    #[test]
    fn midpoint_straight() {
        let path = make_path(vec![Vec2::new(0.0, 0.0), Vec2::new(2.0, 0.0)]);
        let mid = path.midpoint();
        assert!((mid.x - 1.0).abs() < EPS);
        assert!((mid.y - 0.0).abs() < EPS);
    }

    #[test]
    fn sample_start() {
        let path = make_path(vec![Vec2::new(1.0, 2.0), Vec2::new(3.0, 4.0)]);
        let s = path.sample(0.0);
        assert!((s.x - 1.0).abs() < EPS);
        assert!((s.y - 2.0).abs() < EPS);
    }

    #[test]
    fn sample_end() {
        let path = make_path(vec![Vec2::new(1.0, 2.0), Vec2::new(3.0, 4.0)]);
        let s = path.sample(1.0);
        assert!((s.x - 3.0).abs() < EPS);
        assert!((s.y - 4.0).abs() < EPS);
    }

    #[test]
    fn tangent_straight() {
        let path = make_path(vec![Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0)]);
        let t = path.tangent(0.5);
        assert!((t.x - 1.0).abs() < EPS);
        assert!((t.y - 0.0).abs() < EPS);
    }

    #[test]
    fn serde_round_trip() {
        let params = StrokeParams::default();
        let json = serde_json::to_string(&params).unwrap();
        let _: StrokeParams = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn texture_source_default_is_none() {
        assert_eq!(TextureSource::default(), TextureSource::None);
    }

    #[test]
    fn embedded_texture_partial_eq_uses_content_hash() {
        let px1 = vec![[1.0; 4]];
        let px2 = vec![[0.0; 4]];
        let t1 = EmbeddedTexture {
            label: "a.png".to_string(),
            content_hash: EmbeddedTexture::compute_content_hash(&px1),
            pixels: Arc::new(px1),
            width: 1,
            height: 1,
        };
        let t2 = EmbeddedTexture {
            label: "a.png".to_string(),
            content_hash: EmbeddedTexture::compute_content_hash(&px2),
            pixels: Arc::new(px2),
            width: 1,
            height: 1,
        };
        assert_ne!(
            t1, t2,
            "Different pixel data should produce different content_hash"
        );

        let px3 = vec![[1.0; 4]];
        let t3 = EmbeddedTexture {
            label: "b.png".to_string(), // different label, same pixels
            content_hash: EmbeddedTexture::compute_content_hash(&px3),
            pixels: Arc::new(px3),
            width: 1,
            height: 1,
        };
        assert_eq!(
            t1, t3,
            "Same content_hash + dims should be equal regardless of label"
        );
    }

    #[test]
    fn checkerboard_dimensions() {
        let cb = checkerboard_warning_texture();
        assert_eq!(cb.width, 256);
        assert_eq!(cb.height, 256);
        assert_eq!(cb.pixels.len(), 256 * 256);
        // First tile (0,0) should be magenta
        assert_eq!(cb.pixels[0], [1.0, 0.0, 1.0, 1.0]);
        // Adjacent tile (16,0) should be dark gray
        assert_eq!(cb.pixels[16], [0.15, 0.15, 0.15, 1.0]);
    }

    #[test]
    fn resolution_preset_values() {
        assert_eq!(ResolutionPreset::Preview.resolution(), 512);
        assert_eq!(ResolutionPreset::Standard.resolution(), 1024);
        assert_eq!(ResolutionPreset::High.resolution(), 2048);
        assert_eq!(ResolutionPreset::Ultra.resolution(), 4096);
    }
}

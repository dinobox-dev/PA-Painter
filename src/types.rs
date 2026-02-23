use glam::Vec2;
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};

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

/// Per-layer stroke parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrokeParams {
    pub brush_width: f32,
    pub load: f32,
    pub base_height: f32,
    pub ridge_height: f32,
    pub ridge_width: f32,
    pub ridge_variation: f32,
    pub body_wiggle: f32,
    pub stroke_spacing: f32,
    pub pressure_preset: PressurePreset,
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
    pub seed: u32,
}

impl Default for StrokeParams {
    fn default() -> Self {
        Self {
            brush_width: 30.0,
            load: 0.8,
            base_height: 0.5,
            ridge_height: 0.3,
            ridge_width: 5.0,
            ridge_variation: 0.1,
            body_wiggle: 0.15,
            stroke_spacing: 1.0,
            pressure_preset: PressurePreset::FadeOut,
            color_variation: 0.1,
            max_stroke_length: 240.0,
            angle_variation: 5.0,
            max_turn_angle: 15.0,
            color_break_threshold: None,
            normal_break_threshold: None,
            overlap_ratio: None,
            overlap_dist_factor: None,
            seed: 42,
        }
    }
}

/// Direction guide vertex placed by the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuideVertex {
    pub position: Vec2,
    pub direction: Vec2,
    pub influence: f32,
}

impl Default for GuideVertex {
    fn default() -> Self {
        Self {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 0.2,
        }
    }
}

/// A paint layer defining stroke parameters and direction guides.
/// Covers the full UV space [0,1]² — no polygon mask.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaintLayer {
    pub name: String,
    pub order: i32,
    pub params: StrokeParams,
    pub guides: Vec<GuideVertex>,
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

// ── Preset System ──

/// Brush physics values extracted from StrokeParams.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrokeValues {
    pub brush_width: f32,
    pub load: f32,
    pub base_height: f32,
    pub ridge_height: f32,
    pub ridge_width: f32,
    pub ridge_variation: f32,
    pub body_wiggle: f32,
    pub pressure_preset: PressurePreset,
}

impl Default for StrokeValues {
    fn default() -> Self {
        let d = StrokeParams::default();
        Self {
            brush_width: d.brush_width,
            load: d.load,
            base_height: d.base_height,
            ridge_height: d.ridge_height,
            ridge_width: d.ridge_width,
            ridge_variation: d.ridge_variation,
            body_wiggle: d.body_wiggle,
            pressure_preset: d.pressure_preset,
        }
    }
}

impl Hash for StrokeValues {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.brush_width.to_bits().hash(state);
        self.load.to_bits().hash(state);
        self.base_height.to_bits().hash(state);
        self.ridge_height.to_bits().hash(state);
        self.ridge_width.to_bits().hash(state);
        self.ridge_variation.to_bits().hash(state);
        self.body_wiggle.to_bits().hash(state);
        self.pressure_preset.hash(state);
    }
}

/// Placement strategy values extracted from StrokeParams.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternValues {
    pub guides: Vec<GuideVertex>,
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
}

impl Default for PatternValues {
    fn default() -> Self {
        let d = StrokeParams::default();
        Self {
            guides: vec![GuideVertex::default()],
            stroke_spacing: d.stroke_spacing,
            max_stroke_length: d.max_stroke_length,
            angle_variation: d.angle_variation,
            max_turn_angle: d.max_turn_angle,
            color_break_threshold: d.color_break_threshold,
            normal_break_threshold: d.normal_break_threshold,
            overlap_ratio: d.overlap_ratio,
            overlap_dist_factor: d.overlap_dist_factor,
            color_variation: d.color_variation,
        }
    }
}

impl Hash for PatternValues {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for g in &self.guides {
            g.position.x.to_bits().hash(state);
            g.position.y.to_bits().hash(state);
            g.direction.x.to_bits().hash(state);
            g.direction.y.to_bits().hash(state);
            g.influence.to_bits().hash(state);
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
    }
}

/// A named stroke preset (brush physics template).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrokePreset {
    pub name: String,
    #[serde(flatten)]
    pub values: StrokeValues,
}

/// A named pattern preset (placement strategy template).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternPreset {
    pub name: String,
    #[serde(flatten)]
    pub values: PatternValues,
}

/// A paint slot binding a mesh group to stroke/pattern settings.
/// Replaces PaintLayer as the user-facing representation.
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

impl PaintSlot {
    /// Convert to PaintLayer for downstream pipeline compatibility.
    pub fn to_paint_layer(&self) -> PaintLayer {
        PaintLayer {
            name: self.group_name.clone(),
            order: self.order,
            params: StrokeParams::from_values(&self.stroke, &self.pattern, self.seed),
            guides: self.pattern.guides.clone(),
        }
    }

    /// Create a PaintSlot from a PaintLayer (used for v2→v3 migration).
    pub fn from_paint_layer(layer: &PaintLayer) -> Self {
        let p = &layer.params;
        PaintSlot {
            group_name: "__full_uv__".to_string(),
            order: layer.order,
            stroke: StrokeValues {
                brush_width: p.brush_width,
                load: p.load,
                base_height: p.base_height,
                ridge_height: p.ridge_height,
                ridge_width: p.ridge_width,
                ridge_variation: p.ridge_variation,
                body_wiggle: p.body_wiggle,
                pressure_preset: p.pressure_preset,
            },
            pattern: PatternValues {
                guides: layer.guides.clone(),
                stroke_spacing: p.stroke_spacing,
                max_stroke_length: p.max_stroke_length,
                angle_variation: p.angle_variation,
                max_turn_angle: p.max_turn_angle,
                color_break_threshold: p.color_break_threshold,
                normal_break_threshold: p.normal_break_threshold,
                overlap_ratio: p.overlap_ratio,
                overlap_dist_factor: p.overlap_dist_factor,
                color_variation: p.color_variation,
            },
            seed: p.seed,
        }
    }

    /// Apply a stroke preset (value copy).
    pub fn apply_stroke_preset(&mut self, preset: &StrokePreset) {
        self.stroke = preset.values.clone();
    }

    /// Apply a pattern preset (value copy).
    pub fn apply_pattern_preset(&mut self, preset: &PatternPreset) {
        self.pattern = preset.values.clone();
    }
}

impl StrokeParams {
    /// Reconstruct StrokeParams from split values.
    pub fn from_values(stroke: &StrokeValues, pattern: &PatternValues, seed: u32) -> StrokeParams {
        StrokeParams {
            brush_width: stroke.brush_width,
            load: stroke.load,
            base_height: stroke.base_height,
            ridge_height: stroke.ridge_height,
            ridge_width: stroke.ridge_width,
            ridge_variation: stroke.ridge_variation,
            body_wiggle: stroke.body_wiggle,
            pressure_preset: stroke.pressure_preset,
            stroke_spacing: pattern.stroke_spacing,
            max_stroke_length: pattern.max_stroke_length,
            angle_variation: pattern.angle_variation,
            max_turn_angle: pattern.max_turn_angle,
            color_break_threshold: pattern.color_break_threshold,
            normal_break_threshold: pattern.normal_break_threshold,
            overlap_ratio: pattern.overlap_ratio,
            overlap_dist_factor: pattern.overlap_dist_factor,
            color_variation: pattern.color_variation,
            seed,
        }
    }
}

/// Global library of reusable presets.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PresetLibrary {
    pub strokes: Vec<StrokePreset>,
    pub patterns: Vec<PatternPreset>,
}

impl PresetLibrary {
    /// Find a stroke preset whose values match exactly.
    pub fn matching_stroke_preset(&self, values: &StrokeValues) -> Option<&str> {
        self.strokes
            .iter()
            .find(|p| &p.values == values)
            .map(|p| p.name.as_str())
    }

    /// Find a pattern preset whose values match exactly.
    pub fn matching_pattern_preset(&self, values: &PatternValues) -> Option<&str> {
        self.patterns
            .iter()
            .find(|p| &p.values == values)
            .map(|p| p.name.as_str())
    }

    /// Add a stroke preset if no duplicate exists. Returns Err with the existing name.
    pub fn try_add_stroke_preset(&mut self, preset: StrokePreset) -> Result<(), String> {
        if let Some(name) = self.matching_stroke_preset(&preset.values) {
            return Err(name.to_string());
        }
        self.strokes.push(preset);
        Ok(())
    }

    /// Add a pattern preset if no duplicate exists. Returns Err with the existing name.
    pub fn try_add_pattern_preset(&mut self, preset: PatternPreset) -> Result<(), String> {
        if let Some(name) = self.matching_pattern_preset(&preset.values) {
            return Err(name.to_string());
        }
        self.patterns.push(preset);
        Ok(())
    }

    /// Built-in default presets.
    pub fn built_in() -> PresetLibrary {
        PresetLibrary {
            strokes: vec![
                StrokePreset {
                    name: "flat_wide".to_string(),
                    values: StrokeValues {
                        brush_width: 40.0,
                        load: 0.8,
                        base_height: 0.5,
                        ridge_height: 0.3,
                        ridge_width: 5.0,
                        ridge_variation: 0.1,
                        body_wiggle: 0.15,
                        pressure_preset: PressurePreset::FadeOut,
                    },
                },
                StrokePreset {
                    name: "round_thin".to_string(),
                    values: StrokeValues {
                        brush_width: 15.0,
                        load: 0.9,
                        base_height: 0.6,
                        ridge_height: 0.2,
                        ridge_width: 3.0,
                        ridge_variation: 0.05,
                        body_wiggle: 0.1,
                        pressure_preset: PressurePreset::Taper,
                    },
                },
                StrokePreset {
                    name: "dry_brush".to_string(),
                    values: StrokeValues {
                        brush_width: 50.0,
                        load: 0.3,
                        base_height: 0.3,
                        ridge_height: 0.1,
                        ridge_width: 6.0,
                        ridge_variation: 0.2,
                        body_wiggle: 0.2,
                        pressure_preset: PressurePreset::FadeOut,
                    },
                },
                StrokePreset {
                    name: "impasto".to_string(),
                    values: StrokeValues {
                        brush_width: 30.0,
                        load: 1.0,
                        base_height: 0.8,
                        ridge_height: 0.5,
                        ridge_width: 4.0,
                        ridge_variation: 0.15,
                        body_wiggle: 0.1,
                        pressure_preset: PressurePreset::Bell,
                    },
                },
                StrokePreset {
                    name: "glaze".to_string(),
                    values: StrokeValues {
                        brush_width: 35.0,
                        load: 0.5,
                        base_height: 0.15,
                        ridge_height: 0.05,
                        ridge_width: 5.0,
                        ridge_variation: 0.05,
                        body_wiggle: 0.1,
                        pressure_preset: PressurePreset::Uniform,
                    },
                },
            ],
            patterns: vec![
                PatternPreset {
                    name: "uniform_horizontal".to_string(),
                    values: PatternValues {
                        guides: vec![GuideVertex {
                            position: Vec2::new(0.5, 0.5),
                            direction: Vec2::X,
                            influence: 1.0,
                        }],
                        stroke_spacing: 1.0,
                        max_stroke_length: 240.0,
                        angle_variation: 0.0,
                        max_turn_angle: 15.0,
                        color_break_threshold: None,
                        normal_break_threshold: None,
                        overlap_ratio: None,
                        overlap_dist_factor: None,
                        color_variation: 0.1,
                    },
                },
                PatternPreset {
                    name: "crosshatch".to_string(),
                    values: PatternValues {
                        guides: vec![
                            GuideVertex {
                                position: Vec2::new(0.3, 0.5),
                                direction: Vec2::new(1.0, 0.3).normalize(),
                                influence: 0.6,
                            },
                            GuideVertex {
                                position: Vec2::new(0.7, 0.5),
                                direction: Vec2::new(-0.3, 1.0).normalize(),
                                influence: 0.6,
                            },
                        ],
                        stroke_spacing: 0.8,
                        max_stroke_length: 120.0,
                        angle_variation: 5.0,
                        max_turn_angle: 20.0,
                        color_break_threshold: None,
                        normal_break_threshold: None,
                        overlap_ratio: None,
                        overlap_dist_factor: None,
                        color_variation: 0.1,
                    },
                },
                PatternPreset {
                    name: "loose_organic".to_string(),
                    values: PatternValues {
                        guides: vec![GuideVertex {
                            position: Vec2::new(0.5, 0.5),
                            direction: Vec2::X,
                            influence: 0.5,
                        }],
                        stroke_spacing: 1.2,
                        max_stroke_length: 300.0,
                        angle_variation: 15.0,
                        max_turn_angle: 30.0,
                        color_break_threshold: None,
                        normal_break_threshold: None,
                        overlap_ratio: None,
                        overlap_dist_factor: None,
                        color_variation: 0.15,
                    },
                },
                PatternPreset {
                    name: "tight_fill".to_string(),
                    values: PatternValues {
                        guides: vec![GuideVertex {
                            position: Vec2::new(0.5, 0.5),
                            direction: Vec2::X,
                            influence: 0.8,
                        }],
                        stroke_spacing: 0.6,
                        max_stroke_length: 180.0,
                        angle_variation: 3.0,
                        max_turn_angle: 15.0,
                        color_break_threshold: None,
                        normal_break_threshold: None,
                        overlap_ratio: Some(0.8),
                        overlap_dist_factor: Some(0.2),
                        color_variation: 0.08,
                    },
                },
            ],
        }
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
        let idx = match self
            .cumulative_lengths
            .binary_search_by(|v| v.partial_cmp(&distance).unwrap())
        {
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
            return *self.points.last().unwrap();
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

/// Global output settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
            normal_strength: 1.0,
            normal_mode: NormalMode::default(),
            background_mode: BackgroundMode::default(),
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
    fn resolution_preset_values() {
        assert_eq!(ResolutionPreset::Preview.resolution(), 512);
        assert_eq!(ResolutionPreset::Standard.resolution(), 1024);
        assert_eq!(ResolutionPreset::High.resolution(), 2048);
        assert_eq!(ResolutionPreset::Ultra.resolution(), 4096);
    }
}

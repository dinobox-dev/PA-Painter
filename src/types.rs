use glam::Vec2;
use serde::{Deserialize, Serialize};

// ── Color Types ──

/// RGBA color in linear float [0, 1] per channel.
/// This is the canonical color type for the entire project.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
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

// ── Enums ──

/// Pressure curve preset determining how brush pressure varies along a stroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PressurePreset {
    Uniform,
    FadeOut,
    FadeIn,
    Bell,
    Taper,
}

/// Per-layer stroke parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
            seed: 42,
        }
    }
}

/// Direction guide vertex placed by the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaintLayer {
    pub name: String,
    pub order: i32,
    pub params: StrokeParams,
    pub guides: Vec<GuideVertex>,
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
    pub output_resolution: u32,
    pub normal_strength: f32,
}

impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            resolution_preset: ResolutionPreset::Standard,
            output_resolution: 1024,
            normal_strength: 1.0,
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

use crate::types::Color;
use glam::Vec2;

/// Hermite smoothstep: 0 at edge0, 1 at edge1, smooth transition.
/// Argument order follows GLSL convention: (edge0, edge1, x).
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge1 <= edge0 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Linear interpolation.
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Rotate a 2D vector by angle in radians.
pub fn rotate_vec2(v: Vec2, angle_rad: f32) -> Vec2 {
    let (s, c) = angle_rad.sin_cos();
    Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c)
}

/// Perpendicular vector (90° counter-clockwise).
pub fn perpendicular(v: Vec2) -> Vec2 {
    Vec2::new(-v.y, v.x)
}

/// Linearly interpolate two Colors channel-wise.
pub fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: lerp(a.r, b.r, t),
        g: lerp(a.g, b.g, t),
        b: lerp(a.b, b.b, t),
        a: lerp(a.a, b.a, t),
    }
}

/// Bilinear sample a row-major Color texture at UV coordinates.
/// Uses half-texel offset for texel-center convention, edge-clamped.
pub fn sample_bilinear_color(texture: &[Color], width: u32, height: u32, uv: Vec2) -> Color {
    let x = uv.x * width as f32 - 0.5;
    let y = uv.y * height as f32 - 0.5;

    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;

    let x0c = x0.clamp(0, width as i32 - 1) as u32;
    let y0c = y0.clamp(0, height as i32 - 1) as u32;
    let x1c = (x0 + 1).clamp(0, width as i32 - 1) as u32;
    let y1c = (y0 + 1).clamp(0, height as i32 - 1) as u32;

    let c00 = texture[(y0c * width + x0c) as usize];
    let c10 = texture[(y0c * width + x1c) as usize];
    let c01 = texture[(y1c * width + x0c) as usize];
    let c11 = texture[(y1c * width + x1c) as usize];

    let top = lerp_color(c00, c10, fx);
    let bot = lerp_color(c01, c11, fx);
    lerp_color(top, bot, fy)
}

/// Bilinear sample a row-major f32 map at UV coordinates.
/// Uses half-texel offset for texel-center convention, edge-clamped.
pub fn sample_bilinear_f32(data: &[f32], width: u32, height: u32, uv: Vec2) -> f32 {
    let x = uv.x * width as f32 - 0.5;
    let y = uv.y * height as f32 - 0.5;

    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;

    let x0c = x0.clamp(0, width as i32 - 1) as u32;
    let y0c = y0.clamp(0, height as i32 - 1) as u32;
    let x1c = (x0 + 1).clamp(0, width as i32 - 1) as u32;
    let y1c = (y0 + 1).clamp(0, height as i32 - 1) as u32;

    let v00 = data[(y0c * width + x0c) as usize];
    let v10 = data[(y0c * width + x1c) as usize];
    let v01 = data[(y1c * width + x0c) as usize];
    let v11 = data[(y1c * width + x1c) as usize];

    v00 * (1.0 - fx) * (1.0 - fy) + v10 * fx * (1.0 - fy)
        + v01 * (1.0 - fx) * fy + v11 * fx * fy
}

/// Linearly interpolate a 1D array at fractional index.
pub fn interpolate_array(arr: &[f32], index: f32) -> f32 {
    if arr.is_empty() {
        return 0.0;
    }
    let i = index.floor() as usize;
    let frac = index - index.floor();
    if i >= arr.len() - 1 {
        return arr[arr.len() - 1];
    }
    lerp(arr[i], arr[i + 1], frac)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    const EPS: f32 = 1e-5;

    #[test]
    fn smoothstep_midpoint() {
        assert!((smoothstep(0.0, 1.0, 0.5) - 0.5).abs() < EPS);
    }

    #[test]
    fn smoothstep_edges() {
        assert!((smoothstep(0.0, 1.0, 0.0) - 0.0).abs() < EPS);
        assert!((smoothstep(0.0, 1.0, 1.0) - 1.0).abs() < EPS);
    }

    #[test]
    fn smoothstep_clamped() {
        assert!((smoothstep(0.0, 1.0, -1.0) - 0.0).abs() < EPS);
        assert!((smoothstep(0.0, 1.0, 2.0) - 1.0).abs() < EPS);
    }

    #[test]
    fn rotate_vec2_90_degrees() {
        let v = Vec2::new(1.0, 0.0);
        let rotated = rotate_vec2(v, FRAC_PI_2);
        assert!((rotated.x - 0.0).abs() < EPS);
        assert!((rotated.y - 1.0).abs() < EPS);
    }

    #[test]
    fn perpendicular_x_axis() {
        let v = Vec2::new(1.0, 0.0);
        let p = perpendicular(v);
        assert!((p.x - 0.0).abs() < EPS);
        assert!((p.y - 1.0).abs() < EPS);
    }

    #[test]
    fn interpolate_array_midpoint() {
        assert!((interpolate_array(&[0.0, 1.0], 0.5) - 0.5).abs() < EPS);
    }

    #[test]
    fn interpolate_array_empty() {
        assert!((interpolate_array(&[], 0.5) - 0.0).abs() < EPS);
    }

    #[test]
    fn interpolate_array_beyond_end() {
        assert!((interpolate_array(&[0.0, 1.0, 2.0], 5.0) - 2.0).abs() < EPS);
    }

    #[test]
    fn lerp_color_midpoint() {
        let red = Color::new(1.0, 0.0, 0.0, 1.0);
        let blue = Color::new(0.0, 0.0, 1.0, 1.0);
        let mid = lerp_color(red, blue, 0.5);
        assert!((mid.r - 0.5).abs() < EPS);
        assert!((mid.g - 0.0).abs() < EPS);
        assert!((mid.b - 0.5).abs() < EPS);
        assert!((mid.a - 1.0).abs() < EPS);
    }

    #[test]
    fn lerp_color_at_zero() {
        let a = Color::new(0.2, 0.3, 0.4, 0.5);
        let b = Color::new(0.8, 0.7, 0.6, 1.0);
        let result = lerp_color(a, b, 0.0);
        assert!((result.r - a.r).abs() < EPS);
        assert!((result.g - a.g).abs() < EPS);
        assert!((result.b - a.b).abs() < EPS);
        assert!((result.a - a.a).abs() < EPS);
    }

    #[test]
    fn lerp_color_at_one() {
        let a = Color::new(0.2, 0.3, 0.4, 0.5);
        let b = Color::new(0.8, 0.7, 0.6, 1.0);
        let result = lerp_color(a, b, 1.0);
        assert!((result.r - b.r).abs() < EPS);
        assert!((result.g - b.g).abs() < EPS);
        assert!((result.b - b.b).abs() < EPS);
        assert!((result.a - b.a).abs() < EPS);
    }
}

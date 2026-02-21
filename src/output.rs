use std::path::Path;

use crate::asset_io::linear_to_srgb;
use crate::compositing::GlobalMaps;
use crate::types::{Color, OutputSettings, PaintLayer};

// ── Error Type ──

/// Error type for output operations.
#[derive(Debug)]
pub enum OutputError {
    Io(std::io::Error),
    ImageError(image::ImageError),
    ExrError(String),
}

impl From<std::io::Error> for OutputError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<image::ImageError> for OutputError {
    fn from(e: image::ImageError) -> Self {
        Self::ImageError(e)
    }
}

impl std::fmt::Display for OutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputError::Io(e) => write!(f, "IO error: {e}"),
            OutputError::ImageError(e) => write!(f, "Image error: {e}"),
            OutputError::ExrError(s) => write!(f, "EXR error: {s}"),
        }
    }
}

impl std::error::Error for OutputError {}

// ── Export Format ──

/// Output format selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Png,
    Exr,
}

// ── Height Map Normalization ──

/// Normalize a height map for export.
///
/// Uses the maximum possible height (base_height + ridge_height across all layers)
/// as a reference, with display_cap = 2x that value.
///
/// Returns a new Vec<f32> with values in [0, 1].
pub fn normalize_height_map(height: &[f32], layers: &[PaintLayer]) -> Vec<f32> {
    let mut max_possible: f32 = 0.0;
    for layer in layers {
        let layer_max = layer.params.base_height + layer.params.ridge_height;
        max_possible = max_possible.max(layer_max);
    }

    let mut display_cap = 2.0 * max_possible;
    if display_cap <= 0.0 {
        display_cap = 1.0;
    }

    height
        .iter()
        .map(|&h| (h / display_cap).clamp(0.0, 1.0))
        .collect()
}

// ── Normal Map Generation (Sobel Filter) ──

/// Generate a tangent-space normal map from a height map.
///
/// - `height`: normalized height map [0, 1], row-major, resolution x resolution
/// - `resolution`: map dimensions (square)
/// - `normal_strength`: depth intensity multiplier (default 1.0)
///
/// Returns Vec<[f32; 3]> of normal vectors encoded as (R, G, B) where
/// flat = (0.5, 0.5, 1.0). Row-major, resolution x resolution.
pub fn generate_normal_map(
    height: &[f32],
    resolution: u32,
    normal_strength: f32,
) -> Vec<[f32; 3]> {
    let res = resolution;
    let mut normals = vec![[0.5f32, 0.5, 1.0]; (res * res) as usize];

    let idx = |x: u32, y: u32| -> usize { (y * res + x) as usize };

    for y in 1..(res - 1) {
        for x in 1..(res - 1) {
            // Sobel X kernel:  [-1 0 1]
            //                  [-2 0 2]
            //                  [-1 0 1]
            let gx = -height[idx(x - 1, y - 1)]
                - 2.0 * height[idx(x - 1, y)]
                - height[idx(x - 1, y + 1)]
                + height[idx(x + 1, y - 1)]
                + 2.0 * height[idx(x + 1, y)]
                + height[idx(x + 1, y + 1)];

            // Sobel Y kernel:  [-1 -2 -1]
            //                  [ 0  0  0]
            //                  [ 1  2  1]
            let gy = -height[idx(x - 1, y - 1)]
                - 2.0 * height[idx(x, y - 1)]
                - height[idx(x + 1, y - 1)]
                + height[idx(x - 1, y + 1)]
                + 2.0 * height[idx(x, y + 1)]
                + height[idx(x + 1, y + 1)];

            let nx = -gx * normal_strength;
            let ny = -gy * normal_strength;
            let nz = 1.0f32;

            let len = (nx * nx + ny * ny + nz * nz).sqrt();
            let nx = nx / len;
            let ny = ny / len;
            let nz = nz / len;

            normals[idx(x, y)] = [nx * 0.5 + 0.5, ny * 0.5 + 0.5, nz * 0.5 + 0.5];
        }
    }

    normals
}

// ── PNG Export Functions ──

/// Export a height map as a grayscale PNG (linear, no gamma).
pub fn export_height_png(
    height: &[f32],
    resolution: u32,
    path: &Path,
) -> Result<(), OutputError> {
    let pixels: Vec<u8> = height
        .iter()
        .map(|&h| (h.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect();

    image::save_buffer(path, &pixels, resolution, resolution, image::ColorType::L8)?;
    Ok(())
}

/// Export a color map as an sRGB PNG.
/// Applies linear_to_srgb() per RGB channel before writing.
pub fn export_color_png(
    color: &[Color],
    resolution: u32,
    path: &Path,
) -> Result<(), OutputError> {
    let pixels: Vec<u8> = color
        .iter()
        .flat_map(|c| {
            [
                (linear_to_srgb(c.r.clamp(0.0, 1.0)) * 255.0).round() as u8,
                (linear_to_srgb(c.g.clamp(0.0, 1.0)) * 255.0).round() as u8,
                (linear_to_srgb(c.b.clamp(0.0, 1.0)) * 255.0).round() as u8,
            ]
        })
        .collect();

    image::save_buffer(
        path,
        &pixels,
        resolution,
        resolution,
        image::ColorType::Rgb8,
    )?;
    Ok(())
}

/// Export a normal map as an RGB PNG (linear, no gamma).
pub fn export_normal_png(
    normals: &[[f32; 3]],
    resolution: u32,
    path: &Path,
) -> Result<(), OutputError> {
    let pixels: Vec<u8> = normals
        .iter()
        .flat_map(|n| {
            [
                (n[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                (n[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                (n[2].clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        })
        .collect();

    image::save_buffer(
        path,
        &pixels,
        resolution,
        resolution,
        image::ColorType::Rgb8,
    )?;
    Ok(())
}

/// Export a stroke ID map as an RGB PNG where each unique stroke gets a distinct color.
/// Uses golden-angle hue spacing so adjacent stroke IDs receive visually dissimilar colors.
/// ID 0 (unpainted) is black.
pub fn export_stroke_id_png(
    ids: &[u32],
    resolution: u32,
    path: &Path,
) -> Result<(), OutputError> {
    let mut unique: Vec<u32> = ids.iter().copied().filter(|&id| id != 0).collect();
    unique.sort_unstable();
    unique.dedup();

    // Golden angle ≈ 137.508° ensures maximum hue separation between consecutive indices.
    const GOLDEN_ANGLE: f32 = 137.508;

    let id_to_rgb: std::collections::HashMap<u32, [u8; 3]> = unique
        .iter()
        .enumerate()
        .map(|(i, &id)| {
            let hue = (i as f32 * GOLDEN_ANGLE) % 360.0;
            // Alternate saturation/value to further separate neighbors
            let sat = if i % 2 == 0 { 0.9 } else { 0.65 };
            let val = if (i / 2) % 2 == 0 { 1.0 } else { 0.75 };
            (id, hsv_to_rgb_bytes(hue, sat, val))
        })
        .collect();

    let mut pixels = Vec::with_capacity(ids.len() * 3);
    for &id in ids {
        if id == 0 {
            pixels.extend_from_slice(&[0, 0, 0]);
        } else {
            pixels.extend_from_slice(&id_to_rgb[&id]);
        }
    }

    image::save_buffer(path, &pixels, resolution, resolution, image::ColorType::Rgb8)?;
    Ok(())
}

/// Convert HSV (h: 0..360, s: 0..1, v: 0..1) to sRGB bytes.
fn hsv_to_rgb_bytes(h: f32, s: f32, v: f32) -> [u8; 3] {
    let c = v * s;
    let h2 = h / 60.0;
    let x = c * (1.0 - (h2 % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match h2 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    ]
}

// ── EXR Export Functions ──

/// Export a height map as a single-channel float32 EXR (linear).
pub fn export_height_exr(
    height: &[f32],
    resolution: u32,
    path: &Path,
) -> Result<(), OutputError> {
    use exr::prelude::*;

    let res = resolution as usize;

    write_rgb_file(
        path,
        res,
        res,
        |x, y| {
            let h = height[y * res + x];
            (h, h, h)
        },
    )
    .map_err(|e| OutputError::ExrError(e.to_string()))?;

    Ok(())
}

/// Export a color map as a 3-channel float32 EXR (linear, NO sRGB conversion).
/// EXR consumers expect linear data.
pub fn export_color_exr(
    color: &[Color],
    resolution: u32,
    path: &Path,
) -> Result<(), OutputError> {
    use exr::prelude::*;

    let res = resolution as usize;

    write_rgb_file(
        path,
        res,
        res,
        |x, y| {
            let c = &color[y * res + x];
            (c.r, c.g, c.b)
        },
    )
    .map_err(|e| OutputError::ExrError(e.to_string()))?;

    Ok(())
}

// ── Export All ──

/// Export all textures at once.
pub fn export_all(
    global: &GlobalMaps,
    layers: &[PaintLayer],
    settings: &OutputSettings,
    output_dir: &Path,
    format: ExportFormat,
) -> Result<(), OutputError> {
    std::fs::create_dir_all(output_dir)?;

    let normalized_height = normalize_height_map(&global.height, layers);
    let normals = generate_normal_map(&normalized_height, global.resolution, settings.normal_strength);

    match format {
        ExportFormat::Png => {
            export_color_png(
                &global.color,
                global.resolution,
                &output_dir.join("color_map.png"),
            )?;
            export_height_png(
                &normalized_height,
                global.resolution,
                &output_dir.join("height_map.png"),
            )?;
        }
        ExportFormat::Exr => {
            export_color_exr(
                &global.color,
                global.resolution,
                &output_dir.join("color_map.exr"),
            )?;
            export_height_exr(
                &normalized_height,
                global.resolution,
                &output_dir.join("height_map.exr"),
            )?;
        }
    }

    // Normal map and stroke ID are always PNG
    export_normal_png(
        &normals,
        global.resolution,
        &output_dir.join("normal_map.png"),
    )?;
    export_stroke_id_png(
        &global.stroke_id,
        global.resolution,
        &output_dir.join("stroke_id_map.png"),
    )?;

    Ok(())
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_io::{linear_to_srgb, load_texture};
    use crate::compositing::composite_all;
    use crate::types::{
        Color, GuideVertex, OutputSettings, PaintLayer, StrokeParams,
    };
    use glam::Vec2;

    const EPS: f32 = 1e-4;

    fn make_layer_with_order(order: i32) -> PaintLayer {
        PaintLayer {
            name: format!("layer_{}", order),
            order,
            params: StrokeParams::default(),
            guides: vec![GuideVertex {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::X,
                influence: 1.5,
            }],
        }
    }

    // ── Height Normalization Tests ──

    #[test]
    fn normalize_all_zeros() {
        let height = vec![0.0f32; 64];
        let layer = make_layer_with_order(0);
        let normalized = normalize_height_map(&height, &[layer]);
        assert!(normalized.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn normalize_at_display_cap() {
        // max_possible = 0.5 + 0.3 = 0.8, display_cap = 1.6
        let mut layer = make_layer_with_order(0);
        layer.params.base_height = 0.5;
        layer.params.ridge_height = 0.3;

        let height = vec![1.6f32; 1]; // exactly at display_cap
        let normalized = normalize_height_map(&height, &[layer]);
        assert!(
            (normalized[0] - 1.0).abs() < EPS,
            "at display_cap: expected 1.0, got {}",
            normalized[0]
        );
    }

    #[test]
    fn normalize_half_of_cap() {
        let mut layer = make_layer_with_order(0);
        layer.params.base_height = 0.5;
        layer.params.ridge_height = 0.3;
        // display_cap = 1.6

        let height = vec![0.8f32; 1]; // half of 1.6
        let normalized = normalize_height_map(&height, &[layer]);
        assert!(
            (normalized[0] - 0.5).abs() < EPS,
            "half of cap: expected 0.5, got {}",
            normalized[0]
        );
    }

    #[test]
    fn normalize_no_layers() {
        let height = vec![0.5f32; 4];
        let normalized = normalize_height_map(&height, &[]);
        // display_cap = 1.0 (fallback)
        assert!(
            (normalized[0] - 0.5).abs() < EPS,
            "no layers: expected 0.5, got {}",
            normalized[0]
        );
    }

    #[test]
    fn normalize_clamps_above_cap() {
        let mut layer = make_layer_with_order(0);
        layer.params.base_height = 0.5;
        layer.params.ridge_height = 0.3;
        // display_cap = 1.6

        let height = vec![3.0f32; 1]; // above display_cap
        let normalized = normalize_height_map(&height, &[layer]);
        assert!(
            (normalized[0] - 1.0).abs() < EPS,
            "above cap: expected 1.0, got {}",
            normalized[0]
        );
    }

    // ── Normal Map Tests ──

    #[test]
    fn normal_flat_surface() {
        let res = 16u32;
        let height = vec![0.5f32; (res * res) as usize];
        let normals = generate_normal_map(&height, res, 1.0);

        // All interior pixels should be flat
        for y in 1..(res - 1) {
            for x in 1..(res - 1) {
                let n = normals[(y * res + x) as usize];
                assert!(
                    (n[0] - 0.5).abs() < EPS && (n[1] - 0.5).abs() < EPS && (n[2] - 1.0).abs() < EPS,
                    "flat surface at ({x},{y}): expected (0.5, 0.5, 1.0), got ({:.4}, {:.4}, {:.4})",
                    n[0], n[1], n[2]
                );
            }
        }
    }

    #[test]
    fn normal_slope_right() {
        // Height increases left to right → normal tilts left (nx < 0.5)
        let res = 16u32;
        let mut height = vec![0.0f32; (res * res) as usize];
        for y in 0..res {
            for x in 0..res {
                height[(y * res + x) as usize] = x as f32 / res as f32;
            }
        }

        let normals = generate_normal_map(&height, res, 1.0);
        // Check center pixel
        let center = normals[(8 * res + 8) as usize];
        assert!(
            center[0] < 0.5,
            "slope right: nx should be < 0.5 (tilts left), got {:.4}",
            center[0]
        );
    }

    #[test]
    fn normal_slope_up() {
        // Height increases bottom (y=0, V=0) to top (y=res, V=1) → ny < 0.5
        let res = 16u32;
        let mut height = vec![0.0f32; (res * res) as usize];
        for y in 0..res {
            for x in 0..res {
                // In UV space, V=0 is bottom (row 0), V=1 is top (row res-1)
                // "Height increases bottom to top" = height grows with y
                height[(y * res + x) as usize] = y as f32 / res as f32;
            }
        }

        let normals = generate_normal_map(&height, res, 1.0);
        let center = normals[(8 * res + 8) as usize];
        assert!(
            center[1] < 0.5,
            "slope up: ny should be < 0.5, got {:.4}",
            center[1]
        );
    }

    #[test]
    fn normal_strength_zero() {
        let res = 16u32;
        let mut height = vec![0.0f32; (res * res) as usize];
        for y in 0..res {
            for x in 0..res {
                height[(y * res + x) as usize] = x as f32 / res as f32;
            }
        }

        let normals = generate_normal_map(&height, res, 0.0);
        // All should be near-flat
        for y in 1..(res - 1) {
            for x in 1..(res - 1) {
                let n = normals[(y * res + x) as usize];
                assert!(
                    (n[0] - 0.5).abs() < EPS && (n[1] - 0.5).abs() < EPS,
                    "strength=0 at ({x},{y}): expected near (0.5, 0.5, 1.0), got ({:.4}, {:.4}, {:.4})",
                    n[0], n[1], n[2]
                );
            }
        }
    }

    #[test]
    fn normal_strength_scales() {
        let res = 16u32;
        let mut height = vec![0.0f32; (res * res) as usize];
        for y in 0..res {
            for x in 0..res {
                height[(y * res + x) as usize] = x as f32 / res as f32;
            }
        }

        let normals_1 = generate_normal_map(&height, res, 1.0);
        let normals_2 = generate_normal_map(&height, res, 2.0);

        let n1 = normals_1[(8 * res + 8) as usize];
        let n2 = normals_2[(8 * res + 8) as usize];

        // strength=2 should tilt more (further from 0.5 in x)
        let deviation_1 = (n1[0] - 0.5).abs();
        let deviation_2 = (n2[0] - 0.5).abs();
        assert!(
            deviation_2 > deviation_1,
            "strength=2 should have more deviation: s1={:.4}, s2={:.4}",
            deviation_1, deviation_2
        );
    }

    #[test]
    fn normal_edge_pixels_flat() {
        let res = 16u32;
        let mut height = vec![0.0f32; (res * res) as usize];
        for i in 0..height.len() {
            height[i] = (i as f32) / height.len() as f32;
        }

        let normals = generate_normal_map(&height, res, 1.0);

        // Check edge pixels
        for x in 0..res {
            let top = normals[(0 * res + x) as usize];
            let bottom = normals[((res - 1) * res + x) as usize];
            assert!(
                (top[0] - 0.5).abs() < EPS && (top[1] - 0.5).abs() < EPS && (top[2] - 1.0).abs() < EPS,
                "top edge pixel ({x},0) should be flat"
            );
            assert!(
                (bottom[0] - 0.5).abs() < EPS && (bottom[1] - 0.5).abs() < EPS && (bottom[2] - 1.0).abs() < EPS,
                "bottom edge pixel ({x},{}) should be flat", res - 1
            );
        }
        for y in 0..res {
            let left = normals[(y * res + 0) as usize];
            let right = normals[(y * res + res - 1) as usize];
            assert!(
                (left[0] - 0.5).abs() < EPS && (left[1] - 0.5).abs() < EPS && (left[2] - 1.0).abs() < EPS,
                "left edge pixel (0,{y}) should be flat"
            );
            assert!(
                (right[0] - 0.5).abs() < EPS && (right[1] - 0.5).abs() < EPS && (right[2] - 1.0).abs() < EPS,
                "right edge pixel ({},{y}) should be flat", res - 1
            );
        }
    }

    #[test]
    fn normal_unit_length() {
        let res = 32u32;
        let mut height = vec![0.0f32; (res * res) as usize];
        for y in 0..res {
            for x in 0..res {
                height[(y * res + x) as usize] =
                    ((x as f32 * 0.5).sin() * 0.5 + 0.5) * ((y as f32 * 0.3).cos() * 0.5 + 0.5);
            }
        }

        let normals = generate_normal_map(&height, res, 1.5);

        for y in 1..(res - 1) {
            for x in 1..(res - 1) {
                let n = normals[(y * res + x) as usize];
                // Decode from [0,1] to [-1,1]
                let dx = n[0] * 2.0 - 1.0;
                let dy = n[1] * 2.0 - 1.0;
                let dz = n[2] * 2.0 - 1.0;
                let len = (dx * dx + dy * dy + dz * dz).sqrt();
                assert!(
                    (len - 1.0).abs() < 0.01,
                    "normal at ({x},{y}) not unit length: {len:.4}"
                );
            }
        }
    }

    // ── PNG Export Tests ──

    #[test]
    fn png_dimensions() {
        let res = 128u32;
        let height = vec![0.5f32; (res * res) as usize];
        let dir = std::env::temp_dir().join("pap_test_output");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("dim_test_height.png");

        export_height_png(&height, res, &path).unwrap();

        let img = image::open(&path).unwrap();
        assert_eq!(img.width(), 128);
        assert_eq!(img.height(), 128);
    }

    #[test]
    fn color_png_srgb() {
        let res = 4u32;
        let linear_val = 0.5f32; // known linear value
        let colors = vec![Color::rgb(linear_val, linear_val, linear_val); (res * res) as usize];

        let dir = std::env::temp_dir().join("pap_test_output");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("srgb_test.png");

        export_color_png(&colors, res, &path).unwrap();

        let img = image::open(&path).unwrap().to_rgb8();
        let pixel = img.get_pixel(0, 0);
        let expected = (linear_to_srgb(linear_val) * 255.0).round() as u8;
        assert_eq!(
            pixel[0], expected,
            "sRGB conversion: expected {expected}, got {}",
            pixel[0]
        );
    }

    #[test]
    fn height_png_range() {
        let res = 4u32;
        let mut height = vec![0.0f32; (res * res) as usize];
        height[0] = 0.0; // black
        height[1] = 1.0; // white

        let dir = std::env::temp_dir().join("pap_test_output");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("range_test.png");

        export_height_png(&height, res, &path).unwrap();

        let img = image::open(&path).unwrap().to_luma8();
        assert_eq!(img.get_pixel(0, 0)[0], 0, "0.0 should map to black");
        assert_eq!(img.get_pixel(1, 0)[0], 255, "1.0 should map to white");
    }

    #[test]
    fn normal_png_flat() {
        let res = 4u32;
        let normals = vec![[0.5f32, 0.5, 1.0]; (res * res) as usize];

        let dir = std::env::temp_dir().join("pap_test_output");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("normal_flat_test.png");

        export_normal_png(&normals, res, &path).unwrap();

        let img = image::open(&path).unwrap().to_rgb8();
        let pixel = img.get_pixel(0, 0);
        assert_eq!(pixel[0], 128, "flat normal R should be 128, got {}", pixel[0]);
        assert_eq!(pixel[1], 128, "flat normal G should be 128, got {}", pixel[1]);
        assert_eq!(pixel[2], 255, "flat normal B should be 255, got {}", pixel[2]);
    }

    #[test]
    fn stroke_id_png_mapping() {
        let res = 4u32;
        let mut ids = vec![0u32; (res * res) as usize];
        ids[0] = 0;
        ids[1] = 5;
        ids[2] = 10;

        let dir = std::env::temp_dir().join("pap_test_output");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("stroke_id_test.png");

        export_stroke_id_png(&ids, res, &path).unwrap();

        let img = image::open(&path).unwrap().to_rgb8();
        // ID 0 → black
        let p0 = img.get_pixel(0, 0);
        assert_eq!(p0[0], 0, "ID 0 R should be 0");
        assert_eq!(p0[1], 0, "ID 0 G should be 0");
        assert_eq!(p0[2], 0, "ID 0 B should be 0");
        // ID 5 and ID 10 should be different non-black colors
        let p1 = img.get_pixel(1, 0);
        let p2 = img.get_pixel(2, 0);
        assert!(p1[0] > 0 || p1[1] > 0 || p1[2] > 0, "ID 5 should not be black");
        assert!(p2[0] > 0 || p2[1] > 0 || p2[2] > 0, "ID 10 should not be black");
        assert_ne!([p1[0], p1[1], p1[2]], [p2[0], p2[1], p2[2]], "different IDs should have different colors");
    }

    // ── EXR Export Tests ──

    #[test]
    fn exr_height_dimensions_and_values() {
        let res = 16u32;
        let mut height = vec![0.0f32; (res * res) as usize];
        height[0] = 0.0;
        height[1] = 0.5;
        height[2] = 1.0;

        let dir = std::env::temp_dir().join("pap_test_output");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("height_test.exr");

        export_height_exr(&height, res, &path).unwrap();

        // Read back
        let tex = load_texture(&path).unwrap();
        assert_eq!(tex.width, 16);
        assert_eq!(tex.height, 16);

        // Values should be preserved (written as RGB with R=G=B=height)
        assert!(
            (tex.pixels[0][0] - 0.0).abs() < EPS,
            "height[0]: expected 0.0, got {}",
            tex.pixels[0][0]
        );
        assert!(
            (tex.pixels[1][0] - 0.5).abs() < EPS,
            "height[1]: expected 0.5, got {}",
            tex.pixels[1][0]
        );
        assert!(
            (tex.pixels[2][0] - 1.0).abs() < EPS,
            "height[2]: expected 1.0, got {}",
            tex.pixels[2][0]
        );
    }

    #[test]
    fn exr_color_linear() {
        let res = 4u32;
        let known_color = Color::rgb(0.3, 0.6, 0.9);
        let colors = vec![known_color; (res * res) as usize];

        let dir = std::env::temp_dir().join("pap_test_output");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("color_linear_test.exr");

        export_color_exr(&colors, res, &path).unwrap();

        // Read back
        let tex = load_texture(&path).unwrap();
        assert_eq!(tex.width, 4);
        assert_eq!(tex.height, 4);

        // Values should be written as-is (no sRGB conversion)
        let p = tex.pixels[0];
        assert!(
            (p[0] - 0.3).abs() < EPS,
            "R: expected 0.3, got {}",
            p[0]
        );
        assert!(
            (p[1] - 0.6).abs() < EPS,
            "G: expected 0.6, got {}",
            p[1]
        );
        assert!(
            (p[2] - 0.9).abs() < EPS,
            "B: expected 0.9, got {}",
            p[2]
        );
    }

    // ── Export All Test ──

    #[test]
    fn export_all_png() {
        let res = 32u32;
        let mut layer = make_layer_with_order(0);
        layer.params.brush_width = 10.0;

        let settings = OutputSettings::default();

        let maps = composite_all(
            &[layer.clone()],
            res,
            None,
            0,
            0,
            Color::rgb(0.5, 0.5, 0.5),
            &settings,
        );

        let dir = std::env::temp_dir().join("pap_test_output").join("export_all_png");
        export_all(&maps, &[layer], &settings, &dir, ExportFormat::Png).unwrap();

        // Verify files exist
        assert!(dir.join("color_map.png").exists(), "color_map.png should exist");
        assert!(dir.join("height_map.png").exists(), "height_map.png should exist");
        assert!(dir.join("normal_map.png").exists(), "normal_map.png should exist");
        assert!(dir.join("stroke_id_map.png").exists(), "stroke_id_map.png should exist");
    }

    #[test]
    fn export_all_exr() {
        let res = 32u32;
        let mut layer = make_layer_with_order(0);
        layer.params.brush_width = 10.0;

        let settings = OutputSettings::default();

        let maps = composite_all(
            &[layer.clone()],
            res,
            None,
            0,
            0,
            Color::rgb(0.5, 0.5, 0.5),
            &settings,
        );

        let dir = std::env::temp_dir().join("pap_test_output").join("export_all_exr");
        export_all(&maps, &[layer], &settings, &dir, ExportFormat::Exr).unwrap();

        assert!(dir.join("color_map.exr").exists(), "color_map.exr should exist");
        assert!(dir.join("height_map.exr").exists(), "height_map.exr should exist");
        assert!(dir.join("normal_map.png").exists(), "normal_map.png should exist");
        assert!(dir.join("stroke_id_map.png").exists(), "stroke_id_map.png should exist");
    }

    // ── Visual Integration Test ──

    #[test]
    fn visual_full_output() {
        let mut layer = make_layer_with_order(0);
        layer.params.brush_width = 25.0;
        layer.params.ridge_height = 0.3;
        layer.params.color_variation = 0.1;

        let settings = OutputSettings::default();

        let solid = Color::rgb(0.6, 0.4, 0.3);
        let maps = composite_all(&[layer.clone()], 256, None, 0, 0, solid, &settings);

        let dir = crate::test_module_output_dir("export");
        let _ = std::fs::create_dir_all(&dir);

        export_all(&maps, &[layer], &settings, &dir, ExportFormat::Png).unwrap();

        eprintln!("Wrote: {}/color_map.png", dir.display());
        eprintln!("Wrote: {}/height_map.png", dir.display());
        eprintln!("Wrote: {}/normal_map.png", dir.display());
        eprintln!("Wrote: {}/stroke_id_map.png", dir.display());
    }
}

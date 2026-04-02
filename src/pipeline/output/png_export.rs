//! PNG export functions for height, color, normal, stroke ID, and stroke time maps.

use std::path::Path;

use log::debug;

use super::normal_map::normals_to_pixels;
use super::OutputError;
use crate::mesh::asset_io::linear_to_srgb;
use crate::types::{Color, HsvColor, NormalYConvention};
use crate::util::stroke_color::hsv_to_rgb;

/// Export a height map as a grayscale PNG (linear, no gamma).
pub fn export_height_png(height: &[f32], resolution: u32, path: &Path) -> Result<(), OutputError> {
    debug!("Exporting height PNG: {}", path.display());
    let pixels: Vec<u8> = height
        .iter()
        .map(|&h| (h.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect();

    image::save_buffer(path, &pixels, resolution, resolution, image::ColorType::L8)?;
    Ok(())
}

/// Export a color map as an sRGB PNG.
/// Applies linear_to_srgb() per RGB channel before writing.
/// When `with_alpha` is true, outputs RGBA8 with the alpha channel from Color.
///
/// Internal color data is premultiplied alpha; this function un-premultiplies
/// before writing so the PNG stores straight (non-premultiplied) RGBA.
pub fn export_color_png(
    color: &[Color],
    resolution: u32,
    path: &Path,
    with_alpha: bool,
) -> Result<(), OutputError> {
    debug!(
        "Exporting color PNG: {} (alpha={})",
        path.display(),
        with_alpha
    );
    if with_alpha {
        let pixels: Vec<u8> = color
            .iter()
            .flat_map(|c| {
                let a = c.a.clamp(0.0, 1.0);
                let (r, g, b) = if a > 0.0 {
                    (
                        (c.r / a).clamp(0.0, 1.0),
                        (c.g / a).clamp(0.0, 1.0),
                        (c.b / a).clamp(0.0, 1.0),
                    )
                } else {
                    (0.0, 0.0, 0.0)
                };
                [
                    (linear_to_srgb(r) * 255.0).round() as u8,
                    (linear_to_srgb(g) * 255.0).round() as u8,
                    (linear_to_srgb(b) * 255.0).round() as u8,
                    (a * 255.0).round() as u8,
                ]
            })
            .collect();

        image::save_buffer(
            path,
            &pixels,
            resolution,
            resolution,
            image::ColorType::Rgba8,
        )?;
    } else {
        // Opaque mode: alpha is 1.0 everywhere, so premultiplied == straight.
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
    }
    Ok(())
}

/// Export a normal map as an RGB PNG (linear, no gamma).
pub fn export_normal_png(
    normals: &[[f32; 3]],
    resolution: u32,
    path: &Path,
    convention: NormalYConvention,
) -> Result<(), OutputError> {
    debug!("Exporting normal PNG: {}", path.display());
    let pixels = normals_to_pixels(normals, convention);
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
pub fn export_stroke_id_png(ids: &[u32], resolution: u32, path: &Path) -> Result<(), OutputError> {
    debug!("Exporting stroke ID PNG: {}", path.display());
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
            let c = hsv_to_rgb(HsvColor {
                h: hue / 360.0,
                s: sat,
                v: val,
            });
            (
                id,
                [
                    (c.r * 255.0).round() as u8,
                    (c.g * 255.0).round() as u8,
                    (c.b * 255.0).round() as u8,
                ],
            )
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

    image::save_buffer(
        path,
        &pixels,
        resolution,
        resolution,
        image::ColorType::Rgb8,
    )?;
    Ok(())
}

/// Export a stroke time map as an RGB PNG.
/// R = stroke_time_order (normalized stroke sequence), G = stroke_time_arc (arc-length progress),
/// B = 0 (reserved for layer order in future).
pub fn export_stroke_time_png(
    order: &[f32],
    arc: &[f32],
    resolution: u32,
    path: &Path,
) -> Result<(), OutputError> {
    debug!("Exporting stroke time PNG: {}", path.display());
    let pixels: Vec<u8> = order
        .iter()
        .zip(arc.iter())
        .flat_map(|(&o, &a)| {
            [
                (o.clamp(0.0, 1.0) * 255.0).round() as u8,
                (a.clamp(0.0, 1.0) * 255.0).round() as u8,
                0u8,
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

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::asset_io::linear_to_srgb;

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
        let linear_val = 0.5f32;
        let colors = vec![Color::rgb(linear_val, linear_val, linear_val); (res * res) as usize];

        let dir = std::env::temp_dir().join("pap_test_output");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("srgb_test.png");

        export_color_png(&colors, res, &path, false).unwrap();

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
        height[0] = 0.0;
        height[1] = 1.0;

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

        export_normal_png(&normals, res, &path, NormalYConvention::OpenGL).unwrap();

        let img = image::open(&path).unwrap().to_rgb8();
        let pixel = img.get_pixel(0, 0);
        assert_eq!(
            pixel[0], 128,
            "flat normal R should be 128, got {}",
            pixel[0]
        );
        assert_eq!(
            pixel[1], 128,
            "flat normal G should be 128, got {}",
            pixel[1]
        );
        assert_eq!(
            pixel[2], 255,
            "flat normal B should be 255, got {}",
            pixel[2]
        );
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
        let p0 = img.get_pixel(0, 0);
        assert_eq!(p0[0], 0, "ID 0 R should be 0");
        assert_eq!(p0[1], 0, "ID 0 G should be 0");
        assert_eq!(p0[2], 0, "ID 0 B should be 0");
        let p1 = img.get_pixel(1, 0);
        let p2 = img.get_pixel(2, 0);
        assert!(
            p1[0] > 0 || p1[1] > 0 || p1[2] > 0,
            "ID 5 should not be black"
        );
        assert!(
            p2[0] > 0 || p2[1] > 0 || p2[2] > 0,
            "ID 10 should not be black"
        );
        assert_ne!(
            [p1[0], p1[1], p1[2]],
            [p2[0], p2[1], p2[2]],
            "different IDs should have different colors"
        );
    }
}

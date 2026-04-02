//! EXR export functions for height, color, and stroke time maps.

use std::path::Path;

use log::debug;

use super::OutputError;
use crate::types::Color;

/// Export a height map as a single-channel float32 EXR (linear).
pub fn export_height_exr(height: &[f32], resolution: u32, path: &Path) -> Result<(), OutputError> {
    debug!("Exporting height EXR: {}", path.display());
    use exr::prelude::*;

    let res = resolution as usize;

    write_rgb_file(path, res, res, |x, y| {
        let h = height[y * res + x];
        (h, h, h)
    })
    .map_err(|e| OutputError::ExrError(e.to_string()))?;

    Ok(())
}

/// Export a color map as a float32 EXR (linear, NO sRGB conversion).
/// When `with_alpha` is true, outputs RGBA; otherwise RGB.
///
/// Internal color data is premultiplied alpha; this function un-premultiplies
/// before writing so the EXR stores straight (non-premultiplied) RGBA.
pub fn export_color_exr(
    color: &[Color],
    resolution: u32,
    path: &Path,
    with_alpha: bool,
) -> Result<(), OutputError> {
    debug!(
        "Exporting color EXR: {} (alpha={})",
        path.display(),
        with_alpha
    );
    use exr::prelude::*;

    let res = resolution as usize;

    if with_alpha {
        write_rgba_file(path, res, res, |x, y| {
            let c = &color[y * res + x];
            let a = c.a;
            if a > 0.0 {
                (c.r / a, c.g / a, c.b / a, a)
            } else {
                (0.0, 0.0, 0.0, 0.0)
            }
        })
        .map_err(|e| OutputError::ExrError(e.to_string()))?;
    } else {
        // Opaque mode: alpha is 1.0 everywhere, so premultiplied == straight.
        write_rgb_file(path, res, res, |x, y| {
            let c = &color[y * res + x];
            (c.r, c.g, c.b)
        })
        .map_err(|e| OutputError::ExrError(e.to_string()))?;
    }

    Ok(())
}

/// Export a stroke time map as a float32 EXR.
/// R = stroke_time_order, G = stroke_time_arc, B = 0 (reserved for layer order).
pub fn export_stroke_time_exr(
    order: &[f32],
    arc: &[f32],
    resolution: u32,
    path: &Path,
) -> Result<(), OutputError> {
    debug!("Exporting stroke time EXR: {}", path.display());
    use exr::prelude::*;

    let res = resolution as usize;

    write_rgb_file(path, res, res, |x, y| {
        let idx = y * res + x;
        (order[idx], arc[idx], 0.0f32)
    })
    .map_err(|e| OutputError::ExrError(e.to_string()))?;

    Ok(())
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::asset_io::load_texture;

    const EPS: f32 = 1e-4;

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

        let tex = load_texture(&path).unwrap();
        assert_eq!(tex.width, 16);
        assert_eq!(tex.height, 16);

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

        export_color_exr(&colors, res, &path, false).unwrap();

        let tex = load_texture(&path).unwrap();
        assert_eq!(tex.width, 4);
        assert_eq!(tex.height, 4);

        let p = tex.pixels[0];
        assert!((p[0] - 0.3).abs() < EPS, "R: expected 0.3, got {}", p[0]);
        assert!((p[1] - 0.6).abs() < EPS, "G: expected 0.6, got {}", p[1]);
        assert!((p[2] - 0.9).abs() < EPS, "B: expected 0.9, got {}", p[2]);
    }
}

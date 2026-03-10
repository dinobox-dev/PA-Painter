use eframe::egui;

use practical_arcana_painter::asset_io::LoadedTexture;
use practical_arcana_painter::types::Color;

/// Convert a LoadedTexture (linear float RGBA) to an egui TextureHandle.
/// Applies linear → sRGB conversion for correct display.
#[allow(dead_code)] // Will be used by per-layer texture picker (Commit 6).
pub fn loaded_texture_to_handle(
    ctx: &egui::Context,
    tex: &LoadedTexture,
    name: &str,
) -> egui::TextureHandle {
    let pixels: Vec<egui::Color32> = tex
        .pixels
        .iter()
        .map(|p| {
            egui::Color32::from_rgba_unmultiplied(
                linear_to_srgb_u8(p[0]),
                linear_to_srgb_u8(p[1]),
                linear_to_srgb_u8(p[2]),
                (p[3].clamp(0.0, 1.0) * 255.0) as u8,
            )
        })
        .collect();

    ctx.load_texture(
        name,
        egui::ColorImage::new([tex.width as usize, tex.height as usize], pixels),
        egui::TextureOptions::LINEAR,
    )
}

/// Convert a LoadedTexture to an egui TextureHandle without sRGB conversion.
/// Use for non-color data like normal maps.
pub fn loaded_texture_raw_handle(
    ctx: &egui::Context,
    tex: &LoadedTexture,
    name: &str,
) -> egui::TextureHandle {
    let pixels: Vec<egui::Color32> = tex
        .pixels
        .iter()
        .map(|p| {
            egui::Color32::from_rgba_unmultiplied(
                linear_to_raw_u8(p[0]),
                linear_to_raw_u8(p[1]),
                linear_to_raw_u8(p[2]),
                (p[3].clamp(0.0, 1.0) * 255.0) as u8,
            )
        })
        .collect();

    ctx.load_texture(
        name,
        egui::ColorImage::new([tex.width as usize, tex.height as usize], pixels),
        egui::TextureOptions::LINEAR,
    )
}

/// Convert a `Vec<Color>` (linear float RGBA) to an egui TextureHandle.
pub fn color_buffer_to_handle(
    ctx: &egui::Context,
    colors: &[Color],
    width: u32,
    height: u32,
    name: &str,
) -> egui::TextureHandle {
    let pixels: Vec<egui::Color32> = colors
        .iter()
        .map(|c| {
            egui::Color32::from_rgba_unmultiplied(
                linear_to_srgb_u8(c.r),
                linear_to_srgb_u8(c.g),
                linear_to_srgb_u8(c.b),
                (c.a.clamp(0.0, 1.0) * 255.0) as u8,
            )
        })
        .collect();

    ctx.load_texture(
        name,
        egui::ColorImage::new([width as usize, height as usize], pixels),
        egui::TextureOptions::LINEAR,
    )
}

/// Convert a height map (f32 values in \[0,1\]) to a grayscale texture.
pub fn height_buffer_to_handle(
    ctx: &egui::Context,
    heights: &[f32],
    resolution: u32,
    name: &str,
) -> egui::TextureHandle {
    let pixels: Vec<egui::Color32> = heights
        .iter()
        .map(|&h| {
            let v = (h.clamp(0.0, 1.0) * 255.0) as u8;
            egui::Color32::from_gray(v)
        })
        .collect();

    ctx.load_texture(
        name,
        egui::ColorImage::new([resolution as usize, resolution as usize], pixels),
        egui::TextureOptions::LINEAR,
    )
}

/// Convert a normal map (`Vec<[f32; 3]>` with components in \[0,1\]) to a texture.
/// Values are already encoded: 0.5 = zero perturbation, matching PNG export format.
pub fn normal_map_to_handle(
    ctx: &egui::Context,
    normals: &[[f32; 3]],
    resolution: u32,
    name: &str,
) -> egui::TextureHandle {
    let pixels: Vec<egui::Color32> = normals
        .iter()
        .map(|n| {
            egui::Color32::from_rgb(
                (n[0].clamp(0.0, 1.0) * 255.0) as u8,
                (n[1].clamp(0.0, 1.0) * 255.0) as u8,
                (n[2].clamp(0.0, 1.0) * 255.0) as u8,
            )
        })
        .collect();

    ctx.load_texture(
        name,
        egui::ColorImage::new([resolution as usize, resolution as usize], pixels),
        egui::TextureOptions::LINEAR,
    )
}

/// Convert a stroke ID buffer to a color-coded visualization texture.
pub fn stroke_id_to_handle(
    ctx: &egui::Context,
    ids: &[u32],
    resolution: u32,
    name: &str,
) -> egui::TextureHandle {
    let pixels: Vec<egui::Color32> = ids
        .iter()
        .map(|&id| {
            if id == 0 {
                egui::Color32::from_gray(0)
            } else {
                // Golden-ratio hue spread for visually distinct colors
                let hue = (id as f32 * 0.618_034) % 1.0;
                let (r, g, b) = hsv_to_rgb(hue, 0.65, 0.85);
                egui::Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
            }
        })
        .collect();

    ctx.load_texture(
        name,
        egui::ColorImage::new([resolution as usize, resolution as usize], pixels),
        egui::TextureOptions::LINEAR,
    )
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let h6 = h * 6.0;
    let c = v * s;
    let x = c * (1.0 - ((h6 % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h6 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (r + m, g + m, b + m)
}

/// Linear float \[0,1\] to u8 without gamma correction (for non-color data like normal maps).
pub(super) fn linear_to_raw_u8(l: f32) -> u8 {
    (l.clamp(0.0, 1.0) * 255.0) as u8
}

pub(super) fn linear_to_srgb_u8(l: f32) -> u8 {
    let s = if l <= 0.0031308 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (s.clamp(0.0, 1.0) * 255.0) as u8
}

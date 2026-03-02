use practical_arcana_painter::compositing;
use practical_arcana_painter::types::*;

fn make_layer() -> PaintLayer {
    PaintLayer {
        name: "layer_0".to_string(),
        order: 0,
        params: StrokeParams::default(),
        guides: vec![Guide {
            position: glam::Vec2::new(0.5, 0.5),
            direction: glam::Vec2::X,
            influence: 1.5,
            ..Guide::default()
        }],
    }
}

/// Render CPU output at high resolution.
/// Saves height and color maps at 1024px and 2048px.
#[test]
#[ignore]
fn visual_highres_cpu() {
    let mut layer = make_layer();
    layer.params.brush_width = 25.0;
    layer.params.color_variation = 0.15;

    let settings = OutputSettings::default();

    let solid = Color::rgb(0.55, 0.35, 0.25);

    let out_dir =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/results/highres");
    let _ = std::fs::create_dir_all(&out_dir);

    for &res in &[1024u32, 2048] {
        eprintln!("Rendering {}px...", res);

        let cpu_maps = compositing::composite_all(
            &[layer.clone()],
            res,
            &[LayerBaseColor::solid(solid)],
            &settings,
            None,
            &[],
        );
        save_maps(&cpu_maps, res, &format!("cpu_{}", res));

        eprintln!(
            "  Strokes painted (non-zero height pixels): {}",
            cpu_maps.height.iter().filter(|&&h| h > 0.0).count()
        );
    }
}

fn save_maps(maps: &compositing::GlobalMaps, resolution: u32, prefix: &str) {
    let out_dir =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/results/highres");
    let _ = std::fs::create_dir_all(&out_dir);

    // Height map (normalized grayscale)
    let max_h = maps
        .height
        .iter()
        .cloned()
        .fold(0.0f32, f32::max)
        .max(1e-10);
    let height_pixels: Vec<u8> = maps
        .height
        .iter()
        .map(|&h| ((h / max_h).clamp(0.0, 1.0) * 255.0) as u8)
        .collect();
    let height_path = out_dir.join(format!("{}_height.png", prefix));
    image::save_buffer(
        &height_path,
        &height_pixels,
        resolution,
        resolution,
        image::ColorType::L8,
    )
    .expect("save height");

    // Color map (RGB)
    let color_pixels: Vec<u8> = maps
        .color
        .iter()
        .flat_map(|c| {
            [
                (c.r.clamp(0.0, 1.0) * 255.0) as u8,
                (c.g.clamp(0.0, 1.0) * 255.0) as u8,
                (c.b.clamp(0.0, 1.0) * 255.0) as u8,
            ]
        })
        .collect();
    let color_path = out_dir.join(format!("{}_color.png", prefix));
    image::save_buffer(
        &color_path,
        &color_pixels,
        resolution,
        resolution,
        image::ColorType::Rgb8,
    )
    .expect("save color");

    eprintln!("  Wrote: {}", height_path.display());
    eprintln!("  Wrote: {}", color_path.display());
}

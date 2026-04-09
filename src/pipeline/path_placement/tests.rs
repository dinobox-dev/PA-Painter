use super::*;
use crate::types::{Guide, PaintLayer, StrokeParams};
use glam::Vec3;

fn make_layer() -> PaintLayer {
    PaintLayer {
        name: String::from("test"),
        order: 0,
        params: StrokeParams::default(),
        guides: vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 1.5,
            ..Guide::default()
        }],
    }
}

// ── Seed Distribution Tests (Poisson) ──

#[test]
fn seed_poisson_coverage() {
    let layer = make_layer();
    let seeds = generate_seeds_poisson_in(&layer.params, 512, Vec2::ZERO, Vec2::ONE, None, None);
    // Poisson disk should produce a reasonable number of seeds
    assert!(
        seeds.len() > 50 && seeds.len() < 1000,
        "unexpected seed count: {}",
        seeds.len()
    );
}

#[test]
fn seeds_poisson_within_bounds() {
    let layer = make_layer();
    let seeds = generate_seeds_poisson_in(&layer.params, 512, Vec2::ZERO, Vec2::ONE, None, None);
    assert!(!seeds.is_empty());
    for seed in &seeds {
        assert!(
            seed.x >= 0.0 && seed.x <= 1.0 && seed.y >= 0.0 && seed.y <= 1.0,
            "seed ({:.3}, {:.3}) is outside UV [0,1] bounds",
            seed.x,
            seed.y
        );
    }
}

#[test]
fn seed_poisson_determinism() {
    let layer = make_layer();
    let seeds1 = generate_seeds_poisson_in(&layer.params, 512, Vec2::ZERO, Vec2::ONE, None, None);
    let seeds2 = generate_seeds_poisson_in(&layer.params, 512, Vec2::ZERO, Vec2::ONE, None, None);
    assert_eq!(seeds1.len(), seeds2.len());
    for (a, b) in seeds1.iter().zip(seeds2.iter()) {
        assert_eq!(a.x, b.x);
        assert_eq!(a.y, b.y);
    }
}

#[test]
fn seed_poisson_minimum_spacing() {
    let layer = make_layer();
    let resolution = 512u32;
    let min_dist = layer.params.brush_width / resolution as f32 * layer.params.stroke_spacing;
    let seeds =
        generate_seeds_poisson_in(&layer.params, resolution, Vec2::ZERO, Vec2::ONE, None, None);
    // Verify all pairs respect minimum distance
    for (i, a) in seeds.iter().enumerate() {
        for b in &seeds[i + 1..] {
            let dist = (*a - *b).length();
            assert!(
                dist >= min_dist - 1e-6,
                "seeds ({:.4},{:.4}) and ({:.4},{:.4}) are {:.6} apart, min_dist = {:.6}",
                a.x,
                a.y,
                b.x,
                b.y,
                dist,
                min_dist
            );
        }
    }
}

// ── Streamline Tracing Tests ──

#[test]
fn streamline_follows_guide_direction() {
    let params = StrokeParams {
        angle_variation: 0.0,
        ..StrokeParams::default()
    };

    // Horizontal guide → path stays near y=0.5
    let h_guides = vec![Guide {
        position: Vec2::new(0.5, 0.5),
        direction: Vec2::X,
        influence: 1.5,
        ..Guide::default()
    }];
    let field = DirectionField::new(&h_guides, 512);
    let mut rng = SeededRng::new(42);
    let path = trace_streamline(
        Vec2::new(0.5, 0.5),
        &field,
        &params,
        512,
        &mut rng,
        None,
        None,
        (Vec2::ZERO, Vec2::ONE),
        None,
        0.0,
        None,
    )
    .expect("horizontal path should exist");
    for p in &path {
        assert!(
            (p.y - 0.5).abs() < 0.01,
            "horizontal: ({:.4}, {:.4}) deviates from y=0.5",
            p.x,
            p.y
        );
    }

    // Vertical guide → path stays near x=0.5
    let v_guides = vec![Guide {
        position: Vec2::new(0.5, 0.5),
        direction: Vec2::Y,
        influence: 1.5,
        ..Guide::default()
    }];
    let field = DirectionField::new(&v_guides, 512);
    let mut rng = SeededRng::new(42);
    let path = trace_streamline(
        Vec2::new(0.5, 0.5),
        &field,
        &params,
        512,
        &mut rng,
        None,
        None,
        (Vec2::ZERO, Vec2::ONE),
        None,
        0.0,
        None,
    )
    .expect("vertical path should exist");
    for p in &path {
        assert!(
            (p.x - 0.5).abs() < 0.01,
            "vertical: ({:.4}, {:.4}) deviates from x=0.5",
            p.x,
            p.y
        );
    }
}

#[test]
fn streamline_uv_boundary_termination() {
    let guides = vec![Guide {
        position: Vec2::new(0.5, 0.5),
        direction: Vec2::X,
        influence: 1.5,
        ..Guide::default()
    }];
    let params = StrokeParams {
        angle_variation: 0.0,
        max_stroke_length: 500.0,
        ..StrokeParams::default()
    };

    let field = DirectionField::new(&guides, 512);
    let mut rng = SeededRng::new(42);
    let path = trace_streamline(
        Vec2::new(0.9, 0.5),
        &field,
        &params,
        512,
        &mut rng,
        None,
        None,
        (Vec2::ZERO, Vec2::ONE),
        None,
        0.0,
        None,
    );

    if let Some(path) = path {
        let last = path.last().unwrap();
        assert!(
            last.x <= 1.01,
            "path should stop near x=1.0 UV boundary, got x={:.4}",
            last.x
        );
    }
}

#[test]
fn streamline_min_length_filter() {
    let guides = vec![Guide {
        position: Vec2::new(0.5, 0.5),
        direction: Vec2::X,
        influence: 1.5,
        ..Guide::default()
    }];
    // Very wide brush → high min_length, very short max → path too short
    let params = StrokeParams {
        brush_width: 100.0,
        max_stroke_length: 1.0,
        angle_variation: 0.0,
        ..StrokeParams::default()
    };

    let field = DirectionField::new(&guides, 512);
    let mut rng = SeededRng::new(42);
    let path = trace_streamline(
        Vec2::new(0.5, 0.5),
        &field,
        &params,
        512,
        &mut rng,
        None,
        None,
        (Vec2::ZERO, Vec2::ONE),
        None,
        0.0,
        None,
    );

    // min_length = 100/512 ≈ 0.195 UV, but max target ≈ 0.002 UV
    assert!(path.is_none(), "short path should be filtered out");
}

#[test]
fn streamline_length_variation() {
    let layer = make_layer();
    let params = StrokeParams {
        angle_variation: 0.0,
        ..StrokeParams::default()
    };

    let field = DirectionField::new(&layer.guides, 512);
    let mut lengths = Vec::new();
    let mut rng = SeededRng::new(42);
    for _ in 0..200 {
        if let Some(path) = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &params,
            512,
            &mut rng,
            None,
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
            0.0,
            None,
        ) {
            let len: f32 = path.windows(2).map(|w| (w[1] - w[0]).length()).sum();
            lengths.push(len);
        }
    }

    assert!(
        lengths.len() >= 20,
        "need enough paths to check distribution"
    );
    let min_len = lengths.iter().cloned().fold(f32::MAX, f32::min);
    let max_len = lengths.iter().cloned().fold(f32::MIN, f32::max);
    assert!(
        max_len > min_len * 1.5,
        "lengths should vary widely: min={min_len:.4}, max={max_len:.4}"
    );

    let max_length_uv = params.max_stroke_length / 512.0;
    let short_count = lengths.iter().filter(|&&l| l < max_length_uv * 0.3).count();
    let long_count = lengths.iter().filter(|&&l| l > max_length_uv * 0.7).count();
    assert!(short_count > 0, "should have some short strokes");
    assert!(long_count > 0, "should have some long strokes");
}

// ── Path Quality Filter Tests ──

#[test]
fn filter_overlapping_paths_cases() {
    let brush_width_uv = 30.0 / 512.0;

    // Non-overlapping: keep all
    let mut paths = vec![
        vec![Vec2::new(0.1, 0.1), Vec2::new(0.2, 0.1)],
        vec![Vec2::new(0.1, 0.9), Vec2::new(0.2, 0.9)],
    ];
    filter_overlapping_paths(&mut paths, brush_width_uv, 0.7, 0.3);
    assert_eq!(paths.len(), 2, "non-overlapping paths should be kept");

    // Identical: remove duplicate
    let p: Vec<Vec2> = (0..20)
        .map(|i| Vec2::new(0.1 + i as f32 * 0.01, 0.5))
        .collect();
    let mut paths = vec![p.clone(), p];
    filter_overlapping_paths(&mut paths, brush_width_uv, 0.7, 0.3);
    assert_eq!(paths.len(), 1, "duplicate path should be removed");

    // Partial overlap: keep both
    let offset = brush_width_uv * 0.5;
    let p1: Vec<Vec2> = (0..20)
        .map(|i| Vec2::new(0.1 + i as f32 * 0.01, 0.5))
        .collect();
    let p2: Vec<Vec2> = (0..20)
        .map(|i| Vec2::new(0.1 + i as f32 * 0.01, 0.5 + offset))
        .collect();
    let mut paths = vec![p1, p2];
    filter_overlapping_paths(&mut paths, brush_width_uv, 0.7, 0.3);
    assert_eq!(
        paths.len(),
        2,
        "partially overlapping paths should both be kept"
    );
}

// ── Full Pipeline Tests ──

#[test]
fn generate_paths_determinism() {
    let layer = make_layer();
    let paths1 = generate_paths(&layer, 0, &PathContext::default());
    let paths2 = generate_paths(&layer, 0, &PathContext::default());

    assert_eq!(paths1.len(), paths2.len());
    for (a, b) in paths1.iter().zip(paths2.iter()) {
        assert_eq!(a.points.len(), b.points.len());
        assert_eq!(a.stroke_id, b.stroke_id);
        assert_eq!(a.layer_index, b.layer_index);
        for (pa, pb) in a.points.iter().zip(b.points.iter()) {
            assert_eq!(pa.x, pb.x);
            assert_eq!(pa.y, pb.y);
        }
    }

    // Verify all path points stay within UV [0,1] bounds
    for path in &paths1 {
        for point in &path.points {
            assert!(
                point.x >= 0.0 && point.x <= 1.0 && point.y >= 0.0 && point.y <= 1.0,
                "path point ({:.4}, {:.4}) is outside UV [0,1] bounds",
                point.x,
                point.y
            );
        }
    }
}

#[test]
fn generate_paths_sorted_by_y() {
    let layer = make_layer();
    let paths = generate_paths(&layer, 0, &PathContext::default());

    assert!(!paths.is_empty(), "should generate some paths");
    for i in 1..paths.len() {
        assert!(
            paths[i].points[0].y >= paths[i - 1].points[0].y,
            "paths should be sorted by y: path[{}].y={:.4} < path[{}].y={:.4}",
            i,
            paths[i].points[0].y,
            i - 1,
            paths[i - 1].points[0].y
        );
    }
}

#[test]
fn generate_paths_unique_stroke_ids() {
    let layer = make_layer();
    let paths = generate_paths(&layer, 0, &PathContext::default());

    let mut ids: Vec<u32> = paths.iter().map(|p| p.stroke_id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), paths.len(), "all stroke IDs should be unique");
}

#[test]
fn area_coverage_90_percent() {
    let layer = make_layer();
    let resolution = 512u32;
    let paths = generate_paths(&layer, 0, &PathContext::default());

    let mut covered = vec![false; (resolution * resolution) as usize];
    let brush_radius_uv = layer.params.brush_width / resolution as f32 / 2.0;
    let r = (brush_radius_uv * resolution as f32).ceil() as i32;

    for path in &paths {
        for point in &path.points {
            let px = (point.x * resolution as f32) as i32;
            let py = (point.y * resolution as f32) as i32;
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx * dx + dy * dy <= r * r {
                        let x = px + dx;
                        let y = py + dy;
                        if x >= 0 && x < resolution as i32 && y >= 0 && y < resolution as i32 {
                            covered[(y * resolution as i32 + x) as usize] = true;
                        }
                    }
                }
            }
        }
    }

    // Check coverage within interior [0.1, 0.9] to avoid UV edge effects
    let margin = (0.1 * resolution as f32) as u32;
    let inner_start = margin;
    let inner_end = resolution - margin;
    let mut total = 0u32;
    let mut hit = 0u32;
    for py in inner_start..inner_end {
        for px in inner_start..inner_end {
            total += 1;
            if covered[(py * resolution + px) as usize] {
                hit += 1;
            }
        }
    }

    let coverage = hit as f32 / total as f32;
    assert!(
        coverage >= 0.90,
        "Coverage {:.1}% is below 90%",
        coverage * 100.0
    );
}

#[test]
fn generate_paths_with_dist_field_produces_strokes() {
    use crate::mesh::uv_mask::UvMask;

    // Full-coverage mask → distance field.
    // Strokes should still be generated (not rejected by the distance check).
    let mask = UvMask::full(512);
    let df = mask.distance_field();

    let layer = make_layer();
    let paths = generate_paths(
        &layer,
        0,
        &PathContext {
            dist_field: Some(&df),
            ..Default::default()
        },
    );

    assert!(
        paths.len() > 10,
        "full-coverage dist_field should produce many paths, got {}",
        paths.len()
    );
}

#[test]
fn generate_paths_with_partial_dist_field() {
    use crate::mesh::uv_mask::UvMask;

    // Mask covering only the left half (columns 0..256 of 512)
    let res = 512u32;
    let mut data = vec![false; (res * res) as usize];
    for py in 0..res {
        for px in 0..res / 2 {
            data[(py * res + px) as usize] = true;
        }
    }
    let mask = UvMask {
        data,
        resolution: res,
    };
    let df = mask.distance_field();

    let layer = make_layer();
    let paths = generate_paths(
        &layer,
        0,
        &PathContext {
            dist_field: Some(&df),
            ..Default::default()
        },
    );

    assert!(
        paths.len() > 5,
        "half-coverage dist_field should produce paths, got {}",
        paths.len()
    );

    // All path points should be in [0, 1] (clipped to UV)
    for path in &paths {
        for &pt in &path.points {
            assert!(
                pt.x >= 0.0 && pt.x <= 1.0 && pt.y >= 0.0 && pt.y <= 1.0,
                "path point ({:.3}, {:.3}) outside UV [0,1]",
                pt.x,
                pt.y
            );
        }
    }
}

#[test]
fn streamline_curved_two_guides() {
    let guides = vec![
        Guide {
            position: Vec2::new(0.2, 0.5),
            direction: Vec2::X,
            influence: 0.5,
            ..Guide::default()
        },
        Guide {
            position: Vec2::new(0.8, 0.5),
            direction: Vec2::Y,
            influence: 0.5,
            ..Guide::default()
        },
    ];
    let params = StrokeParams {
        angle_variation: 0.0,
        ..StrokeParams::default()
    };

    let field = DirectionField::new(&guides, 512);
    let mut rng = SeededRng::new(42);
    let path = trace_streamline(
        Vec2::new(0.2, 0.5),
        &field,
        &params,
        512,
        &mut rng,
        None,
        None,
        (Vec2::ZERO, Vec2::ONE),
        None,
        0.0,
        None,
    );

    let path = path.expect("curved path should exist");
    assert!(path.len() > 10, "path should have enough points");

    let first_dir = (path[1] - path[0]).normalize();
    let last_dir = (path[path.len() - 1] - path[path.len() - 2]).normalize();

    let dot = first_dir.dot(last_dir);
    assert!(
        dot < 0.99,
        "path should curve: dot between first and last direction = {dot:.4}"
    );
}

#[test]
fn turn_angle_termination() {
    let guides = vec![
        Guide {
            position: Vec2::new(0.3, 0.5),
            direction: Vec2::X,
            influence: 0.15,
            ..Guide::default()
        },
        Guide {
            position: Vec2::new(0.7, 0.5),
            direction: Vec2::Y,
            influence: 0.15,
            ..Guide::default()
        },
    ];
    let params = StrokeParams {
        angle_variation: 0.0,
        max_turn_angle: 5.0,
        ..StrokeParams::default()
    };

    let field = DirectionField::new(&guides, 512);
    let mut rng = SeededRng::new(42);
    let path = trace_streamline(
        Vec2::new(0.3, 0.5),
        &field,
        &params,
        512,
        &mut rng,
        None,
        None,
        (Vec2::ZERO, Vec2::ONE),
        None,
        0.0,
        None,
    );

    if let Some(path) = path {
        for w in path.windows(3) {
            let d1 = (w[1] - w[0]).normalize();
            let d2 = (w[2] - w[1]).normalize();
            let dot = d1.dot(d2).clamp(-1.0, 1.0);
            let angle = dot.acos().to_degrees();
            assert!(
                angle <= 10.0,
                "consecutive turn angle {angle:.1}° exceeds limit"
            );
        }
    }
}

// ── Visual Inspection Tests ──

fn draw_paths_to_png(paths: &[StrokePath], resolution: u32, filename: &str) {
    let res = resolution as usize;
    let mut img = vec![40u8; res * res * 3]; // dark gray background

    for (idx, path) in paths.iter().enumerate() {
        let hue = (idx as f32 * 0.618034) % 1.0;
        let (r, g, b) = hue_to_rgb(hue);

        for point in &path.points {
            let px = (point.x * resolution as f32) as i32;
            let py = (point.y * resolution as f32) as i32;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let x = px + dx;
                    let y = py + dy;
                    if x >= 0 && x < resolution as i32 && y >= 0 && y < resolution as i32 {
                        let i = (y as usize * res + x as usize) * 3;
                        img[i] = r;
                        img[i + 1] = g;
                        img[i + 2] = b;
                    }
                }
            }
        }
    }

    let path = crate::test_module_output_dir("path_placement").join(filename);
    image::save_buffer(&path, &img, resolution, resolution, image::ColorType::Rgb8).unwrap();
    eprintln!("Wrote visual test: {}", path.display());
}

fn hue_to_rgb(h: f32) -> (u8, u8, u8) {
    let h = h * 6.0;
    let c = 1.0_f32;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        (r * 220.0 + 35.0) as u8,
        (g * 220.0 + 35.0) as u8,
        (b * 220.0 + 35.0) as u8,
    )
}

#[test]
fn visual_horizontal_strokes() {
    let layer = make_layer();
    let resolution = 512;
    let paths = generate_paths(&layer, 0, &PathContext::default());

    eprintln!("visual_horizontal_strokes: {} paths generated", paths.len());
    draw_paths_to_png(&paths, resolution, "horizontal_strokes.png");
    assert!(!paths.is_empty());
}

#[test]
fn visual_spiral_guides() {
    let layer = PaintLayer {
        name: String::from("spiral"),
        order: 0,
        params: StrokeParams {
            brush_width: 20.0,
            stroke_spacing: 1.0,
            angle_variation: 3.0,
            max_turn_angle: 30.0,
            ..StrokeParams::default()
        },
        guides: vec![
            Guide {
                position: Vec2::new(0.5, 0.2),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.5,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.8, 0.5),
                direction: Vec2::new(0.0, 1.0),
                influence: 0.5,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.5, 0.8),
                direction: Vec2::new(-1.0, 0.0),
                influence: 0.5,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.2, 0.5),
                direction: Vec2::new(0.0, -1.0),
                influence: 0.5,
                ..Guide::default()
            },
        ],
    };
    let resolution = 512;
    let paths = generate_paths(&layer, 0, &PathContext::default());

    eprintln!("visual_spiral_guides: {} paths generated", paths.len());
    draw_paths_to_png(&paths, resolution, "spiral_guides.png");
    assert!(!paths.is_empty());
}

/// Draw paths on top of a color texture background.
fn draw_paths_on_texture(
    paths: &[StrokePath],
    tex: &[crate::types::Color],
    tex_w: u32,
    tex_h: u32,
    resolution: u32,
    filename: &str,
) {
    use crate::util::stroke_color::sample_bilinear;
    let res = resolution as usize;
    let mut img = vec![0u8; res * res * 3];

    // Draw texture background
    for py in 0..resolution {
        for px in 0..resolution {
            let uv = Vec2::new(
                (px as f32 + 0.5) / resolution as f32,
                (py as f32 + 0.5) / resolution as f32,
            );
            let c = sample_bilinear(tex, tex_w, tex_h, uv);
            let i = (py as usize * res + px as usize) * 3;
            img[i] = (c.r.clamp(0.0, 1.0) * 255.0) as u8;
            img[i + 1] = (c.g.clamp(0.0, 1.0) * 255.0) as u8;
            img[i + 2] = (c.b.clamp(0.0, 1.0) * 255.0) as u8;
        }
    }

    // Draw paths
    for (idx, path) in paths.iter().enumerate() {
        let hue = (idx as f32 * 0.618034) % 1.0;
        let (r, g, b) = hue_to_rgb(hue);
        for point in &path.points {
            let px = (point.x * resolution as f32) as i32;
            let py = (point.y * resolution as f32) as i32;
            if px >= 0 && px < resolution as i32 && py >= 0 && py < resolution as i32 {
                let i = (py as usize * res + px as usize) * 3;
                img[i] = r;
                img[i + 1] = g;
                img[i + 2] = b;
            }
        }
    }

    let path = crate::test_module_output_dir("path_placement").join(filename);
    image::save_buffer(&path, &img, resolution, resolution, image::ColorType::Rgb8).unwrap();
    eprintln!("Wrote visual test: {}", path.display());
}

#[test]
fn visual_color_boundary_comparison() {
    // INSPECT: Two images on a 4-stripe color texture (red/green/blue/yellow).
    // "color_break_off.png" — strokes cross color boundaries freely.
    // "color_break_on.png"  — strokes stop at sharp color boundaries.
    // Expected: ON image shows shorter strokes that respect color regions.

    let resolution = 512u32;

    // 128×1 texture: 4 vertical color stripes
    let mut tex_data = Vec::with_capacity(128);
    for i in 0..128 {
        tex_data.push(match i / 32 {
            0 => crate::types::Color::rgb(0.85, 0.2, 0.15), // red
            1 => crate::types::Color::rgb(0.15, 0.7, 0.2),  // green
            2 => crate::types::Color::rgb(0.15, 0.25, 0.85), // blue
            _ => crate::types::Color::rgb(0.9, 0.8, 0.15),  // yellow
        });
    }
    let tex_ref = ColorTextureRef {
        data: &tex_data,
        width: 128,
        height: 1,
    };

    let layer_off = PaintLayer {
        name: String::from("off"),
        order: 0,
        params: StrokeParams {
            brush_width: 15.0,
            angle_variation: 3.0,
            max_turn_angle: 20.0,
            color_break_threshold: None,
            ..StrokeParams::default()
        },
        guides: vec![Guide {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::X,
            influence: 1.0,
            ..Guide::default()
        }],
    };
    let mut layer_on = layer_off.clone();
    layer_on.params.color_break_threshold = Some(0.08);

    let tex_ctx = PathContext {
        color_tex: Some(&tex_ref),
        ..Default::default()
    };
    let paths_off = generate_paths(&layer_off, 0, &tex_ctx);
    let paths_on = generate_paths(&layer_on, 0, &tex_ctx);

    eprintln!(
        "color_break OFF: {} paths, ON: {} paths",
        paths_off.len(),
        paths_on.len()
    );

    draw_paths_on_texture(
        &paths_off,
        &tex_data,
        128,
        1,
        resolution,
        "color_break_off.png",
    );
    draw_paths_on_texture(
        &paths_on,
        &tex_data,
        128,
        1,
        resolution,
        "color_break_on.png",
    );

    assert!(!paths_off.is_empty());
    assert!(!paths_on.is_empty());
}

// ── Stroke Length Distribution Tests ──

#[test]
fn stroke_length_distribution_median() {
    let layer = make_layer();
    let params = StrokeParams {
        angle_variation: 0.0,
        ..StrokeParams::default()
    };

    let field = DirectionField::new(&layer.guides, 512);
    let mut lengths = Vec::new();
    let mut rng = SeededRng::new(123);
    for _ in 0..500 {
        if let Some(path) = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &params,
            512,
            &mut rng,
            None,
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
            0.0,
            None,
        ) {
            let len: f32 = path.windows(2).map(|w| (w[1] - w[0]).length()).sum();
            lengths.push(len);
        }
    }

    lengths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = lengths[lengths.len() / 2];
    let max_length_uv = params.max_stroke_length / 512.0;
    let expected_median = max_length_uv * 0.707;

    assert!(
        (median - expected_median).abs() < expected_median * 0.15,
        "median {median:.4} should be near expected {expected_median:.4} (±15%)"
    );
}

#[test]
fn stroke_length_respects_max() {
    let layer = make_layer();
    let params = StrokeParams {
        angle_variation: 0.0,
        ..StrokeParams::default()
    };

    let max_length_uv = params.max_stroke_length / 512.0;
    let field = DirectionField::new(&layer.guides, 512);
    let mut rng = SeededRng::new(77);
    for i in 0..500 {
        if let Some(path) = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &params,
            512,
            &mut rng,
            None,
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
            0.0,
            None,
        ) {
            let len: f32 = path.windows(2).map(|w| (w[1] - w[0]).length()).sum();
            assert!(
                len <= max_length_uv + 0.005,
                "path {i} length {len:.4} exceeds max {max_length_uv:.4}"
            );
        }
    }
}

#[test]
fn max_stroke_length_scaling() {
    let layer = make_layer();
    let field = DirectionField::new(&layer.guides, 512);
    let mut medians = Vec::new();
    for &max_len in &[120.0_f32, 240.0] {
        let params = StrokeParams {
            angle_variation: 0.0,
            max_stroke_length: max_len,
            ..StrokeParams::default()
        };

        let mut lengths = Vec::new();
        let mut rng = SeededRng::new(55);
        for _ in 0..300 {
            if let Some(path) = trace_streamline(
                Vec2::new(0.1, 0.5),
                &field,
                &params,
                512,
                &mut rng,
                None,
                None,
                (Vec2::ZERO, Vec2::ONE),
                None,
                0.0,
                None,
            ) {
                let len: f32 = path.windows(2).map(|w| (w[1] - w[0]).length()).sum();
                lengths.push(len);
            }
        }
        lengths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        medians.push(lengths[lengths.len() / 2]);
    }

    let ratio = medians[1] / medians[0];
    assert!(
        ratio > 1.5 && ratio < 2.5,
        "doubling max_stroke_length should roughly double median: ratio={ratio:.2} (medians: {:.4}, {:.4})",
        medians[0],
        medians[1]
    );
}

// ── Color Boundary Break Tests ──

#[test]
fn color_boundary_breaks_path() {
    // Horizontal stroke crossing a sharp red/blue boundary at u=0.5.
    // Use a 64-pixel-wide texture so the bilinear transition zone is narrow
    // enough (~1/64 UV) that per-step color diff exceeds the threshold.
    let guides = vec![Guide {
        position: Vec2::new(0.5, 0.5),
        direction: Vec2::X,
        influence: 1.5,
        ..Guide::default()
    }];
    let params = StrokeParams {
        angle_variation: 0.0,
        max_stroke_length: 500.0,
        color_break_threshold: Some(0.1),
        ..StrokeParams::default()
    };
    let field = DirectionField::new(&guides, 512);

    // 64×1 texture: left half red, right half blue
    let mut tex_data = Vec::with_capacity(64);
    for i in 0..64 {
        if i < 32 {
            tex_data.push(crate::types::Color::rgb(1.0, 0.0, 0.0));
        } else {
            tex_data.push(crate::types::Color::rgb(0.0, 0.0, 1.0));
        }
    }
    let tex_ref = ColorTextureRef {
        data: &tex_data,
        width: 64,
        height: 1,
    };

    let mut rng = SeededRng::new(42);
    let path = trace_streamline(
        Vec2::new(0.1, 0.5),
        &field,
        &params,
        512,
        &mut rng,
        Some(&tex_ref),
        None,
        (Vec2::ZERO, Vec2::ONE),
        None,
        0.0,
        None,
    );

    // Path should exist but be shorter than without boundary
    let mut rng2 = SeededRng::new(42);
    let path_no_break = trace_streamline(
        Vec2::new(0.1, 0.5),
        &field,
        &StrokeParams {
            color_break_threshold: None,
            ..params.clone()
        },
        512,
        &mut rng2,
        Some(&tex_ref),
        None,
        (Vec2::ZERO, Vec2::ONE),
        None,
        0.0,
        None,
    );

    if let (Some(p), Some(p_nb)) = (path, path_no_break) {
        assert!(
            p.len() < p_nb.len(),
            "color boundary should shorten path: {} vs {} points",
            p.len(),
            p_nb.len()
        );
        // Path should stop before or near the boundary (u ≈ 0.5)
        let last_x = p.last().unwrap().x;
        assert!(
            last_x < 0.65,
            "path should stop near color boundary, got last x={last_x:.3}"
        );
    }
}

#[test]
fn uniform_texture_no_break() {
    // Uniform texture: color boundary should never trigger.
    let guides = vec![Guide {
        position: Vec2::new(0.5, 0.5),
        direction: Vec2::X,
        influence: 1.5,
        ..Guide::default()
    }];
    let params_break = StrokeParams {
        angle_variation: 0.0,
        color_break_threshold: Some(0.05),
        ..StrokeParams::default()
    };
    let params_no_break = StrokeParams {
        color_break_threshold: None,
        ..params_break.clone()
    };
    let field = DirectionField::new(&guides, 512);

    let tex_data = vec![crate::types::Color::rgb(0.5, 0.3, 0.7); 4];
    let tex_ref = ColorTextureRef {
        data: &tex_data,
        width: 2,
        height: 2,
    };

    let mut rng1 = SeededRng::new(42);
    let mut rng2 = SeededRng::new(42);
    let path_break = trace_streamline(
        Vec2::new(0.1, 0.5),
        &field,
        &params_break,
        512,
        &mut rng1,
        Some(&tex_ref),
        None,
        (Vec2::ZERO, Vec2::ONE),
        None,
        0.0,
        None,
    );
    let path_no_break = trace_streamline(
        Vec2::new(0.1, 0.5),
        &field,
        &params_no_break,
        512,
        &mut rng2,
        Some(&tex_ref),
        None,
        (Vec2::ZERO, Vec2::ONE),
        None,
        0.0,
        None,
    );

    match (path_break, path_no_break) {
        (Some(a), Some(b)) => {
            assert_eq!(
                a.len(),
                b.len(),
                "uniform texture should not cause any breaks"
            );
        }
        (None, None) => {}
        _ => panic!("uniform texture should not affect path existence"),
    }
}

// ── Normal Boundary Break Tests ──

/// Build a simple MeshNormalData with two halves: left = +Z, right = +X.
/// Simulates a 90° hard edge at u=0.5.
fn make_two_face_normal_data(resolution: u32) -> MeshNormalData {
    let size = (resolution * resolution) as usize;
    let mut normals = Vec::with_capacity(size);
    let tangents = vec![Vec3::X; size];
    let bitangents = vec![Vec3::Y; size];
    for py in 0..resolution {
        for px in 0..resolution {
            let u = (px as f32 + 0.5) / resolution as f32;
            if u < 0.5 {
                normals.push(Vec3::Z); // left face
            } else {
                normals.push(Vec3::X); // right face (90° from left)
            }
            let _ = py; // suppress unused warning
        }
    }
    MeshNormalData {
        object_normals: normals,
        tangents,
        bitangents,
        resolution,
    }
}

#[test]
fn normal_boundary_no_longer_breaks_path() {
    // Normal break now handled by per-pixel clamp in compositing,
    // so paths should NOT be shortened at face boundaries.
    let guides = vec![Guide {
        position: Vec2::new(0.5, 0.5),
        direction: Vec2::X,
        influence: 1.5,
        ..Guide::default()
    }];
    let params = StrokeParams {
        angle_variation: 0.0,
        max_stroke_length: 500.0,
        normal_break_threshold: Some(0.5),
        ..StrokeParams::default()
    };
    let field = DirectionField::new(&guides, 512);
    let nd = make_two_face_normal_data(64);

    let mut rng = SeededRng::new(42);
    let path = trace_streamline(
        Vec2::new(0.1, 0.5),
        &field,
        &params,
        512,
        &mut rng,
        None,
        Some(&nd),
        (Vec2::ZERO, Vec2::ONE),
        None,
        0.0,
        None,
    );

    // Path without normal break for comparison
    let mut rng2 = SeededRng::new(42);
    let path_no_break = trace_streamline(
        Vec2::new(0.1, 0.5),
        &field,
        &StrokeParams {
            normal_break_threshold: None,
            ..params.clone()
        },
        512,
        &mut rng2,
        None,
        Some(&nd),
        (Vec2::ZERO, Vec2::ONE),
        None,
        0.0,
        None,
    );

    if let (Some(p), Some(p_nb)) = (path, path_no_break) {
        assert_eq!(
            p.len(),
            p_nb.len(),
            "path should NOT be shortened by normal_break_threshold (now per-pixel)"
        );
        let last_x = p.last().unwrap().x;
        assert!(
            last_x < 0.6,
            "path should stop near normal boundary at u=0.5, got last x={last_x:.3}"
        );
    }
}

#[test]
fn threshold_none_ignores_features() {
    // With threshold=None, neither color texture nor normal data should affect paths.
    let guides = vec![Guide {
        position: Vec2::new(0.5, 0.5),
        direction: Vec2::X,
        influence: 1.5,
        ..Guide::default()
    }];
    let field = DirectionField::new(&guides, 512);

    // Color texture case
    {
        let params = StrokeParams {
            angle_variation: 0.0,
            color_break_threshold: None,
            ..StrokeParams::default()
        };
        let tex_data = vec![
            crate::types::Color::rgb(1.0, 0.0, 0.0),
            crate::types::Color::rgb(0.0, 0.0, 1.0),
            crate::types::Color::rgb(1.0, 0.0, 0.0),
            crate::types::Color::rgb(0.0, 0.0, 1.0),
        ];
        let tex_ref = ColorTextureRef {
            data: &tex_data,
            width: 2,
            height: 2,
        };

        let mut rng1 = SeededRng::new(42);
        let mut rng2 = SeededRng::new(42);
        let path_no_tex = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &params,
            512,
            &mut rng1,
            None,
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
            0.0,
            None,
        );
        let path_with_tex = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &params,
            512,
            &mut rng2,
            Some(&tex_ref),
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
            0.0,
            None,
        );

        match (path_no_tex, path_with_tex) {
            (Some(a), Some(b)) => {
                assert_eq!(
                    a.len(),
                    b.len(),
                    "color: threshold=None should not affect path length"
                );
            }
            (None, None) => {}
            _ => panic!("color: threshold=None should not affect path existence"),
        }
    }

    // Normal data case
    {
        let params = StrokeParams {
            angle_variation: 0.0,
            normal_break_threshold: None,
            ..StrokeParams::default()
        };
        let nd = make_two_face_normal_data(64);

        let mut rng1 = SeededRng::new(42);
        let mut rng2 = SeededRng::new(42);
        let path_no_nd = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &params,
            512,
            &mut rng1,
            None,
            None,
            (Vec2::ZERO, Vec2::ONE),
            None,
            0.0,
            None,
        );
        let path_with_nd = trace_streamline(
            Vec2::new(0.1, 0.5),
            &field,
            &params,
            512,
            &mut rng2,
            None,
            Some(&nd),
            (Vec2::ZERO, Vec2::ONE),
            None,
            0.0,
            None,
        );

        match (path_no_nd, path_with_nd) {
            (Some(a), Some(b)) => {
                assert_eq!(
                    a.len(),
                    b.len(),
                    "normal: threshold=None should not affect path length"
                );
            }
            (None, None) => {}
            _ => panic!("normal: threshold=None should not affect path existence"),
        }
    }
}

// ── Normal Boundary Visual Tests ──

/// Build a 4-face normal data: quadrants have different normals.
/// Top-left=+Z, top-right=+X, bottom-left=+Y, bottom-right=(-1,1,1)/√3
fn make_four_face_normal_data(resolution: u32) -> MeshNormalData {
    let size = (resolution * resolution) as usize;
    let mut normals = Vec::with_capacity(size);
    let tangents = vec![Vec3::X; size];
    let bitangents = vec![Vec3::Y; size];
    let diag = Vec3::new(-1.0, 1.0, 1.0).normalize();
    for py in 0..resolution {
        for px in 0..resolution {
            let u = (px as f32 + 0.5) / resolution as f32;
            let v = (py as f32 + 0.5) / resolution as f32;
            let n = match (u < 0.5, v < 0.5) {
                (true, true) => Vec3::Z,
                (false, true) => Vec3::X,
                (true, false) => Vec3::Y,
                (false, false) => diag,
            };
            normals.push(n);
        }
    }
    MeshNormalData {
        object_normals: normals,
        tangents,
        bitangents,
        resolution,
    }
}

/// Draw paths on a normal-map background (normals → RGB).
fn draw_paths_on_normals(
    paths: &[StrokePath],
    nd: &MeshNormalData,
    resolution: u32,
    filename: &str,
) {
    let res = resolution as usize;
    let mut img = vec![0u8; res * res * 3];

    // Draw normal map background (object normal → RGB, same encoding as normal maps)
    for py in 0..resolution {
        for px in 0..resolution {
            let uv = Vec2::new(
                (px as f32 + 0.5) / resolution as f32,
                (py as f32 + 0.5) / resolution as f32,
            );
            let n = sample_object_normal(nd, uv);
            let i = (py as usize * res + px as usize) * 3;
            // Map [-1,1] → [0,255], dimmed to 40% so paths stand out
            img[i] = ((n.x * 0.5 + 0.5) * 0.4 * 255.0) as u8;
            img[i + 1] = ((n.y * 0.5 + 0.5) * 0.4 * 255.0) as u8;
            img[i + 2] = ((n.z * 0.5 + 0.5) * 0.4 * 255.0) as u8;
        }
    }

    // Draw paths
    for (idx, path) in paths.iter().enumerate() {
        let hue = (idx as f32 * 0.618034) % 1.0;
        let (r, g, b) = hue_to_rgb(hue);
        for point in &path.points {
            let px = (point.x * resolution as f32) as i32;
            let py = (point.y * resolution as f32) as i32;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let x = px + dx;
                    let y = py + dy;
                    if x >= 0 && x < resolution as i32 && y >= 0 && y < resolution as i32 {
                        let i = (y as usize * res + x as usize) * 3;
                        img[i] = r;
                        img[i + 1] = g;
                        img[i + 2] = b;
                    }
                }
            }
        }
    }

    let path = crate::test_module_output_dir("path_placement").join(filename);
    image::save_buffer(&path, &img, resolution, resolution, image::ColorType::Rgb8).unwrap();
    eprintln!("Wrote visual test: {}", path.display());
}

#[test]
fn visual_normal_boundary_comparison() {
    // INSPECT: Two images on a 4-face normal map.
    // "normal_break_off.png" — strokes cross normal boundaries freely.
    // "normal_break_on.png"  — strokes stop at hard normal edges.
    // Expected: ON image shows shorter strokes confined to each face.

    let resolution = 512u32;
    let nd = make_four_face_normal_data(128);

    let layer_off = PaintLayer {
        name: String::from("off"),
        order: 0,
        params: StrokeParams {
            brush_width: 15.0,
            angle_variation: 3.0,
            max_turn_angle: 20.0,
            normal_break_threshold: None,
            ..StrokeParams::default()
        },
        guides: vec![
            Guide {
                position: Vec2::new(0.25, 0.25),
                direction: Vec2::new(1.0, 0.3).normalize(),
                influence: 0.5,
                ..Guide::default()
            },
            Guide {
                position: Vec2::new(0.75, 0.75),
                direction: Vec2::new(-0.3, 1.0).normalize(),
                influence: 0.5,
                ..Guide::default()
            },
        ],
    };
    let mut layer_on = layer_off.clone();
    layer_on.params.normal_break_threshold = Some(0.5);

    let nd_ctx = PathContext {
        normal_data: Some(&nd),
        ..Default::default()
    };
    let paths_off = generate_paths(&layer_off, 0, &nd_ctx);
    let paths_on = generate_paths(&layer_on, 0, &nd_ctx);

    eprintln!(
        "normal_break OFF: {} paths, ON: {} paths",
        paths_off.len(),
        paths_on.len()
    );

    draw_paths_on_normals(&paths_off, &nd, resolution, "normal_break_off.png");
    draw_paths_on_normals(&paths_on, &nd, resolution, "normal_break_on.png");

    assert!(!paths_off.is_empty());
    assert!(!paths_on.is_empty());
}

// ── Density Comparison Visual Tests ──

/// Draw a labelled header onto the top of an image buffer.
fn draw_label(img: &mut [u8], res: usize, label: &str) {
    // Simple 3×5 digit/letter font — just draw a colored bar with contrasting text area
    let bar_h = 18;
    for y in 0..bar_h.min(res) {
        for x in 0..res {
            let i = (y * res + x) * 3;
            img[i] = 20;
            img[i + 1] = 20;
            img[i + 2] = 20;
        }
    }
    // Encode label as simple pixel blocks (4px per char)
    let char_w = 6;
    let char_h = 10;
    let start_x = 4;
    let start_y = 4;
    for (ci, ch) in label.chars().enumerate() {
        if ch == ' ' {
            continue;
        }
        let x0 = start_x + ci * char_w;
        for dy in 0..char_h.min(bar_h - start_y) {
            for dx in 0..(char_w - 1) {
                let x = x0 + dx;
                let y = start_y + dy;
                if x < res && y < res {
                    let i = (y * res + x) * 3;
                    img[i] = 200;
                    img[i + 1] = 200;
                    img[i + 2] = 200;
                }
            }
        }
    }
}

#[test]
fn visual_density_comparison() {
    // INSPECT: Three images comparing density improvement methods.
    // All use the same guide setup (horizontal + slight diagonal).
    // Baseline uses Poisson disk + overscan (the unified pipeline).
    //
    // 1. density_baseline.png        — default params (Poisson + overscan)
    // 2. density_A_tight_spacing.png  — stroke_spacing=0.5
    // 3. density_AB_relaxed.png       — spacing=0.5 + overlap_ratio=0.85, dist_factor=0.15

    let resolution = 512u32;
    let guides = vec![
        Guide {
            position: Vec2::new(0.3, 0.4),
            direction: Vec2::new(1.0, 0.15).normalize(),
            influence: 0.8,
            ..Guide::default()
        },
        Guide {
            position: Vec2::new(0.7, 0.6),
            direction: Vec2::new(1.0, -0.1).normalize(),
            influence: 0.8,
            ..Guide::default()
        },
    ];

    // ── 1. Baseline (Poisson + overscan) ──
    let layer_base = PaintLayer {
        name: String::from("baseline"),
        order: 0,
        params: StrokeParams {
            brush_width: 20.0,
            angle_variation: 3.0,
            max_turn_angle: 20.0,
            ..StrokeParams::default()
        },
        guides: guides.clone(),
    };
    let paths_base = generate_paths(&layer_base, 0, &PathContext::default());

    // ── 2. Method A: tight spacing ──
    let layer_a = PaintLayer {
        name: String::from("tight_spacing"),
        order: 0,
        params: StrokeParams {
            stroke_spacing: 0.5,
            ..layer_base.params.clone()
        },
        guides: guides.clone(),
    };
    let paths_a = generate_paths(&layer_a, 0, &PathContext::default());

    // ── 3. Method A+B: tight spacing + relaxed overlap filter ──
    let layer_ab = PaintLayer {
        name: String::from("relaxed_filter"),
        order: 0,
        params: StrokeParams {
            stroke_spacing: 0.5,
            overlap_ratio: Some(0.85),
            overlap_dist_factor: Some(0.15),
            ..layer_base.params.clone()
        },
        guides: guides.clone(),
    };
    let paths_ab = generate_paths(&layer_ab, 0, &PathContext::default());

    eprintln!("=== Density Comparison ===");
    eprintln!("  Baseline:      {} paths", paths_base.len());
    eprintln!("  A (spacing):   {} paths", paths_a.len());
    eprintln!("  A+B (relaxed): {} paths", paths_ab.len());

    let cases: &[(&[StrokePath], &str, &str)] = &[
        (
            &paths_base,
            "density_1_baseline.png",
            "BASELINE (Poisson+overscan)",
        ),
        (&paths_a, "density_2_A_tight_spacing.png", "A: spacing=0.5"),
        (&paths_ab, "density_3_AB_relaxed.png", "A+B: spacing+filter"),
    ];

    for &(paths, filename, label) in cases {
        let res = resolution as usize;
        let mut img = vec![40u8; res * res * 3];

        for (idx, path) in paths.iter().enumerate() {
            let hue = (idx as f32 * 0.618034) % 1.0;
            let (r, g, b) = hue_to_rgb(hue);
            for point in &path.points {
                let px = (point.x * resolution as f32) as i32;
                let py = (point.y * resolution as f32) as i32;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let x = px + dx;
                        let y = py + dy;
                        if x >= 0 && x < resolution as i32 && y >= 0 && y < resolution as i32 {
                            let i = (y as usize * res + x as usize) * 3;
                            img[i] = r;
                            img[i + 1] = g;
                            img[i + 2] = b;
                        }
                    }
                }
            }
        }

        draw_label(&mut img, res, &format!("{} ({} paths)", label, paths.len()));

        let path = crate::test_module_output_dir("path_placement").join(filename);
        image::save_buffer(&path, &img, resolution, resolution, image::ColorType::Rgb8).unwrap();
        eprintln!("Wrote: {}", path.display());
    }

    // All methods should produce paths
    assert!(!paths_base.is_empty());
    assert!(!paths_a.is_empty());
    assert!(!paths_ab.is_empty());

    // Density should increase: A > baseline, A+B > A
    assert!(
        paths_a.len() > paths_base.len(),
        "A ({}) should have more paths than baseline ({})",
        paths_a.len(),
        paths_base.len()
    );
    assert!(
        paths_ab.len() > paths_a.len(),
        "A+B ({}) should have more paths than A ({})",
        paths_ab.len(),
        paths_a.len()
    );
}

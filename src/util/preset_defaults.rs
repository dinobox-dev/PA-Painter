//! Built-in default paint presets.

use crate::types::{
    CurveKnot, PaintPreset, PaintValues, PresetLibrary, PressureCurve, PressurePreset, StrokeParams,
};

use super::pressure::preset_to_custom;

impl PresetLibrary {
    /// Built-in default presets.
    pub fn built_in() -> PresetLibrary {
        let default_layout = StrokeParams::default();

        PresetLibrary {
            presets: vec![
                PaintPreset {
                    name: "flat_wide".to_string(),
                    values: PaintValues {
                        brush_width: 40.0,
                        load: 1.4,
                        body_wiggle: 0.15,
                        pressure_curve: preset_to_custom(PressurePreset::FadeOut),
                        stroke_spacing: default_layout.stroke_spacing,
                        max_stroke_length: default_layout.max_stroke_length,
                        angle_variation: default_layout.angle_variation,
                        max_turn_angle: default_layout.max_turn_angle,
                        color_break_threshold: None,
                        normal_break_threshold: None,
                        overlap_ratio: None,
                        overlap_dist_factor: None,
                        color_variation: default_layout.color_variation,
                        viscosity: 0.0,
                    },
                },
                PaintPreset {
                    name: "round_thin".to_string(),
                    values: PaintValues {
                        brush_width: 15.0,
                        load: 1.2,
                        body_wiggle: 0.1,
                        pressure_curve: preset_to_custom(PressurePreset::Taper),
                        stroke_spacing: 1.0,
                        max_stroke_length: 240.0,
                        angle_variation: 5.0,
                        max_turn_angle: 15.0,
                        color_break_threshold: None,
                        normal_break_threshold: None,
                        overlap_ratio: None,
                        overlap_dist_factor: None,
                        color_variation: 0.1,
                        viscosity: 0.0,
                    },
                },
                PaintPreset {
                    name: "dry_brush".to_string(),
                    values: PaintValues {
                        brush_width: 50.0,
                        load: 0.5,
                        body_wiggle: 0.2,
                        pressure_curve: preset_to_custom(PressurePreset::FadeOut),
                        stroke_spacing: 1.2,
                        max_stroke_length: 300.0,
                        angle_variation: 15.0,
                        max_turn_angle: 30.0,
                        color_break_threshold: None,
                        normal_break_threshold: None,
                        overlap_ratio: None,
                        overlap_dist_factor: None,
                        color_variation: 0.15,
                        viscosity: 0.0,
                    },
                },
                PaintPreset {
                    name: "impasto".to_string(),
                    values: PaintValues {
                        brush_width: 30.0,
                        load: 1.8,
                        body_wiggle: 0.1,
                        pressure_curve: preset_to_custom(PressurePreset::Bell),
                        stroke_spacing: 0.6,
                        max_stroke_length: 180.0,
                        angle_variation: 3.0,
                        max_turn_angle: 15.0,
                        color_break_threshold: None,
                        normal_break_threshold: None,
                        overlap_ratio: Some(0.8),
                        overlap_dist_factor: Some(0.2),
                        color_variation: 0.08,
                        viscosity: 0.4,
                    },
                },
                PaintPreset {
                    name: "glaze".to_string(),
                    values: PaintValues {
                        brush_width: 35.0,
                        load: 0.7,
                        body_wiggle: 0.1,
                        pressure_curve: preset_to_custom(PressurePreset::Uniform),
                        stroke_spacing: 1.0,
                        max_stroke_length: 240.0,
                        angle_variation: 0.0,
                        max_turn_angle: 15.0,
                        color_break_threshold: None,
                        normal_break_threshold: None,
                        overlap_ratio: None,
                        overlap_dist_factor: None,
                        color_variation: 0.1,
                        viscosity: 0.0,
                    },
                },
                PaintPreset {
                    name: "heavy_load".to_string(),
                    values: PaintValues {
                        brush_width: 42.0,
                        load: 1.7,
                        body_wiggle: 0.15,
                        pressure_curve: PressureCurve::Custom(vec![
                            CurveKnot {
                                pos: [0.0, 0.0],
                                handle_in: [0.0, 0.0],
                                handle_out: [0.006319868, 0.9460547],
                            },
                            CurveKnot {
                                pos: [0.25, 0.9375],
                                handle_in: [0.16666666, 0.9791667],
                                handle_out: [0.33333334, 0.8958333],
                            },
                            CurveKnot {
                                pos: [0.46651104, 0.83425784],
                                handle_in: [0.33325395, 0.9663867],
                                handle_out: [0.5498443, 0.7509245],
                            },
                            CurveKnot {
                                pos: [0.75, 0.4375],
                                handle_in: [0.6666667, 0.5625],
                                handle_out: [0.8333333, 0.3125],
                            },
                            CurveKnot {
                                pos: [1.0, 0.0],
                                handle_in: [0.9166667, 0.14583334],
                                handle_out: [1.0, 0.0],
                            },
                        ]),
                        stroke_spacing: 1.0,
                        max_stroke_length: 240.0,
                        angle_variation: 5.0,
                        max_turn_angle: 15.0,
                        color_break_threshold: None,
                        normal_break_threshold: None,
                        overlap_ratio: None,
                        overlap_dist_factor: None,
                        color_variation: 0.1,
                        viscosity: 0.0,
                    },
                },
                PaintPreset {
                    name: "crosshatch".to_string(),
                    values: PaintValues {
                        brush_width: 30.0,
                        load: 1.2,
                        body_wiggle: 0.15,
                        pressure_curve: preset_to_custom(PressurePreset::FadeOut),
                        stroke_spacing: 0.8,
                        max_stroke_length: 120.0,
                        angle_variation: 5.0,
                        max_turn_angle: 20.0,
                        color_break_threshold: None,
                        normal_break_threshold: None,
                        overlap_ratio: None,
                        overlap_dist_factor: None,
                        color_variation: 0.1,
                        viscosity: 0.0,
                    },
                },
                PaintPreset {
                    name: "loose_organic".to_string(),
                    values: PaintValues {
                        brush_width: 30.0,
                        load: 1.3,
                        body_wiggle: 0.15,
                        pressure_curve: preset_to_custom(PressurePreset::FadeOut),
                        stroke_spacing: 1.2,
                        max_stroke_length: 300.0,
                        angle_variation: 15.0,
                        max_turn_angle: 30.0,
                        color_break_threshold: None,
                        normal_break_threshold: None,
                        overlap_ratio: None,
                        overlap_dist_factor: None,
                        color_variation: 0.15,
                        viscosity: 0.0,
                    },
                },
            ],
        }
    }
}

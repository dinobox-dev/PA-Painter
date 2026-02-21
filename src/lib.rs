#[cfg(test)]
pub(crate) fn test_module_output_dir(module: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/results")
        .join(module);
    std::fs::create_dir_all(&dir).ok();
    dir
}

#[cfg(test)]
pub(crate) fn test_fixtures_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub mod error;
#[cfg(test)]
pub mod test_util;

pub mod asset_io; // Phase 00
pub mod math; // Phase 01
pub mod pressure; // Phase 01
pub mod rng; // Phase 01
pub mod types; // Phase 01
pub mod brush_profile; // Phase 02
pub mod stroke_height; // Phase 02
pub mod direction_field; // Phase 03
pub mod path_placement; // Phase 05
pub mod local_frame; // Phase 06
pub mod stroke_color; // Phase 07
pub mod compositing; // Phase 08
pub mod output; // Phase 09
pub mod project; // Phase 10

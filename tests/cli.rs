use std::process::Command;

fn pa_painter_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pa-painter"))
}

// ── Argument parsing ─────────────────────────────────────────────

#[test]
fn help_flag_exits_zero() {
    let output = pa_painter_bin().arg("--help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage:"));
}

#[test]
fn no_args_exits_with_error() {
    let output = pa_painter_bin().output().unwrap();
    assert!(!output.status.success());
}

#[test]
fn invalid_resolution_rejected() {
    let output = pa_painter_bin()
        .args(["dummy.papr", "-r", "99999"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("16384"));
}

#[test]
fn invalid_format_rejected() {
    let output = pa_painter_bin()
        .args(["dummy.papr", "-f", "bmp"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("png"));
}

// ── E2E: full render pipeline ────────────────────────────────────

#[test]
fn e2e_render_example_project() {
    let project = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/PAPainterLogo.papr");
    let out_dir = tempfile::tempdir().unwrap();

    let output = pa_painter_bin()
        .args([project, "-o", out_dir.path().to_str().unwrap(), "-r", "256"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify output files exist
    assert!(out_dir.path().join("color_map.png").exists());
    assert!(out_dir.path().join("height_map.png").exists());
    assert!(out_dir.path().join("normal_map.png").exists());
}

# Practical Arcana Painter

![CI](https://github.com/dinobox-dev/Practical-Arcana-Painter/actions/workflows/ci.yml/badge.svg)

Procedural paint stroke generator for 3D assets. Turn any mesh into a hand-painted look with full control over stroke direction, density, and layering.

Built for *Practical Arcana* (coming soon), but works with any UV-mapped 3D mesh. Provides both a **GUI editor** for interactive work and a **CLI renderer** for headless batch processing. The core rendering engine is available as a Rust library crate.

<!-- ![Screenshot](docs/screenshot.png) -->

## Features

- **5-stage CPU pipeline**: direction field → path placement → stroke height → compositing → output
- **Layer system**: multiple paint layers with independent brush settings, blend ordering, and visibility
- **Guide tools**: directional, source, sink, and vortex guides to control stroke flow
- **Pressure curves**: preset and custom Bézier spline editors for stroke pressure variation
- **Output maps**: color, normal, height, and ambient occlusion — export as PNG or EXR
- **3D preview**: real-time mesh preview with generated textures applied
- **GUI editor**: full-featured editor built with egui/eframe
- **CLI renderer**: headless batch rendering for automation
- **File format**: `.pap` project files with undo/redo support

## Building

Requires Rust 1.87+ and a GPU with Vulkan/Metal/DX12 support (for the GUI).

```bash
# Build both CLI and GUI
cargo build --release

# Run the GUI editor
cargo run --release --bin practical-arcana-painter-gui

# Run the CLI renderer
cargo run --release --bin practical-arcana-painter -- <input.pap> -o output/
```

### CLI Options

```
Usage: practical-arcana-painter <project.pap> [options]

Options:
  -o, --output <dir>       Output directory (default: ./output)
  -r, --resolution <px>    Override output resolution (1–16384)
  -f, --format <fmt>       Export format: png (default) or exr
  -h, --help               Show this help
```

## Supported Formats

- **Mesh import**: OBJ, GLB/GLTF
- **Texture export**: PNG, OpenEXR
- **3D export**: GLB (with generated textures baked in)
- **Project files**: `.pap` (ZIP-based, contains JSON + asset references)

## Architecture

The rendering engine is a 5-stage pipeline, each implemented as a standalone module:

| Stage | Module | Description |
|-------|--------|-------------|
| 1 | `direction_field` | Compute per-texel stroke flow from user-placed guides |
| 2 | `path_placement` | Poisson-disk seeding + streamline tracing along the flow |
| 3 | `stroke_height` | Pressure curves and brush profiles → height values |
| 4 | `compositing` | Blend all visible layers into unified global maps |
| 5 | `output` | Generate final color, normal, height, and AO textures |

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for more details on the design.

## Output Maps

Each render produces up to four texture maps:

- **Color map** — base color with per-stroke HSV variation
- **Normal map** — tangent-space or depicted-form normals from stroke height gradients
- **Height map** — grayscale displacement from accumulated stroke heights
- **AO map** — ambient occlusion derived from height field curvature

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/my-change`)
3. Run `cargo fmt` and `cargo clippy --all-targets -- -D warnings` before committing
4. Open a pull request

## License

[MIT](LICENSE) — Copyright (c) 2026 DiNo Box Inc.

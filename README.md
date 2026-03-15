# PA Painter

![CI](https://github.com/dinobox-dev/PA-Painter/actions/workflows/ci.yml/badge.svg)
[![itch.io](https://img.shields.io/badge/itch.io-PA%20Painter-FA5C5C?logo=itchdotio&logoColor=white)](https://dinoboxgamedev.itch.io/papainter)

Procedural paint stroke generator for 3D assets. Turn any mesh into a hand-painted look with full control over stroke direction, density, and layering.

Originally built for *Practical Arcana* (coming soon), but works with any UV-mapped 3D mesh. Provides both a **GUI editor** for interactive work and a **CLI renderer** for headless batch processing. The core rendering engine is available as a Rust library crate.

![PA Painter GUI — 3D Preview](https://raw.githubusercontent.com/dinobox-dev/PA-Painter/main/docs/images/gui-preview.png)

## Features

- **5-stage CPU pipeline**: direction field → path placement → stroke height → compositing → output
- **Layer system**: multiple paint layers with independent brush settings, blend ordering, and visibility
- **Guide tools**: directional, source, sink, and vortex guides to control stroke flow
- **Pressure curves**: preset and custom Bézier spline editors for stroke pressure variation
- **Output maps**: color, normal, height, stroke ID, and stroke time — export as PNG or EXR
- **3D preview**: real-time mesh preview with generated textures applied
- **GUI editor**: full-featured editor built with egui/eframe
- **CLI renderer**: headless batch rendering for automation
- **File format**: `.papr` project files with undo/redo support

## Download

Pre-built binaries for Windows, Linux, and macOS are available on [itch.io](https://dinoboxgamedev.itch.io/papainter). macOS builds are code-signed and notarized by Apple.

## Building from Source

Requires Rust 1.87+ and a GPU with Vulkan/Metal/DX12 support (for the GUI).

```bash
# Build both CLI and GUI
cargo build --release

# Run the GUI editor
cargo run --release --bin pa-painter-gui

# Run the CLI renderer
cargo run --release --bin pa-painter -- <input.papr> -o output/
```

### CLI Options

```
Usage: pa-painter <project.papr> [options]

Options:
  -o, --output <dir>       Output directory (default: ./output)
  -r, --resolution <px>    Override output resolution (1–16384)
  -f, --format <fmt>       Export format: png (default) or exr
      --per-layer          Export each layer as separate textures
  -h, --help               Show this help
```

## Supported Formats

- **Mesh import**: OBJ, GLB/GLTF
- **Texture export**: PNG, OpenEXR
- **3D export**: GLB (with generated textures baked in)
- **Project files**: `.papr` (ZIP-based, contains JSON + asset references)

## Architecture

The rendering engine is a 5-stage pipeline, each implemented as a standalone module:

| Stage | Module | Description |
|-------|--------|-------------|
| 1 | `direction_field` | Compute per-texel stroke flow from user-placed guides |
| 2 | `path_placement` | Poisson-disk seeding + streamline tracing along the flow |
| 3 | `stroke_height` | Pressure curves and brush profiles → height values |
| 4 | `compositing` | Blend all visible layers into unified global maps |
| 5 | `output` | Generate final color, normal, height, stroke ID, and stroke time textures |

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for more details on the design.

## Output Maps

Each render produces the following texture maps:

- **Color map** — base color with per-stroke HSV variation
- **Normal map** — tangent-space or depicted-form normals from stroke height gradients
- **Height map** — grayscale displacement from accumulated stroke heights
- **Stroke ID map** — per-pixel layer index for masking and post-processing
- **Stroke time map** — R: normalized stroke order, G: arc-length position within each stroke (for animated reveal effects; see [`docs/Stroke Time Map Shader Reference.md`](docs/Stroke%20Time%20Map%20Shader%20Reference.md))

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/my-change`)
3. Run `cargo fmt` and `cargo clippy --all-targets -- -D warnings` before committing
4. Open a pull request

## License

[MIT](LICENSE) — Copyright (c) 2026 DiNo Box Inc.

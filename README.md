# Practical Arcana Painter

Procedural paint stroke generator for 3D assets. Turn any mesh into a hand-painted look with full control over stroke direction, density, and layering.

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

Requires Rust 1.70+ and a GPU with Vulkan/Metal/DX12 support (for the GUI).

```bash
# Build both CLI and GUI
cargo build --release

# Run the GUI editor
cargo run --release --bin practical-arcana-painter-gui

# Run the CLI renderer
cargo run --release --bin practical-arcana-painter -- <input.pap> -o output/
```

## Supported Mesh Formats

- **Import**: OBJ, GLB/GLTF
- **Export**: GLB (with generated textures baked in)

## License

[MIT](LICENSE) — Copyright (c) 2026 DiNo Box Inc.

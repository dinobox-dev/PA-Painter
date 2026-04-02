# Architecture

This document describes the high-level architecture of PA Painter.
See also the rustdoc on `lib.rs` for API-level documentation.

## Bird's Eye View

The project turns a UV-mapped 3D mesh, a base colour, and user-placed direction
guides into hand-painted-looking texture maps (colour, height, normal, stroke-ID,
stroke-time) through a deterministic, 5-stage CPU pipeline.

```
  Guides + Mesh + Base Colour
              │
     1. Direction Field
              │
     2. Path Placement
              │
     3. Stroke Height
              │
     4. Compositing
              │
     5. Output
              │
   Colour / Height / Normal / Stroke-ID / Stroke-Time
```

Everything runs in linear-float colour space; sRGB conversion happens only at I/O
boundaries. Identical parameters + seed always produce identical output (ChaCha8).

## Crate Layout

The project ships as **one library crate** with **two binaries** (CLI and GUI).

### Library (`src/`)

| Directory | Role |
|-----------|------|
| `pipeline/` | The 5-stage stroke generation pipeline (stateless, pure functions) |
| `mesh/` | Mesh processing, UV operations, and asset loading (OBJ / glTF / textures) |
| `io/` | `.papr` project file I/O and GLB export |
| `types/` | Core data structures (`Color`, `PaintValues`, `Layer`, etc.) |
| `util/` | Math, RNG, pressure curves, brush profiles, colour helpers |

For the full module listing, see `cargo doc` or the rustdoc header in `src/lib.rs`.

### GUI (`src/gui/`)

egui/eframe application in `src/main_gui.rs`. For the full list of sub-modules
and their responsibilities, see `src/gui/mod.rs`.

### CLI (`src/main.rs`)

Loads a `.papr` project, runs the pipeline, and exports maps. No GUI dependency.

## Key Design Decisions

- **Density-based compositing**: strokes do not accumulate; each pixel keeps only
  its maximum density. This avoids the "over-painted mud" problem.
- **PaintValues as the preset unit**: brush physics and layout strategy live in
  one struct, making presets copy-by-value with no reference bookkeeping.
- **Layer order**: layers composite in ascending `order`; within a layer, strokes
  composite in path order (top-to-bottom from Poisson-disk placement).
  The GUI maps top = highest order = painted last (on top).
- **Deferred actions in GUI**: menu-bar closures borrow `&mut self`, so
  file/save/export/generate set `pending_*` flags consumed in the next frame.
- **Binary ↔ library import boundary**: GUI modules use
  `pa_painter::` (external crate path), not `crate::`.

## Project File Format (`.papr`)

A ZIP archive (Deflate) containing JSON metadata and embedded assets.
Current version: **2**.

```
├── manifest.json                  # version, app name, timestamps
├── project.json                   # unified: mesh_ref, layers, presets, settings, export_settings
├── assets/
│   ├── mesh.{glb,obj}             # embedded mesh binary (Stored, uncompressed)
│   ├── mesh.mtl                   # OBJ material file (OBJ meshes only)
│   ├── mesh_textures/*            # OBJ material textures (OBJ meshes only)
│   ├── layer_{n}_color.png        # per-layer File-mode colour texture
│   ├── layer_{n}_normal.png       # per-layer File-mode normal texture
│   └── …
└── editor.json                    # opaque editor UI state (camera, viewport, playback)
```

Output maps (colour, height, normal, stroke-time) are exported to a separate
directory — they are not stored inside the `.papr` archive.

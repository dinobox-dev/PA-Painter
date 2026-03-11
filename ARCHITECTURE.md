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

| Area | Modules |
|------|---------|
| Pipeline Stage 1 | `direction_field` |
| Pipeline Stage 2 | `path_placement` |
| Pipeline Stage 3 | `stroke_height`, `brush_profile` |
| Pipeline Stage 4 | `compositing`, `stroke_color`, `object_normal` |
| Pipeline Stage 5 | `output`, `glb_export` |
| Data model | `types`, `project`, `pressure` |
| I/O | `asset_io` (OBJ / glTF / PNG / EXR) |
| Utilities | `math`, `rng`, `stretch_map`, `uv_mask`, `error` |

Every pipeline module is **stateless** — pure functions with no side effects,
testable without a GUI or GPU.

### GUI (`src/gui/`)

egui/eframe application in `src/main_gui.rs`, with sub-modules:

| Module | Responsibility |
|--------|----------------|
| `mod` / `state` | App shell, central `AppState`, deferred-action pattern |
| `viewport` | UV and Guide viewports (pan/zoom, overlays) |
| `sidebar` | Left panel: mesh, colour, settings, layer list |
| `slot_editor` | Right panel: layer inspector, pressure-curve editor |
| `guide_editor` | Interactive guide placement and manipulation |
| `preview` | Stroke / path / preset thumbnail caches |
| `generation` | Background worker thread for full pipeline runs |
| `mesh_preview` | wgpu-based 3D preview |
| `dialogs` | File dialogs via rfd |
| `undo` | Snapshot-based undo with auto-coalescing |
| `textures` | Buffer → `TextureHandle` helpers |
| `widgets` | Custom egui widgets (icon buttons, sliders) |

### CLI (`src/main.rs`)

Loads a `.pap` project, runs the pipeline, and exports maps. No GUI dependency.

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

## Project File Format (`.pap`)

A ZIP archive (Deflate) containing JSON metadata, embedded assets, and cached
output maps. Current version: **1**.

```
├── manifest.json                  # version, app name, timestamps
├── project.json                   # unified: mesh_ref, layers, presets, settings, export_settings
├── assets/
│   ├── mesh.{glb,obj}             # embedded mesh binary (Stored, uncompressed)
│   ├── layer_0_color.png          # per-layer File-mode colour texture
│   ├── layer_0_normal.png         # per-layer File-mode normal texture
│   └── …
├── output/                        # cached generation results (optional)
│   ├── color.png                  # sRGB RGBA
│   ├── normal.png                 # linear RGB
│   ├── height.png                 # 16-bit grayscale
│   ├── stroke_time_order.png      # 16-bit grayscale
│   ├── stroke_time_arc.png        # 16-bit grayscale
│   └── snapshot.json              # generation-time state hash for staleness detection
├── thumbnails/
│   └── preview.png                # 256×256 preview thumbnail
└── editor.json                    # opaque editor UI state (camera, viewport, playback)
```

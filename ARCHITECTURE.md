# Architecture

This document describes the high-level architecture of Practical Arcana Painter.
See also the rustdoc on `lib.rs` for API-level documentation.

## Bird's Eye View

The project turns a UV-mapped 3D mesh, a base colour, and user-placed direction
guides into hand-painted-looking texture maps (colour, height, normal, stroke-ID)
through a deterministic, 5-stage CPU pipeline.

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
   Colour / Height / Normal
```

Everything runs in linear-float colour space; sRGB conversion happens only at I/O
boundaries. Identical parameters + seed always produce identical output (ChaCha8).

## Crate Layout

The project ships as **one library crate** with **two binaries** (CLI and GUI).

### Library (`src/`)

| Area | Modules |
|------|---------|
| Pipeline Stage 1 | `direction_field` |
| Pipeline Stage 2 | `path_placement`, `uv_mask` |
| Pipeline Stage 3 | `stroke_height`, `brush_profile` |
| Pipeline Stage 4 | `compositing`, `stroke_color`, `object_normal` |
| Pipeline Stage 5 | `output`, `glb_export` |
| Data model | `types`, `project`, `pressure` |
| I/O | `asset_io` (OBJ / glTF / PNG / EXR) |
| Utilities | `math`, `rng`, `stretch_map`, `error` |

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
  paint in seed-y order. The GUI maps top = highest order = painted last (on top).
- **Deferred actions in GUI**: menu-bar closures borrow `&mut self`, so
  file/save/export/generate set `pending_*` flags consumed in the next frame.
- **Binary ↔ library import boundary**: GUI modules use
  `practical_arcana_painter::` (external crate path), not `crate::`.

## Project File Format (`.pap`)

A ZIP archive (Deflate) containing JSON metadata + optional Bincode caches.
Current version: **1**.

```
├── manifest.json        # version, app name, timestamps
├── mesh_ref.json        # external mesh file path + format
├── base_sources.json    # base colour + base normal references
├── layer_stack.json     # Vec<Layer> (paint settings + guides)
├── presets.json         # PresetLibrary
├── settings.json        # OutputSettings (resolution, normal mode, …)
└── cache/               # optional Bincode caches
    ├── height_map.bin
    └── color_map.bin
```

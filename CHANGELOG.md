# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [0.2.1] — 2026-03-16

### Added

- In-app update check: on launch, checks for newer releases via GitHub API and shows a sky-blue banner below the menu bar with a download link to itch.io

### CI

- Release workflow now creates a GitHub Release (no binaries) linking to itch.io after butler publish completes

## [0.2.0] — 2026-03-15

### Changed

- Normal map Y axis now defaults to OpenGL convention (Y+up); added export setting to switch between OpenGL and DirectX conventions
- Compositing pipeline unified to premultiplied alpha blending, replacing separate transparent/opaque branches and shader-side background mixing
- Default base color changed from gray to white
- Tangent computation replaced with MikkTSpace algorithm for correct tangent-space normal maps
- CLI argument parsing migrated to clap derive for standard `--help` formatting
- f32-to-u8 color conversion now uses rounding instead of truncation

### Fixed

- Console window no longer appears when launching the GUI on Windows

### Distribution

- Pre-built binaries are now distributed exclusively via [itch.io](https://dinoboxgamedev.itch.io/papainter) (GitHub Releases removed)
- macOS universal binaries are code-signed with Developer ID and notarized by Apple
- Added Linux ARM64 build target

## [0.1.0] — 2026-03-11

### Added

- 5-stage CPU paint stroke pipeline: direction field, path placement, stroke height, compositing, output
- GUI editor with egui/eframe: viewport pan/zoom, layer management, pressure curve Bézier editor, guide tools, 3D mesh preview, undo/redo
- CLI renderer for headless batch processing with resolution override and format selection
- Layer system with independent brush settings, blend ordering, visibility, and mesh-group masks
- Guide types: directional, source, sink, and vortex with adjustable influence and strength
- Pressure curves: 8 built-in presets and custom cubic Bézier spline editor
- Output maps: color, normal (tangent-space and depicted-form), height, stroke ID, and stroke time (order + arc-length)
- Export formats: PNG, OpenEXR, GLB (with baked textures)
- Mesh import: OBJ and glTF/GLB with UV and vertex group support
- `.papr` project file format (ZIP-based) with auto-save and undo history
- Poisson-disk sampling with overscan and multi-pass refinement
- Color/normal boundary break detection for natural stroke termination
- GitHub Actions CI and cross-platform release workflows

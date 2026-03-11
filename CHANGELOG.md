# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [0.1.0] — 2026-03-02

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
- `.pap` project file format (ZIP-based) with auto-save and undo history
- Poisson-disk sampling with overscan and multi-pass refinement
- Color/normal boundary break detection for natural stroke termination
- GitHub Actions CI and cross-platform release workflows

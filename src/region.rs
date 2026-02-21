use glam::Vec2;

use crate::types::{Polygon, Region};

/// Axis-aligned bounding box in UV space.
#[derive(Debug, Clone, Copy)]
pub struct BBox {
    pub min: Vec2,
    pub max: Vec2,
}

/// Rasterized region mask. Row-major, true = inside region.
pub struct RasterMask {
    pub data: Vec<bool>,
    pub width: u32,
    pub height: u32,
}

/// Test if a point is inside a polygon using ray casting.
///
/// Casts a horizontal ray from the query point to +X infinity and counts
/// edge crossings. Odd count = inside.
pub fn point_in_polygon(point: Vec2, polygon: &Polygon) -> bool {
    let n = polygon.vertices.len();
    if n < 3 {
        return false;
    }

    let mut inside = false;
    let mut j = n - 1;

    for i in 0..n {
        let vi = polygon.vertices[i];
        let vj = polygon.vertices[j];

        if (vi.y > point.y) != (vj.y > point.y) {
            let x_intersect = vj.x + (point.y - vj.y) / (vi.y - vj.y) * (vi.x - vj.x);
            if point.x < x_intersect {
                inside = !inside;
            }
        }

        j = i;
    }

    inside
}

/// Test if a point is inside a region (union of mask polygons).
pub fn point_in_region(point: Vec2, region: &Region) -> bool {
    region.mask.iter().any(|poly| point_in_polygon(point, poly))
}

/// Compute the bounding box of a region's mask polygons.
pub fn region_bbox(region: &Region) -> BBox {
    let mut min = Vec2::new(f32::MAX, f32::MAX);
    let mut max = Vec2::new(f32::MIN, f32::MIN);

    for poly in &region.mask {
        for &v in &poly.vertices {
            min = min.min(v);
            max = max.max(v);
        }
    }

    BBox { min, max }
}

/// Rasterize a region mask at the given resolution.
/// The mask covers UV space [0,1] x [0,1].
pub fn rasterize_mask(region: &Region, resolution: u32) -> RasterMask {
    let size = (resolution * resolution) as usize;
    let mut data = vec![false; size];
    let inv_res = 1.0 / resolution as f32;

    for y in 0..resolution {
        for x in 0..resolution {
            let uv = Vec2::new((x as f32 + 0.5) * inv_res, (y as f32 + 0.5) * inv_res);
            if point_in_region(uv, region) {
                data[(y * resolution + x) as usize] = true;
            }
        }
    }

    RasterMask {
        data,
        width: resolution,
        height: resolution,
    }
}

impl RasterMask {
    /// Check if a UV coordinate is inside the rasterized mask.
    pub fn contains(&self, uv: Vec2) -> bool {
        let x = (uv.x * self.width as f32) as i32;
        let y = (uv.y * self.height as f32) as i32;
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return false;
        }
        self.data[(y as u32 * self.width + x as u32) as usize]
    }

    /// Check if a pixel coordinate is inside the rasterized mask.
    pub fn contains_px(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return false;
        }
        self.data[(y as u32 * self.width + x as u32) as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::StrokeParams;

    const EPS: f32 = 1e-5;

    fn make_polygon(verts: Vec<Vec2>) -> Polygon {
        Polygon { vertices: verts }
    }

    fn make_region(polygons: Vec<Polygon>) -> Region {
        Region {
            id: 0,
            name: String::from("test"),
            mask: polygons,
            order: 0,
            params: StrokeParams::default(),
            guides: vec![],
        }
    }

    // ── Point-in-Polygon Tests ──

    #[test]
    fn pip_inside_square() {
        let poly = make_polygon(vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ]);
        assert!(point_in_polygon(Vec2::new(0.5, 0.5), &poly));
    }

    #[test]
    fn pip_outside_square() {
        let poly = make_polygon(vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ]);
        assert!(!point_in_polygon(Vec2::new(1.5, 0.5), &poly));
    }

    #[test]
    fn pip_inside_triangle() {
        let poly = make_polygon(vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(0.5, 1.0),
        ]);
        assert!(point_in_polygon(Vec2::new(0.5, 0.3), &poly));
    }

    #[test]
    fn pip_outside_triangle() {
        let poly = make_polygon(vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(0.5, 1.0),
        ]);
        assert!(!point_in_polygon(Vec2::new(0.1, 0.9), &poly));
    }

    #[test]
    fn pip_concave_l_shape() {
        // L-shaped polygon: top-right notch
        let poly = make_polygon(vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 0.5),
            Vec2::new(0.5, 0.5),
            Vec2::new(0.5, 1.0),
            Vec2::new(0.0, 1.0),
        ]);
        // Center of the notch — should be outside
        assert!(!point_in_polygon(Vec2::new(0.75, 0.75), &poly));
        // Inside the arm
        assert!(point_in_polygon(Vec2::new(0.25, 0.75), &poly));
        // Inside the top part
        assert!(point_in_polygon(Vec2::new(0.75, 0.25), &poly));
    }

    #[test]
    fn pip_degenerate_two_points() {
        let poly = make_polygon(vec![Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0)]);
        assert!(!point_in_polygon(Vec2::new(0.5, 0.0), &poly));
    }

    #[test]
    fn pip_empty_polygon() {
        let poly = make_polygon(vec![]);
        assert!(!point_in_polygon(Vec2::ZERO, &poly));
    }

    // ── Point-in-Region Tests (Union) ──

    #[test]
    fn pir_two_non_overlapping_squares() {
        let left = make_polygon(vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(0.4, 0.0),
            Vec2::new(0.4, 1.0),
            Vec2::new(0.0, 1.0),
        ]);
        let right = make_polygon(vec![
            Vec2::new(0.6, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.6, 1.0),
        ]);
        let region = make_region(vec![left, right]);

        assert!(point_in_region(Vec2::new(0.2, 0.5), &region));
        assert!(point_in_region(Vec2::new(0.8, 0.5), &region));
        assert!(!point_in_region(Vec2::new(0.5, 0.5), &region));
    }

    #[test]
    fn pir_overlapping_squares() {
        let a = make_polygon(vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(0.6, 0.0),
            Vec2::new(0.6, 1.0),
            Vec2::new(0.0, 1.0),
        ]);
        let b = make_polygon(vec![
            Vec2::new(0.4, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.4, 1.0),
        ]);
        let region = make_region(vec![a, b]);

        assert!(point_in_region(Vec2::new(0.5, 0.5), &region));
    }

    // ── Bounding Box Tests ──

    #[test]
    fn bbox_unit_square() {
        let poly = make_polygon(vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ]);
        let region = make_region(vec![poly]);
        let bb = region_bbox(&region);
        assert!((bb.min.x - 0.0).abs() < EPS);
        assert!((bb.min.y - 0.0).abs() < EPS);
        assert!((bb.max.x - 1.0).abs() < EPS);
        assert!((bb.max.y - 1.0).abs() < EPS);
    }

    #[test]
    fn bbox_offset_square() {
        let poly = make_polygon(vec![
            Vec2::new(0.2, 0.3),
            Vec2::new(0.8, 0.3),
            Vec2::new(0.8, 0.7),
            Vec2::new(0.2, 0.7),
        ]);
        let region = make_region(vec![poly]);
        let bb = region_bbox(&region);
        assert!((bb.min.x - 0.2).abs() < EPS);
        assert!((bb.min.y - 0.3).abs() < EPS);
        assert!((bb.max.x - 0.8).abs() < EPS);
        assert!((bb.max.y - 0.7).abs() < EPS);
    }

    #[test]
    fn bbox_two_polygons() {
        let tri = make_polygon(vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(0.3, 0.0),
            Vec2::new(0.15, 0.3),
        ]);
        let sq = make_polygon(vec![
            Vec2::new(0.5, 0.5),
            Vec2::new(0.9, 0.5),
            Vec2::new(0.9, 0.9),
            Vec2::new(0.5, 0.9),
        ]);
        let region = make_region(vec![tri, sq]);
        let bb = region_bbox(&region);
        assert!((bb.min.x - 0.0).abs() < EPS);
        assert!((bb.min.y - 0.0).abs() < EPS);
        assert!((bb.max.x - 0.9).abs() < EPS);
        assert!((bb.max.y - 0.9).abs() < EPS);
    }

    // ── Raster Mask Tests ──

    #[test]
    fn raster_full_uv_square() {
        let poly = make_polygon(vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ]);
        let region = make_region(vec![poly]);
        let mask = rasterize_mask(&region, 10);
        assert_eq!(mask.width, 10);
        assert_eq!(mask.height, 10);
        assert!(mask.data.iter().all(|&v| v));
    }

    #[test]
    fn raster_half_uv() {
        let poly = make_polygon(vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(0.5, 0.0),
            Vec2::new(0.5, 1.0),
            Vec2::new(0.0, 1.0),
        ]);
        let region = make_region(vec![poly]);
        let mask = rasterize_mask(&region, 10);

        for y in 0..10 {
            for x in 0..10 {
                let inside = mask.contains_px(x as i32, y as i32);
                if x < 5 {
                    assert!(inside, "pixel ({x},{y}) should be inside");
                } else {
                    assert!(!inside, "pixel ({x},{y}) should be outside");
                }
            }
        }
    }

    #[test]
    fn raster_triangle_shape() {
        let poly = make_polygon(vec![
            Vec2::new(0.5, 0.1),
            Vec2::new(0.9, 0.9),
            Vec2::new(0.1, 0.9),
        ]);
        let region = make_region(vec![poly]);
        let mask = rasterize_mask(&region, 100);

        assert!(mask.contains(Vec2::new(0.5, 0.5)));
        assert!(!mask.contains(Vec2::new(0.05, 0.05)));
        assert!(!mask.contains(Vec2::new(0.95, 0.05)));
    }

    #[test]
    fn raster_contains_lookup() {
        let poly = make_polygon(vec![
            Vec2::new(0.2, 0.2),
            Vec2::new(0.8, 0.2),
            Vec2::new(0.8, 0.8),
            Vec2::new(0.2, 0.8),
        ]);
        let region = make_region(vec![poly]);
        let mask = rasterize_mask(&region, 20);

        assert!(mask.contains(Vec2::new(0.5, 0.5)));
        assert!(!mask.contains(Vec2::new(0.05, 0.05)));
        assert!(!mask.contains(Vec2::new(0.95, 0.95)));
    }

    #[test]
    fn raster_contains_out_of_bounds() {
        let poly = make_polygon(vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ]);
        let region = make_region(vec![poly]);
        let mask = rasterize_mask(&region, 10);

        assert!(!mask.contains(Vec2::new(-0.1, 0.5)));
        assert!(!mask.contains(Vec2::new(0.5, -0.1)));
        assert!(!mask.contains_px(-1, 5));
        assert!(!mask.contains_px(5, -1));
        assert!(!mask.contains_px(10, 5));
        assert!(!mask.contains_px(5, 10));
    }

    #[test]
    fn raster_matches_point_in_region() {
        let poly = make_polygon(vec![
            Vec2::new(0.3, 0.2),
            Vec2::new(0.7, 0.1),
            Vec2::new(0.9, 0.6),
            Vec2::new(0.5, 0.9),
            Vec2::new(0.1, 0.7),
        ]);
        let region = make_region(vec![poly]);
        let res = 50u32;
        let mask = rasterize_mask(&region, res);

        for y in 0..res {
            for x in 0..res {
                let uv = Vec2::new(
                    (x as f32 + 0.5) / res as f32,
                    (y as f32 + 0.5) / res as f32,
                );
                let expected = point_in_region(uv, &region);
                let actual = mask.data[(y * res + x) as usize];
                assert_eq!(
                    actual, expected,
                    "mismatch at pixel ({x},{y}), uv=({:.3},{:.3})",
                    uv.x, uv.y
                );
            }
        }
    }
}

//! Height map post-processing: diffusion (blur) and gradient computation.

use super::GlobalMaps;

/// Compute gradient_x / gradient_y from a height map using 3×3 Sobel.
///
/// Plain Sobel with no boundary replacement — the caller is responsible
/// for smoothing the height map beforehand (e.g. via [`diffuse_height`]).
pub(super) fn sobel_3x3(
    height: &[f32],
    gradient_x: &mut [f32],
    gradient_y: &mut [f32],
    resolution: u32,
) {
    let res = resolution as usize;
    for y in 0..res {
        for x in 0..res {
            let idx = y * res + x;
            if height[idx] <= 0.0 {
                continue;
            }

            let xl = x.saturating_sub(1);
            let xr = (x + 1).min(res - 1);
            let yl = y.saturating_sub(1);
            let yr = (y + 1).min(res - 1);

            let s = |sy: usize, sx: usize| height[sy * res + sx];

            let tl = s(yl, xl);
            let tc = s(yl, x);
            let tr = s(yl, xr);
            let ml = s(y, xl);
            let mr = s(y, xr);
            let bl = s(yr, xl);
            let bc = s(yr, x);
            let br = s(yr, xr);

            gradient_x[idx] = (tr + 2.0 * mr + br) - (tl + 2.0 * ml + bl);
            gradient_y[idx] = (bl + 2.0 * bc + br) - (tl + 2.0 * tc + tr);
        }
    }
}

/// Diffuse (blur) the height map to simulate paint spreading.
///
/// Low-viscosity paint spreads outward, producing soft edges and a smoother
/// surface. High-viscosity paint stays put, retaining sharp bristle ridges
/// and defined boundaries.
///
/// `spread` controls the blur radius: 0.0 = no diffusion, 1.0 = maximum.
/// Three passes of separable box blur approximate a Gaussian profile.
pub(super) fn diffuse_height(height: &mut [f32], resolution: u32, spread: f32) {
    if spread <= 0.0 {
        return;
    }
    let radius = (spread * 5.0).ceil() as usize;
    if radius == 0 {
        return;
    }
    let res = resolution as usize;
    let mut tmp = vec![0.0f32; res * res];

    // Three passes of separable box blur ≈ Gaussian.
    // Uses a running sum for O(1) per pixel regardless of radius.
    for _ in 0..3 {
        // Horizontal pass: height → tmp
        for y in 0..res {
            let row = y * res;
            let mut sum = 0.0f32;
            // Seed the window [0, radius]
            for x in 0..=radius.min(res - 1) {
                sum += height[row + x];
            }
            for x in 0..res {
                let win_size = (x + radius).min(res - 1) - x.saturating_sub(radius) + 1;
                tmp[row + x] = sum / win_size as f32;
                // Slide: add right edge, remove left edge
                if x + radius + 1 < res {
                    sum += height[row + x + radius + 1];
                }
                if x >= radius {
                    sum -= height[row + x - radius];
                }
            }
        }
        // Vertical pass: tmp → height
        for x in 0..res {
            let mut sum = 0.0f32;
            for y in 0..=radius.min(res - 1) {
                sum += tmp[y * res + x];
            }
            for y in 0..res {
                let win_size = (y + radius).min(res - 1) - y.saturating_sub(radius) + 1;
                height[y * res + x] = sum / win_size as f32;
                if y + radius + 1 < res {
                    sum += tmp[(y + radius + 1) * res + x];
                }
                if y >= radius {
                    sum -= tmp[(y - radius) * res + x];
                }
            }
        }
    }
}

/// Compute gradient_x / gradient_y from the global height map using 3×3 Sobel.
///
/// Legacy entry point kept for the global pipeline. Delegates to `sobel_3x3`.
pub fn compute_height_gradients(global: &mut GlobalMaps) {
    sobel_3x3(
        &global.height,
        &mut global.gradient_x,
        &mut global.gradient_y,
        global.resolution,
    );
}

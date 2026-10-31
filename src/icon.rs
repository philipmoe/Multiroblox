//! The app icon, drawn in code: two overlapping windows on a blue tile, for "several clients".

/// Coverage (0..1) of a rounded rectangle at pixel (x, y), anti-aliased over one pixel.
fn rounded_rect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r;
    (0.5 - d).clamp(0.0, 1.0)
}

/// Paints `src` with opacity `a` over `dst` (straight, not premultiplied, alpha).
fn blend(dst: &mut [f32; 4], src: [f32; 3], a: f32) {
    let out_a = a + dst[3] * (1.0 - a);
    if out_a <= 0.0 {
        return;
    }
    for i in 0..3 {
        dst[i] = (src[i] * a + dst[i] * dst[3] * (1.0 - a)) / out_a;
    }
    dst[3] = out_a;
}

/// RGBA pixels of the icon at `size` x `size`.
pub fn rgba(size: u32) -> Vec<u8> {
    let s = size as f32 / 32.0;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            let mut c = [0.0f32; 4];
            // Tile: blue to violet from top-left to bottom-right.
            let t = (x + y) / (2.0 * size as f32);
            let tile = [0.20 + 0.25 * t, 0.45 - 0.15 * t, 1.0];
            blend(&mut c, tile, rounded_rect(x, y, 0.5 * s, 0.5 * s, 31.5 * s, 31.5 * s, 7.0 * s));
            // Back window, see-through.
            let back = rounded_rect(x, y, 6.0 * s, 6.0 * s, 21.0 * s, 19.0 * s, 2.5 * s);
            blend(&mut c, [1.0, 1.0, 1.0], back * 0.55);
            // Front window with a title bar.
            let front = rounded_rect(x, y, 11.0 * s, 12.0 * s, 26.0 * s, 25.0 * s, 2.5 * s);
            blend(&mut c, [1.0, 1.0, 1.0], front);
            let bar = rounded_rect(x, y, 11.0 * s, 12.0 * s, 26.0 * s, 15.5 * s, 1.5 * s) * front;
            blend(&mut c, tile, bar * 0.35);
            let a = c[3].clamp(0.0, 1.0);
            out.extend_from_slice(&[
                (c[0] * 255.0) as u8,
                (c[1] * 255.0) as u8,
                (c[2] * 255.0) as u8,
                (a * 255.0) as u8,
            ]);
        }
    }
    out
}

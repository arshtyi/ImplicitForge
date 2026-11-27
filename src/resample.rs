//! Resample packed RGB8 at pixel centers with clamped edges.

#[inline]
fn neighbors(dst: usize, scale: f32, last: usize) -> (usize, usize, u32) {
    let u = ((dst as f32 + 0.5) * scale - 0.5).clamp(0.0, last as f32);
    let left = u as usize;
    (
        left,
        (left + 1).min(last),
        ((u - left as f32) * 256.0).round() as u32,
    )
}

// Two packed lanes with eight bits of space for each fixed-point weight.
#[inline]
fn mix(a: u32, b: u32, weight: u32) -> u32 {
    const RB: u32 = 0xff00ff;
    const G: u32 = 0xff00;
    let inv = 256 - weight;
    let rb = (((a & RB) * inv + (b & RB) * weight + 0x800080) >> 8) & RB;
    let g = (((a & G) * inv + (b & G) * weight + 0x8000) >> 8) & G;
    rb | g
}

/// Write a view into a larger framebuffer. `dest` begins at the view's top left.
/// Full-size frames bypass interpolation to preserve their rendered values.
pub fn blit(src: &[u32], size: [usize; 2], dest: &mut [u32], target: [usize; 2], stride: usize) {
    let [sw, sh] = size;
    let [dw, dh] = target;
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return;
    }
    let xs = sw as f32 / dw as f32;
    let ys = sh as f32 / dh as f32;
    for (y, row) in dest.chunks_mut(stride).take(dh).enumerate() {
        if size == target {
            row[..dw].copy_from_slice(&src[y * sw..(y + 1) * sw]);
            continue;
        }
        let (y0, y1, wy) = neighbors(y, ys, sh - 1);
        let a = &src[y0 * sw..(y0 + 1) * sw];
        let b = &src[y1 * sw..(y1 + 1) * sw];
        for (x, dst) in row[..dw].iter_mut().enumerate() {
            let (x0, x1, wx) = neighbors(x, xs, sw - 1);
            *dst = mix(mix(a[x0], a[x1], wx), mix(b[x0], b[x1], wx), wy);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enlarges_between_pixel_centers_without_touching_padding() {
        let mut buf = [0xabcdef; 10];
        blit(&[0xff0000, 0x0000ff], [2, 1], &mut buf, [4, 2], 5);
        assert_eq!(
            buf,
            [0xff0000, 0xbf0040, 0x4000bf, 0x0000ff, 0xabcdef]
                .repeat(2)
                .as_slice()
        );
    }
}

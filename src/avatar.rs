pub struct Avatar;

fn clamp_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.299 * r + 0.587 * g + 0.114 * b
}

fn chroma_u(r: f32, g: f32, b: f32) -> f32 {
    128.0 - 0.168736 * r - 0.331264 * g + 0.5 * b
}

fn chroma_v(r: f32, g: f32, b: f32) -> f32 {
    128.0 + 0.5 * r - 0.418688 * g - 0.081312 * b
}

pub fn rgba_to_yuyv(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let mut out = vec![0u8; w * h * 2];
    let mut oi = 0;
    for row in 0..h {
        let mut col = 0;
        while col + 1 < w {
            let i0 = (row * w + col) * 4;
            let i1 = (row * w + col + 1) * 4;
            let (r0, g0, b0) = (rgba[i0] as f32, rgba[i0 + 1] as f32, rgba[i0 + 2] as f32);
            let (r1, g1, b1) = (rgba[i1] as f32, rgba[i1 + 1] as f32, rgba[i1 + 2] as f32);
            let y0 = clamp_u8(luma(r0, g0, b0));
            let y1 = clamp_u8(luma(r1, g1, b1));
            let u = clamp_u8((chroma_u(r0, g0, b0) + chroma_u(r1, g1, b1)) * 0.5);
            let v = clamp_u8((chroma_v(r0, g0, b0) + chroma_v(r1, g1, b1)) * 0.5);
            out[oi] = y0;
            out[oi + 1] = u;
            out[oi + 2] = y1;
            out[oi + 3] = v;
            oi += 4;
            col += 2;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_pixels_map_to_full_luma_neutral_chroma() {
        let rgba = vec![255u8; 2 * 4];
        let yuyv = rgba_to_yuyv(&rgba, 2, 1);
        assert_eq!(yuyv.len(), 2 * 2);
        assert_eq!(yuyv[0], 255);
        assert_eq!(yuyv[2], 255);
        assert_eq!(yuyv[1], 128);
        assert_eq!(yuyv[3], 128);
    }

    #[test]
    fn black_pixels_map_to_zero_luma() {
        let rgba = vec![0u8, 0, 0, 255, 0, 0, 0, 255];
        let yuyv = rgba_to_yuyv(&rgba, 2, 1);
        assert_eq!(yuyv[0], 0);
        assert_eq!(yuyv[2], 0);
        assert_eq!(yuyv[1], 128);
        assert_eq!(yuyv[3], 128);
    }
}

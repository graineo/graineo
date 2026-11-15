//! Extended-sRGB float storage for the same defringe algorithm.
use super::*;
fn px(p:&[f32])->[f32;3]{[p[0],p[1],p[2]]}
pub fn apply(data: &mut [f32], w:usize, h:usize, s: &Settings, geo: Geo) {
    if !active(s) {
        return;
    }
    if w < 3 || h < 3 {
        return;
    }
    let bands: Vec<Band> = [
        Band::new(s.defringe_purple_amount, s.defringe_purple_hue, purple_hue_deg),
        Band::new(s.defringe_green_amount, s.defringe_green_hue, green_hue_deg),
    ]
    .into_iter()
    .flatten()
    .collect();
    let a = max_amount(s);
    // Radius in full image pixels grows with the amount (as in common photo apps, a higher
    // amount also acts wider), converted to output pixels
    let r_full = 1.5 + 4.5 * a;
    let r = (r_full / geo.scale.max(1.0)).round().max(1.0) as usize;
    let r2 = 3 * r;
    let norm = 1.0 / ((2 * r2 + 1) as f32 * (2 * r2 + 1) as f32 * 255.0);
    // Edge threshold: brightness span of the encoded luminance in the neighbourhood
    let (t0, t1) = (0.16 - 0.06 * a, 0.36 - 0.1 * a);

    // 1. Per pixel: luminance (bits 24–31), fringe membership × 255 (16–23), colour differences
    //    (0–15). The running maximum/minimum thus yields membership and colour of the
    //    brightest or darkest point at the same time.
    let mut packed = vec![0u32; w * h];
    let row_has: Vec<bool> = packed
        .par_chunks_mut(w)
        .zip(data.par_chunks(w * 3))
        .map(|(o, src)| {
            let mut any = false;
            for x in 0..w {
                let p = &src[x * 3..x * 3 + 3];
                let v = px(p);
                let l = 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2];
                // fast pre-test for chroma
                let (mx, mn) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
                let c = if mx - mn > 1966.0/65535.0 { (membership(v, &bands).0 * 255.0 + 0.5) as u32 } else { 0 };
                any |= c > 0;
                o[x] = ((l * 255.0 + 0.5) as u32).min(255) << 24 | c << 16 | chroma_bytes(v, l);
            }
            any
        })
        .collect();
    if !row_has.iter().any(|&b| b) {
        return;
    }

    // 2. Strip by strip: horizontal running maximum/minimum over ±r (with margin), then per
    //    candidate vertically, edge strength, object protection and correction
    data.par_chunks_mut(w * 3 * STRIP).enumerate().for_each(|(si, strip)| {
        let (y0, y1) = (si * STRIP, (si * STRIP + STRIP).min(h));
        if !row_has[y0..y1].iter().any(|&b| b) {
            return;
        }
        let (ha, hb) = (y0.saturating_sub(r), (y1 + r).min(h));
        let mut hmax = vec![0u32; (hb - ha) * w];
        let mut hmin = vec![0u32; (hb - ha) * w];
        let (mut g, mut hh) = (vec![0u32; w], vec![0u32; w]);
        for yy in ha..hb {
            let src = &packed[yy * w..yy * w + w];
            let o = (yy - ha) * w;
            running(src, r, &mut hmax[o..o + w], &mut g, &mut hh, u32::max);
            running(src, r, &mut hmin[o..o + w], &mut g, &mut hh, u32::min);
        }
        // Horizontal sums of membership over ±r2 (running, border extended)
        let (sa, sb) = (y0.saturating_sub(r2), (y1 + r2).min(h));
        let mut hsum = vec![0u16; (sb - sa) * w];
        for yy in sa..sb {
            let c = |x: isize| packed[yy * w + x.clamp(0, w as isize - 1) as usize] >> 16 & 0xff;
            let o = &mut hsum[(yy - sa) * w..(yy - sa + 1) * w];
            let mut acc: u32 = (-(r2 as isize)..=r2 as isize).map(c).sum();
            for x in 0..w {
                o[x] = acc as u16;
                acc = acc + c(x as isize + r2 as isize + 1) - c(x as isize - r2 as isize);
            }
        }
        for y in y0..y1 {
            if !row_has[y] {
                continue;
            }
            let row = &packed[y * w..y * w + w];
            let line = &mut strip[(y - y0) * w * 3..(y - y0 + 1) * w * 3];
            let (ya, yb) = (y.saturating_sub(r) - ha, (y + r).min(h - 1) - ha);
            for x in 0..w {
                if row[x] >> 16 & 0xff == 0 {
                    continue;
                }
                let (mut mx, mut mn) = (0u32, u32::MAX);
                for yy in ya..=yb {
                    mx = mx.max(hmax[yy * w + x]);
                    mn = mn.min(hmin[yy * w + x]);
                }
                let (lmax, lmin) = ((mx >> 24) as f32 / 255.0, (mn >> 24) as f32 / 255.0);
                let edge = smoothstep(t0, t1, lmax - lmin);
                if edge <= 0.0 {
                    continue;
                }
                // If the brightest or darkest point is itself fringe-coloured, it is an object
                let ext = (mx >> 16 & 0xff).max(mn >> 16 & 0xff) as f32 / 255.0;
                let mut protect = 1.0 - smoothstep(0.05, 0.3, ext);
                if protect <= 0.0 {
                    continue;
                }
                let mut sum = 0u32;
                for dy in -(r2 as isize)..=r2 as isize {
                    let yy = (y as isize + dy).clamp(0, h as isize - 1) as usize;
                    sum += hsum[(yy - sa) * w + x] as u32;
                }
                protect *= 1.0 - smoothstep(0.3, 0.55, sum as f32 * norm);
                if protect <= 0.0 {
                    continue;
                }
                let p = &mut line[x * 3..x * 3 + 3];
                let v = px(p);
                let (m, k) = membership(v, &bands);
                let t = (k * m * edge * protect).min(1.0);
                if t <= 0.0 {
                    continue;
                }
                // Target colour: between darkest and brightest point according to the pixel's own brightness
                let l = 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2];
                let u = ((l - lmin) / (lmax - lmin).max(1e-3)).clamp(0.0, 1.0);
                let (cmax, cmin) = (chroma_of(mx), chroma_of(mn));
                let target = [cmin[0] + u * (cmax[0] - cmin[0]), cmin[1] + u * (cmax[1] - cmin[1])];
                let (cb, cr) = (v[2] - l, v[0] - l);
                let (cb, cr) = (cb + t * (target[0] - cb), cr + t * (target[1] - cr));
                let (rr, bb) = (l + cr, l + cb);
                let gg = (l - 0.2126 * rr - 0.0722 * bb) / 0.7152;
                for (o, c) in p.iter_mut().zip([rr, gg, bb]) {
                    *o = c;
                }
            }
        }
    });
}

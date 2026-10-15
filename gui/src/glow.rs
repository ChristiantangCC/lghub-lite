//! A real blurred glow behind text, like CSS `text-shadow: 0 0 <blur>px <color>`.
//!
//! The text's glyph coverage is copied out of egui's font atlas, blurred with a Gaussian
//! (sigma = blur / 2, as browsers do) and uploaded as a texture that is drawn behind the text.
//! The texture is cached and rebuilt only when the text, colour, font or scale changes.

use std::sync::Arc;

use eframe::egui::{self, Color32, ColorImage, Galley, Painter, Pos2, Rect, TextureHandle, TextureOptions, pos2, vec2};

#[derive(Clone)]
struct Cached {
    key: (String, [u8; 4], u32, u32, bool),
    tex: TextureHandle,
    /// Where the texture goes, relative to the galley's top-left, in points.
    rect: Rect,
}

/// Paint `galley` at `at` with a glow of `blur` px in `color` (alpha included) behind it.
/// `smear` also adds the coverage shifted 1 pt right (the fake bold of single-weight fonts).
pub fn paint(p: &Painter, at: Pos2, galley: &Arc<Galley>, blur: f32, color: Color32, smear: bool) {
    let ctx = p.ctx();
    let ppp = ctx.pixels_per_point();
    let key = (
        galley.text().to_string(),
        color.to_array(),
        blur.to_bits(),
        ppp.to_bits(),
        smear,
    );
    // one glowing text per layer (the big percentage), so one cache slot is enough
    let id = p.layer_id().id.with("text-glow");
    let cached = ctx
        .data(|d| d.get_temp::<Cached>(id))
        .filter(|c| c.key == key)
        .or_else(|| {
            let (image, rect) = render(ctx, galley, blur, color, smear)?;
            let tex = ctx.load_texture("text-glow", image, TextureOptions::LINEAR);
            let c = Cached { key, tex, rect };
            ctx.data_mut(|d| d.insert_temp(id, c.clone()));
            Some(c)
        });
    if let Some(c) = cached {
        let r = c.rect.translate(at.to_vec2());
        p.image(
            c.tex.id(),
            r,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

fn render(ctx: &egui::Context, galley: &Galley, blur: f32, color: Color32, smear: bool) -> Option<(ColorImage, Rect)> {
    let ppp = ctx.pixels_per_point();
    let sigma = blur / 2.0 * ppp;
    let pad = (sigma * 3.0).ceil() as usize + 1;
    let size = galley.size() * ppp;
    let (w, h) = (size.x.ceil() as usize + 2 * pad + 2, size.y.ceil() as usize + 2 * pad);
    let atlas = ctx.fonts(|f| f.image());
    let mut cov = vec![0f32; w * h];
    for pr in &galley.rows {
        for g in &pr.row.glyphs {
            let uv = g.uv_rect;
            if uv.is_nothing() {
                continue;
            }
            let lt = (pr.pos + g.pos.to_vec2() + uv.offset) * ppp;
            let (gx, gy) = (
                lt.x.round() as isize + pad as isize,
                lt.y.round() as isize + pad as isize,
            );
            let (gw, gh) = ((uv.max[0] - uv.min[0]) as usize, (uv.max[1] - uv.min[1]) as usize);
            for shift in 0..=(smear as isize) {
                let sx = (shift as f32 * ppp).round() as isize;
                for y in 0..gh {
                    for x in 0..gw {
                        let a = atlas.pixels[(uv.min[1] as usize + y) * atlas.size[0] + uv.min[0] as usize + x].a();
                        let (dx, dy) = (gx + sx + x as isize, gy + y as isize);
                        if a > 0 && dx >= 0 && dy >= 0 && (dx as usize) < w && (dy as usize) < h {
                            let c = &mut cov[dy as usize * w + dx as usize];
                            *c = c.max(a as f32 / 255.0);
                        }
                    }
                }
            }
        }
    }
    if cov.iter().all(|&c| c == 0.0) {
        return None;
    }
    let blurred = gaussian(&cov, w, h, sigma);
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    let pixels = blurred
        .iter()
        .flat_map(|&v| [r, g, b, (v * a as f32).round().clamp(0.0, 255.0) as u8])
        .collect::<Vec<u8>>();
    let image = ColorImage::from_rgba_unmultiplied([w, h], &pixels);
    let rect = Rect::from_min_size(pos2(-(pad as f32), -(pad as f32)) / ppp, vec2(w as f32, h as f32) / ppp);
    Some((image, rect))
}

/// Separable Gaussian blur of a coverage map.
fn gaussian(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let r = (sigma * 3.0).ceil() as isize;
    let mut k: Vec<f32> = (-r..=r)
        .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f32 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= sum);
    let pass = |src: &[f32], horizontal: bool| {
        let mut out = vec![0f32; w * h];
        for y in 0..h as isize {
            for x in 0..w as isize {
                let mut acc = 0.0;
                for (i, kv) in k.iter().enumerate() {
                    let o = i as isize - r;
                    let (sx, sy) = if horizontal { (x + o, y) } else { (x, y + o) };
                    if sx >= 0 && sy >= 0 && sx < w as isize && sy < h as isize {
                        acc += src[sy as usize * w + sx as usize] * kv;
                    }
                }
                out[y as usize * w + x as usize] = acc;
            }
        }
        out
    };
    pass(&pass(src, true), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaussian_keeps_mass_and_spreads() {
        let (w, h) = (41, 41);
        let mut src = vec![0f32; w * h];
        src[20 * w + 20] = 1.0;
        let out = gaussian(&src, w, h, 3.0);
        let total: f32 = out.iter().sum();
        assert!((total - 1.0).abs() < 1e-3, "{total}");
        assert!(out[20 * w + 20] < 0.05 && out[20 * w + 23] > 0.0);
        // symmetric
        assert!((out[20 * w + 17] - out[20 * w + 23]).abs() < 1e-6);
    }
}

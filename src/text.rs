use std::collections::HashMap;

use ab_glyph::{Font, FontRef, FontVec, GlyphId, ScaleFont, point};
use fontdb::{Database, Family, Query};

/// One face supplies the metrics and coverage masks for the entire interface.
pub struct TextRenderer {
    font: FontVec,
    masks: HashMap<(GlyphId, u32, u8), Mask>,
    dpi: f32,
}

pub struct TextSurface<'a> {
    pub pixels: &'a mut [u32],
    pub stride: usize,
    pub clip: [usize; 2],
}

struct Mask {
    offset: [i32; 2],
    width: usize,
    alpha: Vec<u8>,
}

impl Mask {
    fn rasterize(font: &FontVec, id: GlyphId, px: f32, quarter: u8) -> Self {
        let glyph = id.with_scale_and_position(px, point(f32::from(quarter) * 0.25, 0.0));
        let Some(outline) = font.outline_glyph(glyph) else {
            return Self {
                offset: [0, 0],
                width: 0,
                alpha: Vec::new(),
            };
        };
        let bounds = outline.px_bounds();
        let width = bounds.width() as usize;
        let mut alpha = vec![0; width * bounds.height() as usize];
        outline.draw(|x, y, coverage| {
            alpha[y as usize * width + x as usize] = (coverage * 255.0).round() as u8;
        });
        Self {
            offset: [bounds.min.x as i32, bounds.min.y as i32],
            width,
            alpha,
        }
    }
}

// Measure, wrap and paint with exactly the same advances and pair kerning.
fn positions<'a>(
    font: &'a FontVec,
    value: &'a str,
    px: f32,
) -> impl Iterator<Item = (usize, char, GlyphId, f32, f32)> + 'a {
    let scaled = font.as_scaled(px);
    let mut previous = None;
    let mut pen = 0.;
    value.char_indices().map(move |(index, ch)| {
        let mut id = scaled.glyph_id(ch);
        if id.0 == 0 {
            id = scaled.glyph_id('?');
        }
        if let Some(prev) = previous {
            pen += scaled.kern(prev, id);
        }
        let start = pen;
        pen += scaled.h_advance(id);
        previous = Some(id);
        (index, ch, id, start, pen)
    })
}

impl TextRenderer {
    pub fn new() -> Result<Self, &'static str> {
        let mut db = Database::new();
        db.load_system_fonts();
        Self::from_database(&db)
    }

    fn from_database(db: &Database) -> Result<Self, &'static str> {
        let families = [
            Family::Name("Inter"),
            Family::Name("Segoe UI"),
            Family::Name("Helvetica Neue"),
            Family::Name("Noto Sans"),
            Family::Name("DejaVu Sans"),
            Family::Name("Liberation Sans"),
            Family::SansSerif,
        ];
        let preferred = families.iter().filter_map(|family| {
            db.query(&Query {
                families: std::slice::from_ref(family),
                ..Query::default()
            })
        });
        for id in preferred.chain(db.faces().map(|face| face.id)) {
            let font = db.with_face_data(id, |bytes, index| {
                let face = FontRef::try_from_slice_and_index(bytes, index).ok()?;
                if !(face.height_unscaled() > 0.
                    && (b' '..=b'~').all(|byte| {
                        let id = face.glyph_id(char::from(byte));
                        id.0 != 0
                            && if byte == b' ' {
                                face.h_advance_unscaled(id) > 0.
                            } else {
                                face.outline(id).is_some()
                            }
                    }))
                {
                    return None;
                }
                FontVec::try_from_vec_and_index(bytes.to_vec(), index).ok()
            });
            if let Some(Some(font)) = font {
                return Ok(Self {
                    font,
                    masks: HashMap::new(),
                    dpi: 0.,
                });
            }
        }
        Err(
            "No usable system font for the UI. Install an outline font covering printable ASCII, such as DejaVu Sans or Noto Sans.",
        )
    }

    pub fn set_dpi(&mut self, dpi: f32) {
        if self.dpi != dpi {
            self.masks.clear();
            self.dpi = dpi;
        }
    }

    pub fn width(&self, text: &str, px: f32) -> f32 {
        positions(&self.font, text, px)
            .last()
            .map_or(0., |(_, _, _, _, end)| end)
    }

    /// Return a borrowed line and remainder. Split oversized tokens only at character boundaries.
    pub fn wrap_line<'a>(&self, text: &'a str, px: f32, width: f32) -> (&'a str, &'a str) {
        let text = text.trim_start_matches(|c: char| c != '\n' && c.is_whitespace());
        let mut space = None;
        for (index, ch, _, _, end) in positions(&self.font, text, px) {
            if ch == '\n' {
                return (text[..index].trim_end(), &text[index + 1..]);
            }
            if ch.is_whitespace() {
                space = Some(index);
            }
            if end > width {
                let split = space.unwrap_or(if index == 0 { ch.len_utf8() } else { index });
                return (text[..split].trim_end(), text[split..].trim_start());
            }
        }
        (text.trim_end(), "")
    }

    /// Coordinates, clip bounds (exclusive), and font height are in physical pixels.
    pub fn draw(
        &mut self,
        surface: TextSurface<'_>,
        origin: [usize; 2],
        px: f32,
        color: u32,
        text: &str,
    ) {
        let TextSurface {
            pixels,
            stride,
            clip,
        } = surface;
        let baseline = origin[1] as i32 + self.font.as_scaled(px).ascent().round() as i32;
        for (_, _, id, start, _) in positions(&self.font, text, px) {
            let quarter_x = (start * 4.).round() as i32;
            let quarter = quarter_x.rem_euclid(4) as u8;
            let mask = self
                .masks
                .entry((id, px.to_bits(), quarter))
                .or_insert_with(|| Mask::rasterize(&self.font, id, px, quarter));
            let left = origin[0] as i32 + quarter_x.div_euclid(4) + mask.offset[0];
            if left >= clip[0] as i32 {
                break;
            }
            if mask.width == 0 {
                continue;
            }
            let top = baseline + mask.offset[1];
            let x0 = left.max(0) as usize;
            let x1 = (left + mask.width as i32).clamp(0, clip[0] as i32) as usize;
            if x0 >= x1 {
                continue;
            }
            let skip = (x0 as i32 - left) as usize;
            for (dy, row) in mask.alpha.chunks_exact(mask.width).enumerate() {
                let y = top + dy as i32;
                if y < 0 {
                    continue;
                }
                if y >= clip[1] as i32 {
                    break;
                }
                let start = y as usize * stride;
                for (dest, &coverage) in pixels[start + x0..start + x1].iter_mut().zip(&row[skip..])
                {
                    let a = u32::from(coverage);
                    let channel = |shift: u32| {
                        let bg = (*dest >> shift) & 255;
                        let fg = (color >> shift) & 255;
                        ((bg * (255 - a) + fg * a + 127) / 255) << shift
                    };
                    *dest = channel(16) | channel(8) | channel(0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_database_reports_actionable_error() {
        assert!(
            TextRenderer::from_database(&Database::new())
                .err()
                .unwrap()
                .contains("Install")
        );
    }

    #[test]
    fn system_face_wraps_using_its_metrics_and_reuses_masks() {
        // Fontless hosts can still exercise the explicit error path above.
        let Ok(mut text) = TextRenderer::new() else {
            return;
        };
        let px = 18.75;
        let font = text.font.as_scaled(px);
        let (a, v) = (font.glyph_id('A'), font.glyph_id('V'));
        let expected = font.h_advance(a) + font.kern(a, v) + font.h_advance(v);
        assert!((text.width("AV", px) - expected).abs() < 0.001);
        assert_eq!(text.wrap_line("AV AV", px, expected + 0.1), ("AV", "AV"));
        assert_eq!(text.wrap_line("éé", px, 0.), ("é", "é"));
        let mut pixels = vec![0; 128 * 32];
        text.draw(
            TextSurface {
                pixels: &mut pixels,
                stride: 128,
                clip: [128, 32],
            },
            [0, 0],
            px,
            0xffffff,
            "AV",
        );
        let cached = text.masks.len();
        assert!(pixels.iter().any(|&p| p != 0 && p != 0xffffff));
        text.draw(
            TextSurface {
                pixels: &mut pixels,
                stride: 128,
                clip: [128, 32],
            },
            [0, 0],
            px,
            0xffffff,
            "AV",
        );
        assert_eq!(text.masks.len(), cached);
    }
}

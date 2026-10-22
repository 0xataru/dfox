//! The dfox mark: a fox head of flat-coloured triangles, drawn with half
//! blocks so each cell holds two square pixels. On the splash its pixels fly
//! in from the left and settle into the head, like rows arriving.

use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

use crate::theme::{ColorDepth, Rgb};

/// The facets in a 100×100 box, back to front: three corners and a colour.
/// The same shapes as `examples/logo.svg`.
const FACETS: [([f32; 6], u32); 16] = [
    ([12., 6., 40., 30., 22., 44.], 0xff9e3b),
    ([17., 15., 33., 29., 23., 37.], 0xb8561a),
    ([88., 6., 60., 30., 78., 44.], 0xf08a2a),
    ([83., 15., 67., 29., 77., 37.], 0x9c4714),
    ([40., 30., 60., 30., 50., 50.], 0xffb366),
    ([22., 44., 40., 30., 50., 50.], 0xff9e3b),
    ([78., 44., 60., 30., 50., 50.], 0xe8862a),
    ([6., 58., 22., 44., 36., 66.], 0xe6e6ef),
    ([94., 58., 78., 44., 64., 66.], 0xc9cad8),
    ([22., 44., 50., 50., 36., 66.], 0xe07f25),
    ([78., 44., 50., 50., 64., 66.], 0xc96f1e),
    ([36., 66., 50., 50., 50., 94.], 0xf4f4fa),
    ([64., 66., 50., 50., 50., 94.], 0xd6d7e3),
    ([45., 87., 55., 87., 50., 94.], 0x16161e),
    ([29., 49., 39., 54., 31., 56.], 0x16161e),
    ([71., 49., 61., 54., 69., 56.], 0x16161e),
];

/// The fox, assembled to `progress`: 0 shows nothing, 1 the whole head.
pub struct Fox {
    pub progress: f32,
    pub depth: ColorDepth,
}

impl Fox {
    /// Cells it takes `rows` high: two pixels a row, square.
    pub fn width(rows: u16) -> u16 {
        rows * 2
    }
}

/// The facet colour at (`x`, `y`) of the 100×100 box, if any.
fn colour_at(x: f32, y: f32) -> Option<u32> {
    let side = |ax: f32, ay: f32, bx: f32, by: f32| (x - bx) * (ay - by) - (ax - bx) * (y - by);
    FACETS
        .iter()
        .rev()
        .find(|([ax, ay, bx, by, cx, cy], _)| {
            let d = [
                side(*ax, *ay, *bx, *by),
                side(*bx, *by, *cx, *cy),
                side(*cx, *cy, *ax, *ay),
            ];
            !(d.iter().any(|v| *v < 0.) && d.iter().any(|v| *v > 0.))
        })
        .map(|(_, colour)| *colour)
}

/// A number in 0..1 that is always the same for `n`.
fn noise(n: u32) -> f32 {
    let mut x = n.wrapping_mul(0x9e37_79b9) ^ 0x85eb_ca6b;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2c1b_3c6d);
    x ^= x >> 12;
    (x & 0xffff) as f32 / 65535.
}

impl Widget for Fox {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let size = area.height.min(area.width / 2) as usize * 2;
        if size == 0 {
            return;
        }
        let left = area.x + (area.width - size as u16) / 2;
        // Pixels where they are this frame, `size` square.
        let mut pixels: Vec<Option<u32>> = vec![None; size * size];
        let scale = 100. / size as f32;
        for y in 0..size {
            for x in 0..size {
                let Some(colour) = colour_at((x as f32 + 0.5) * scale, (y as f32 + 0.5) * scale)
                else {
                    continue;
                };
                // Left pixels set off first; each takes half of the time.
                let start = x as f32 / size as f32 * 0.5;
                let own = ((self.progress - start) / 0.5).clamp(0., 1.);
                if own == 0. {
                    continue;
                }
                let away = (1. - own).powi(3);
                let i = (y * size + x) as u32;
                let dx = -(0.4 + noise(i) * 0.8) * size as f32 * away;
                let dy = (noise(i + 7919) - 0.5) * 0.4 * size as f32 * away;
                let (px, py) = (x as f32 + dx, y as f32 + dy);
                if px < 0. || py < 0. || px >= size as f32 || py >= size as f32 {
                    continue;
                }
                pixels[py as usize * size + px as usize] = Some(colour);
            }
        }
        let colour = |c: u32| Rgb((c >> 16) as u8, (c >> 8) as u8, c as u8).to_color(self.depth);
        for row in 0..size / 2 {
            for x in 0..size {
                let top = pixels[row * 2 * size + x];
                let bottom = pixels[(row * 2 + 1) * size + x];
                let cell = &mut buf[(left + x as u16, area.y + row as u16)];
                match (top, bottom) {
                    (None, None) => {}
                    (Some(top), None) => {
                        cell.set_symbol("▀").set_fg(colour(top));
                    }
                    (None, Some(bottom)) => {
                        cell.set_symbol("▄").set_fg(colour(bottom));
                    }
                    (Some(top), Some(bottom)) => {
                        cell.set_symbol("▀")
                            .set_fg(colour(top))
                            .set_bg(colour(bottom));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw(progress: f32) -> Buffer {
        let area = Rect::new(0, 0, 24, 12);
        let mut buf = Buffer::empty(area);
        Fox {
            progress,
            depth: ColorDepth::TrueColor,
        }
        .render(area, &mut buf);
        buf
    }

    fn drawn(buf: &Buffer) -> usize {
        buf.content().iter().filter(|c| c.symbol() != " ").count()
    }

    #[test]
    fn the_fox_assembles_from_nothing() {
        assert_eq!(drawn(&draw(0.)), 0);
        let half = drawn(&draw(0.5));
        let whole = draw(1.);
        assert!(half > 0 && half < drawn(&whole), "{half}");
        // An ear at the top left, the muzzle at the bottom middle, nothing
        // in the bottom corner.
        let ear = (0..3).flat_map(|y| (0..8).map(move |x| (x, y)));
        assert!(ear
            .map(|at| &whole[at])
            .any(|cell| cell.fg == Rgb(0xff, 0x9e, 0x3b).to_color(ColorDepth::TrueColor)));
        assert_ne!(whole[(12, 10)].symbol(), " ");
        assert_eq!(whole[(0, 11)].symbol(), " ");
    }

    #[test]
    fn the_head_fills_its_box() {
        // Inside the outline, every pixel has a colour: no gaps between facets.
        for y in 0..100 {
            for x in 0..100 {
                let (x, y) = (x as f32 + 0.5, y as f32 + 0.5);
                let within = (x - 50.).abs() < 4. && (55. ..85.).contains(&y);
                if within {
                    assert!(colour_at(x, y).is_some(), "{x},{y}");
                }
            }
        }
    }
}

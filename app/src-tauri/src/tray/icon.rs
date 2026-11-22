//! The tray icon: Siler's, inside a progress ring while downloads run, with a dot for a
//! recording or for something that needs the user.

use super::model::Model;

const SIZES: [usize; 4] = [22, 32, 48, 64];
const RECORDING: [u8; 3] = [0xef, 0x44, 0x44];
const NEEDS_YOU: [u8; 3] = [0xf5, 0x9e, 0x0b];
const TRACK: [u8; 3] = [0x80, 0x86, 0x96];
/// The logo's share of the icon inside a ring.
const INSIDE: f32 = 0.70;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ring {
    None,
    /// Percent, in steps of two.
    At(u8),
    /// A size that isn't known: a short arc that stays put.
    Arc,
    /// Only recordings: they have no end to fill up to.
    Recording,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dot {
    None,
    Recording,
    NeedsYou,
}

/// All that decides how the icon looks, so it is drawn again only when that changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub ring: Ring,
    pub dot: Dot,
    pub accent: [u8; 3],
}

impl Look {
    pub fn of(m: &Model, accent: [u8; 3]) -> Look {
        let recording = m.recording();
        let ring = match m.percent {
            _ if m.lines.is_empty() => Ring::None,
            Some(p) => Ring::At(p / 2 * 2),
            None if recording => Ring::Recording,
            None => Ring::Arc,
        };
        let dot = if m.needs_you > 0 {
            Dot::NeedsYou
        } else if recording {
            Dot::Recording
        } else {
            Dot::None
        };
        Look { ring, dot, accent }
    }
}

/// The app's icon as straight-alpha RGBA.
pub struct Logo {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

impl Logo {
    pub fn load() -> Option<Logo> {
        let img =
            tauri::image::Image::from_bytes(include_bytes!("../../icons/128x128.png")).ok()?;
        Some(Logo {
            width: img.width() as usize,
            height: img.height() as usize,
            rgba: img.rgba().to_vec(),
        })
    }

    /// The logo scaled to `side` pixels: the average of the pixels under each one.
    fn scaled(&self, side: usize) -> Vec<[f32; 4]> {
        let mut out = Vec::with_capacity(side * side);
        for y in 0..side {
            let ys = y * self.height / side
                ..((y + 1) * self.height / side).max(y * self.height / side + 1);
            for x in 0..side {
                let xs = x * self.width / side
                    ..((x + 1) * self.width / side).max(x * self.width / side + 1);
                let mut sum = [0f32; 4];
                let mut n = 0f32;
                for sy in ys.clone() {
                    for sx in xs.clone() {
                        let p = &self.rgba[(sy * self.width + sx) * 4..][..4];
                        let a = f32::from(p[3]) / 255.0;
                        for c in 0..3 {
                            sum[c] += f32::from(p[c]) * a;
                        }
                        sum[3] += a;
                        n += 1.0;
                    }
                }
                out.push(sum.map(|v| v / n));
            }
        }
        out
    }
}

/// Paints premultiplied `src` over `dst`.
fn over(dst: &mut [f32; 4], src: [f32; 4]) {
    for c in 0..4 {
        dst[c] = src[c] + dst[c] * (1.0 - src[3]);
    }
}

fn paint(colour: [u8; 3], alpha: f32) -> [f32; 4] {
    let a = alpha.clamp(0.0, 1.0);
    [
        f32::from(colour[0]) * a,
        f32::from(colour[1]) * a,
        f32::from(colour[2]) * a,
        a,
    ]
}

/// How much of a pixel `distance` inside an edge (negative: outside) is covered.
fn cover(distance: f32) -> f32 {
    (distance + 0.5).clamp(0.0, 1.0)
}

/// One size of the icon, as ARGB32 in network byte order.
fn draw(logo: &Logo, size: usize, look: &Look) -> Vec<u8> {
    let mut px = vec![[0f32; 4]; size * size];
    let s = size as f32;
    let side = match look.ring {
        Ring::None => size,
        _ => (s * INSIDE).round() as usize,
    };
    let at = (size - side) / 2;
    for (i, p) in logo.scaled(side).into_iter().enumerate() {
        over(&mut px[(at + i / side) * size + at + i % side], p);
    }

    let centre = s / 2.0;
    let outer = centre - 0.5;
    let inner = outer - (s * 0.11).max(2.5);
    let (colour, fraction) = match look.ring {
        Ring::None => (look.accent, 0.0),
        Ring::At(p) => (look.accent, f32::from(p) / 100.0),
        Ring::Arc => (look.accent, 0.25),
        Ring::Recording => (RECORDING, 1.0),
    };
    let dot = match look.dot {
        Dot::None => None,
        Dot::Recording => Some(RECORDING),
        Dot::NeedsYou => Some(NEEDS_YOU),
    };
    let dot_r = s * 0.19;
    let dot_at = s - dot_r - 0.5;
    for y in 0..size {
        for x in 0..size {
            let p = &mut px[y * size + x];
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            if look.ring != Ring::None {
                let (dx, dy) = (fx - centre, fy - centre);
                let d = dx.hypot(dy);
                let band = cover(outer - d).min(cover(d - inner));
                if band > 0.0 {
                    over(p, paint(TRACK, band * 0.45));
                    // Clockwise from twelve o'clock.
                    let turn =
                        dx.atan2(-dy).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
                    if turn < fraction {
                        over(p, paint(colour, band));
                    }
                }
            }
            if let Some(colour) = dot {
                let d = (fx - dot_at).hypot(fy - dot_at);
                over(p, paint([0x15, 0x17, 0x1a], cover(dot_r - d)));
                over(p, paint(colour, cover(dot_r * 0.72 - d)));
            }
        }
    }

    px.into_iter()
        .flat_map(|[r, g, b, a]| {
            let straight = |c: f32| {
                if a > 0.0 {
                    (c / a).round().min(255.0) as u8
                } else {
                    0
                }
            };
            [
                (a * 255.0).round() as u8,
                straight(r),
                straight(g),
                straight(b),
            ]
        })
        .collect()
}

pub fn render(logo: &Logo, look: &Look) -> Vec<ksni::Icon> {
    SIZES
        .iter()
        .map(|&size| ksni::Icon {
            width: size as i32,
            height: size as i32,
            data: draw(logo, size, look),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCENT: [u8; 3] = [0x8b, 0x7b, 0xff];

    fn pixel(data: &[u8], size: usize, x: usize, y: usize) -> [u8; 4] {
        data[(y * size + x) * 4..][..4].try_into().unwrap()
    }

    #[test]
    fn the_ring_fills_clockwise() {
        let logo = Logo::load().unwrap();
        let look = Look {
            ring: Ring::At(50),
            dot: Dot::None,
            accent: ACCENT,
        };
        let d = draw(&logo, 64, &look);
        let [a, r, g, b] = pixel(&d, 64, 60, 32);
        assert!(a > 240 && [r, g, b] == ACCENT, "the right half is done");
        let [a, r, g, b] = pixel(&d, 64, 3, 32);
        assert!(a < 140 && [r, g, b] != ACCENT, "the left half is track");
    }

    #[test]
    fn dots_and_every_size() {
        let logo = Logo::load().unwrap();
        let look = Look {
            ring: Ring::None,
            dot: Dot::NeedsYou,
            accent: ACCENT,
        };
        for icon in render(&logo, &look) {
            assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
        }
        let d = draw(&logo, 64, &look);
        let [_, r, g, b] = pixel(&d, 64, 51, 51);
        assert_eq!([r, g, b], NEEDS_YOU);
    }

    #[test]
    fn redraws_only_on_visible_steps() {
        let line = super::super::model::Line {
            id: 1,
            name: "a".into(),
            speed: 1,
            doing: super::super::model::Doing::Transfer(Some(40), None),
            can_finish: false,
        };
        let at = |p| {
            let m = Model {
                lines: vec![line.clone()],
                percent: Some(p),
                ..Default::default()
            };
            Look::of(&m, ACCENT)
        };
        assert_eq!(at(40), at(41));
        assert_ne!(at(40), at(42));
    }
}

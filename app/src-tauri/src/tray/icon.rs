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

/// How the progress windows' icon looks: a ring around a down arrow, over one bar for each
/// window in the tray (up to three).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StackLook {
    pub ring: Ring,
    pub bars: u8,
    pub accent: [u8; 3],
}

impl StackLook {
    pub fn of(ring: Ring, windows: usize, accent: [u8; 3]) -> StackLook {
        StackLook {
            ring,
            bars: windows.clamp(1, 3) as u8,
            accent,
        }
    }
}

const ARROW: [u8; 3] = [0x22, 0xc5, 0x5e];

/// Whether a point of the unit square is on the arrow, or on one of the bars under it.
fn on_glyph(x: f32, y: f32, bars: u8) -> (bool, bool) {
    let shaft = (0.445..0.555).contains(&x) && (0.20..0.42).contains(&y);
    // A triangle pointing down: (0.29, 0.40) (0.71, 0.40) (0.5, 0.60).
    let head = (0.40..0.60).contains(&y) && (x - 0.5).abs() <= 0.21 * (0.60 - y) / 0.20;
    let bar = (0..bars).any(|i| {
        let top = 0.64 + f32::from(i) * 0.075;
        (0.33..0.67).contains(&x) && (top..top + 0.045).contains(&y)
    });
    (shaft || head, bar)
}

fn draw_stack(size: usize, look: &StackLook) -> Vec<u8> {
    let mut px = vec![[0f32; 4]; size * size];
    let s = size as f32;
    // Four by four samples a pixel for the glyph's edges.
    const N: usize = 4;
    for y in 0..size {
        for x in 0..size {
            let (mut arrow, mut bar) = (0f32, 0f32);
            for sy in 0..N {
                for sx in 0..N {
                    let fx = (x as f32 + (sx as f32 + 0.5) / N as f32) / s;
                    let fy = (y as f32 + (sy as f32 + 0.5) / N as f32) / s;
                    let (a, b) = on_glyph(fx, fy, look.bars);
                    arrow += f32::from(u8::from(a));
                    bar += f32::from(u8::from(b));
                }
            }
            let n = (N * N) as f32;
            let p = &mut px[y * size + x];
            over(p, paint(look.accent, bar / n));
            over(p, paint(ARROW, arrow / n));
        }
    }
    ring(&mut px, size, look.ring, look.accent);
    argb(px)
}

/// The progress ring around the edge of an icon.
fn ring(px: &mut [[f32; 4]], size: usize, ring: Ring, accent: [u8; 3]) {
    let s = size as f32;
    let centre = s / 2.0;
    let outer = centre - 0.5;
    let inner = outer - (s * 0.11).max(2.5);
    let (colour, fraction) = match ring {
        Ring::None => return,
        Ring::At(p) => (accent, f32::from(p) / 100.0),
        Ring::Arc => (accent, 0.25),
        Ring::Recording => (RECORDING, 1.0),
    };
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 + 0.5 - centre, y as f32 + 0.5 - centre);
            let d = dx.hypot(dy);
            let band = cover(outer - d).min(cover(d - inner));
            if band > 0.0 {
                let p = &mut px[y * size + x];
                over(p, paint(TRACK, band * 0.45));
                let turn = dx.atan2(-dy).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
                if turn < fraction {
                    over(p, paint(colour, band));
                }
            }
        }
    }
}

/// Premultiplied pixels as ARGB32 in network byte order.
fn argb(px: Vec<[f32; 4]>) -> Vec<u8> {
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

pub fn render_stack(look: &StackLook) -> Vec<ksni::Icon> {
    SIZES
        .iter()
        .map(|&size| ksni::Icon {
            width: size as i32,
            height: size as i32,
            data: draw_stack(size, look),
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
    fn the_stack_shows_an_arrow_and_a_bar_per_window() {
        let look = StackLook::of(Ring::At(50), 2, ACCENT);
        for icon in render_stack(&look) {
            assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
        }
        let d = draw_stack(64, &look);
        let [a, r, g, b] = pixel(&d, 64, 32, 20);
        assert!(a > 240 && [r, g, b] == ARROW, "the arrow's shaft");
        let bar = |y| pixel(&d, 64, 32, y);
        assert_eq!(bar(42)[1..], ACCENT, "first bar");
        assert_eq!(bar(47)[1..], ACCENT, "second bar");
        assert_eq!(bar(52)[0], 0, "no third bar");
        let [a, r, g, b] = pixel(&d, 64, 60, 32);
        assert!(a > 240 && [r, g, b] == ACCENT, "the ring's done half");
        assert_eq!(StackLook::of(Ring::Arc, 9, ACCENT).bars, 3);
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

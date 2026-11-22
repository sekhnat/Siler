//! Where the panel goes: which screen and edge the tray icon is at, and how far along it.

/// Space between the panel and the screen's edges.
pub const GAP: i32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    fn contains(&self, (x, y): (f64, f64)) -> bool {
        x >= f64::from(self.x)
            && y >= f64::from(self.y)
            && x <= f64::from(self.x + self.w)
            && y <= f64::from(self.y + self.h)
    }

    /// To its nearest edge; level distances prefer the bottom, then the top.
    fn nearest(&self, (x, y): (f64, f64)) -> (Edge, f64) {
        let all = [
            (Edge::Bottom, f64::from(self.y + self.h) - y),
            (Edge::Top, y - f64::from(self.y)),
            (Edge::Right, f64::from(self.x + self.w) - x),
            (Edge::Left, x - f64::from(self.x)),
        ];
        all.into_iter()
            .fold(all[0], |best, e| if e.1 < best.1 { e } else { best })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    /// In logical pixels.
    pub area: Rect,
    pub scale: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Bottom,
    Top,
    Left,
    Right,
}

/// The tray icon, found from the click.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub screen: usize,
    pub edge: Edge,
    pub at: (f64, f64),
}

/// Reads a click's coordinates. Plasma sends them global, in logical pixels or in device
/// pixels (each screen scaled from its own corner, or the whole desktop from 0,0); other
/// hosts send either, or 0,0 for none. A tray is at a screen's edge, so of the readings
/// that land on a screen, the one nearest an edge is taken.
pub fn locate(screens: &[Screen], click: (i32, i32)) -> Option<Anchor> {
    let first = screens.first()?;
    let p = (f64::from(click.0), f64::from(click.1));
    let mut best: Option<(Anchor, f64)> = None;
    if click != (0, 0) {
        for (i, s) in screens.iter().enumerate() {
            let (ox, oy) = (f64::from(s.area.x), f64::from(s.area.y));
            let readings = [
                p,
                (ox + (p.0 - ox) / s.scale, oy + (p.1 - oy) / s.scale),
                (p.0 / s.scale, p.1 / s.scale),
            ];
            for at in readings.into_iter().filter(|at| s.area.contains(*at)) {
                let (edge, d) = s.area.nearest(at);
                if best.is_none_or(|(_, b)| d < b) {
                    best = Some((
                        Anchor {
                            screen: i,
                            edge,
                            at,
                        },
                        d,
                    ));
                }
            }
        }
    }
    // No usable position: the usual corner of a tray.
    Some(best.map(|(a, _)| a).unwrap_or(Anchor {
        screen: 0,
        edge: Edge::Bottom,
        at: (
            f64::from(first.area.x + first.area.w),
            f64::from(first.area.y + first.area.h),
        ),
    }))
}

/// How far along its edge a panel of `size` starts, centred on the icon and kept on `area`.
pub fn along(area: &Rect, anchor: &Anchor, (w, h): (i32, i32)) -> i32 {
    let (at, start, len, size) = match anchor.edge {
        Edge::Bottom | Edge::Top => (anchor.at.0, area.x, area.w, w),
        Edge::Left | Edge::Right => (anchor.at.1, area.y, area.h, h),
    };
    let wanted = at.round() as i32 - start - size / 2;
    wanted.min(len - size - GAP).max(GAP)
}

/// The panel's corner, for X11, inside `area` (the screen without its panels).
pub fn corner(area: &Rect, anchor: &Anchor, size: (i32, i32)) -> (i32, i32) {
    let along = along(area, anchor, size);
    match anchor.edge {
        Edge::Bottom => (area.x + along, area.y + area.h - size.1 - GAP),
        Edge::Top => (area.x + along, area.y + GAP),
        Edge::Left => (area.x + GAP, area.y + along),
        Edge::Right => (area.x + area.w - size.0 - GAP, area.y + along),
    }
}

/// Each screen's corner and scale from KWin's `supportInformation`, since GDK rounds a
/// fractional scale up.
pub fn kwin_scales(info: &str) -> Vec<((i32, i32), f64)> {
    let mut out = Vec::new();
    let mut corner = None;
    for line in info.lines() {
        let line = line.trim();
        if let Some(g) = line.strip_prefix("Geometry:") {
            let mut n = g.trim().split(',');
            corner = match (n.next(), n.next()) {
                (Some(x), Some(y)) => x.trim().parse().ok().zip(y.trim().parse().ok()),
                _ => None,
            };
        } else if let Some(s) = line.strip_prefix("Scale:")
            && let Some(c) = corner.take()
            && let Ok(scale) = s.trim().parse::<f64>()
            && (0.5..=8.0).contains(&scale)
        {
            out.push((c, scale));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(x: i32, w: i32, h: i32, scale: f64) -> Screen {
        Screen {
            area: Rect { x, y: 0, w, h },
            scale,
        }
    }

    #[test]
    fn all_four_edges() {
        let s = [screen(0, 1920, 1080, 1.0)];
        for (click, edge) in [
            ((1700, 1060), Edge::Bottom),
            ((1700, 18), Edge::Top),
            ((20, 700), Edge::Left),
            ((1900, 700), Edge::Right),
        ] {
            assert_eq!(locate(&s, click).unwrap().edge, edge, "{click:?}");
        }
    }

    #[test]
    fn device_pixels_on_a_scaled_screen() {
        for scale in [1.25, 1.5, 2.0] {
            let s = [screen(0, 1600, 900, scale)];
            for logical in [(1500.0, 880.0), (1500.0, 16.0), (30.0, 20.0), (20.0, 800.0)] {
                let click = ((logical.0 * scale) as i32, (logical.1 * scale) as i32);
                let a = locate(&s, click).unwrap();
                assert!(
                    (a.at.0 - logical.0).abs() < 2.0 && (a.at.1 - logical.1).abs() < 2.0,
                    "{scale}: {click:?} read as {:?}",
                    a.at
                );
            }
        }
        // The same screen when the host sends logical pixels.
        let a = locate(&[screen(0, 1600, 900, 1.5)], (1500, 880)).unwrap();
        assert_eq!(a.at, (1500.0, 880.0));
    }

    #[test]
    fn the_second_screen() {
        let s = [screen(0, 1920, 1080, 1.0), screen(1920, 1280, 800, 2.0)];
        // Plasma: the screen's corner, plus the offset in device pixels.
        let a = locate(&s, (1920 + 2400, 1564)).unwrap();
        assert_eq!((a.screen, a.edge, a.at), (1, Edge::Bottom, (3120.0, 782.0)));
    }

    #[test]
    fn stays_on_screen() {
        let s = screen(0, 1920, 1080, 1.0);
        let a = locate(&[s], (1900, 1075)).unwrap();
        assert_eq!(along(&s.area, &a, (380, 300)), 1920 - 380 - GAP);
        let left = locate(&[s], (300, 1075)).unwrap();
        assert_eq!(along(&s.area, &left, (380, 300)), 110);
        let work = Rect { h: 1036, ..s.area };
        assert_eq!(corner(&work, &a, (380, 300)).1, 1036 - 300 - GAP);
    }

    #[test]
    fn without_coordinates() {
        let a = locate(&[screen(0, 800, 600, 1.0)], (0, 0)).unwrap();
        assert_eq!((a.edge, a.at), (Edge::Bottom, (800.0, 600.0)));
        assert_eq!(locate(&[], (5, 5)), None);
    }

    #[test]
    fn reads_kwins_scales() {
        let info = "Screens\n=======\nScreen 0:\nName: DP-1\nGeometry: 0,0,1707x960\nScale: 1.5\nScreen 1:\nGeometry: 1707,0,1280x800\nScale: 2\n";
        assert_eq!(kwin_scales(info), [((0, 0), 1.5), ((1707, 0), 2.0)]);
        assert!(kwin_scales("nothing here").is_empty());
    }
}

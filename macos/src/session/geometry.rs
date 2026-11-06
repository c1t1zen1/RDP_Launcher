//! Mapping between session-view points and remote framebuffer pixels, and
//! scroll-wheel conversion.

use crate::session::framebuffer::Region;

/// A session view of `view_w`×`view_h` points showing framebuffer region `src`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewMapping {
    pub view_w: f64,
    pub view_h: f64,
    pub src: Region,
}

fn span(lo: u16, hi: u16) -> f64 {
    f64::from(hi.saturating_sub(lo)) + 1.0
}

/// A point in the view (origin top-left) → remote framebuffer pixel, clamped to `src`.
pub fn view_to_remote(m: &ViewMapping, x: f64, y_from_top: f64) -> (u16, u16) {
    let map = |v: f64, view: f64, lo: u16, hi: u16| -> u16 {
        let scaled = if view > 0.0 { v / view * span(lo, hi) } else { 0.0 };
        let px = f64::from(lo) + scaled.floor();
        px.clamp(f64::from(lo), f64::from(hi)) as u16
    };
    (
        map(x, m.view_w, m.src.left, m.src.right),
        map(y_from_top, m.view_h, m.src.top, m.src.bottom),
    )
}

/// `src` in unit coordinates of the whole framebuffer, origin top-left: (x, y, w, h).
pub fn contents_rect_top_left(m: &ViewMapping, fb_w: u16, fb_h: u16) -> (f64, f64, f64, f64) {
    let (w, h) = (f64::from(fb_w.max(1)), f64::from(fb_h.max(1)));
    (
        f64::from(m.src.left) / w,
        f64::from(m.src.top) / h,
        span(m.src.left, m.src.right) / w,
        span(m.src.top, m.src.bottom) / h,
    )
}

/// Converts AppKit scroll deltas to RDP wheel rotation units (120 per notch).
#[derive(Debug, Default)]
pub struct ScrollAccumulator {
    acc: f64,
}

impl ScrollAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// `precise` = trackpad / Magic Mouse (`hasPreciseScrollingDeltas`).
    pub fn push(&mut self, delta: f64, precise: bool) -> i16 {
        let units = if precise {
            self.acc += delta * 10.0;
            let whole = self.acc.trunc();
            self.acc -= whole;
            whole
        } else {
            (delta * 120.0).round()
        };
        units.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::framebuffer::Region;

    fn full(w: u16, h: u16) -> Region {
        Region { left: 0, top: 0, right: w - 1, bottom: h - 1 }
    }

    #[test]
    fn view_to_remote_scales() {
        let m = ViewMapping { view_w: 640., view_h: 400., src: full(1280, 800) };
        assert_eq!(view_to_remote(&m, 320., 200.), (640, 400));
    }

    #[test]
    fn view_to_remote_offsets_into_monitor_region() {
        let m = ViewMapping {
            view_w: 1920.,
            view_h: 1200.,
            src: Region { left: 1920, top: 0, right: 3839, bottom: 1199 },
        };
        assert_eq!(view_to_remote(&m, 0., 0.), (1920, 0));
        assert_eq!(view_to_remote(&m, 960., 600.), (2880, 600));
    }

    #[test]
    fn view_to_remote_clamps() {
        let m = ViewMapping { view_w: 100., view_h: 100., src: full(100, 100) };
        assert_eq!(view_to_remote(&m, -5., 9999.), (0, 99));
    }

    #[test]
    fn contents_rect_for_right_half() {
        let m = ViewMapping {
            view_w: 1920.,
            view_h: 1200.,
            src: Region { left: 1920, top: 0, right: 3839, bottom: 1199 },
        };
        assert_eq!(contents_rect_top_left(&m, 3840, 1200), (0.5, 0.0, 0.5, 1.0));
    }

    #[test]
    fn wheel_notch_is_120_units() {
        assert_eq!(ScrollAccumulator::new().push(1.0, false), 120);
        assert_eq!(ScrollAccumulator::new().push(-2.0, false), -240);
    }

    #[test]
    fn precise_deltas_accumulate_fractional() {
        let mut s = ScrollAccumulator::new();
        assert_eq!(s.push(0.05, true), 0);
        assert_eq!(s.push(0.05, true), 1);
    }

    #[test]
    fn huge_delta_is_clamped() {
        assert_eq!(ScrollAccumulator::new().push(1.0e9, false), i16::MAX);
    }
}

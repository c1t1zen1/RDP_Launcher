//! Launcher layout (same numbers as the Windows app), hit-testing and the
//! drag-reorder spring. Coordinates are flipped: origin top-left, y down.

pub const BTN_W: f64 = 300.0;
pub const BTN_H: f64 = 56.0;
pub const MARGIN: f64 = 12.0;
pub const GAP: f64 = 8.0;
pub const HEADER_H: f64 = 32.0;
pub const HDR_BTN: f64 = 24.0;
pub const HDR_PAD: f64 = 6.0;
pub const STRIP_H: f64 = 44.0;
pub const PLUS_SIZE: f64 = 30.0;
pub const HANDLE_W: f64 = 34.0;
pub const SPRING_STIFFNESS: f64 = 0.30;
pub const SPRING_DAMPING: f64 = 0.70;
/// Fraction of the screen's visible height the launcher may grow to.
pub const MAX_HEIGHT_FRACTION: f64 = 0.85;
pub const CONTENT_W: f64 = BTN_W + 2.0 * MARGIN;

const SLOT: f64 = BTN_H + GAP;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct R {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl R {
    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Body,
    Edit,
    Duplicate,
    Delete,
    Grip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Item(usize, Zone),
    None,
}

fn slot_y(i: usize, scroll: f64) -> f64 {
    HEADER_H + MARGIN + i as f64 * SLOT - scroll
}

pub fn item_rect(i: usize, scroll: f64, width: f64) -> R {
    R { x: MARGIN, y: slot_y(i, scroll), w: width - 2.0 * MARGIN, h: BTN_H }
}

/// Edit, duplicate, delete — bottom-right, just left of the grip.
pub fn icon_rects(item: &R) -> [R; 3] {
    let right_edge = item.right() - HANDLE_W - 2.0;
    std::array::from_fn(|k| R { x: right_edge - (3 - k) as f64 * 20.0, y: item.bottom() - 21.0, w: 18.0, h: 18.0 })
}

pub fn grip_rect(item: &R) -> R {
    R { x: item.right() - HANDLE_W, y: item.y, w: HANDLE_W, h: item.h }
}

pub fn hit_test(px: f64, py: f64, n: usize, scroll: f64, width: f64) -> Hit {
    for i in 0..n {
        let item = item_rect(i, scroll, width);
        if !item.contains(px, py) {
            continue;
        }
        if grip_rect(&item).contains(px, py) {
            return Hit::Item(i, Zone::Grip);
        }
        let zones = [Zone::Edit, Zone::Duplicate, Zone::Delete];
        for (rect, zone) in icon_rects(&item).iter().zip(zones) {
            if rect.contains(px, py) {
                return Hit::Item(i, zone);
            }
        }
        return Hit::Item(i, Zone::Body);
    }
    Hit::None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderButton {
    Minimize,
    Pin,
    Info,
}

/// Header buttons, right-aligned: minimize, then pin, then info (as on Windows).
pub fn header_button_rect(button: HeaderButton, width: f64) -> R {
    let k = match button {
        HeaderButton::Minimize => 0.0,
        HeaderButton::Pin => 1.0,
        HeaderButton::Info => 2.0,
    };
    R {
        x: width - HDR_PAD - HDR_BTN - k * (HDR_BTN + 2.0),
        y: (HEADER_H - HDR_BTN) / 2.0,
        w: HDR_BTN,
        h: HDR_BTN,
    }
}

/// The + button in the bottom strip of a `width`×`height` content area.
pub fn plus_rect(width: f64, height: f64) -> R {
    R { x: width - PLUS_SIZE - 10.0, y: height - STRIP_H + (STRIP_H - PLUS_SIZE) / 2.0, w: PLUS_SIZE, h: PLUS_SIZE }
}

/// Height of the launcher's content for `n` connections (an empty list keeps one row).
pub fn content_height(n: usize) -> f64 {
    HEADER_H + MARGIN + n.max(1) as f64 * SLOT + STRIP_H
}

/// The slot a dragged item (whose top edge is at `drag_top_y`) would drop into.
pub fn drop_index(drag_top_y: f64, n: usize, scroll: f64) -> usize {
    if n == 0 {
        return 0;
    }
    let rel = (drag_top_y + scroll - HEADER_H - MARGIN) / SLOT;
    rel.round().clamp(0.0, (n - 1) as f64) as usize
}

/// Damped spring used to glide buttons into place (same constants as Windows).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub pos: f64,
    pub vel: f64,
}

impl Spring {
    /// Advance one tick toward `target`; returns true once settled (then snapped).
    pub fn step(&mut self, target: f64) -> bool {
        self.vel = (self.vel + (target - self.pos) * SPRING_STIFFNESS) * SPRING_DAMPING;
        self.pos += self.vel;
        if (target - self.pos).abs() <= 0.5 && self.vel.abs() <= 0.5 {
            self.pos = target;
            self.vel = 0.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f64 = 324.0; // BTN_W + 2 * MARGIN

    #[test]
    fn item_rect_first_item() {
        let r = item_rect(0, 0.0, W);
        assert_eq!((r.x, r.y, r.w, r.h), (12.0, 44.0, 300.0, 56.0));
    }

    #[test]
    fn item_rect_accounts_for_scroll() {
        assert_eq!(item_rect(2, 10.0, W).y, 44.0 + 2.0 * 64.0 - 10.0);
    }

    #[test]
    fn icons_sit_left_of_grip() {
        let r = item_rect(0, 0.0, W);
        let ic = icon_rects(&r);
        assert_eq!(ic[2].x, 12.0 + 300.0 - 34.0 - 2.0 - 20.0);
        assert_eq!(ic[0].x, 12.0 + 300.0 - 34.0 - 2.0 - 60.0);
        assert_eq!(ic[0].y, 44.0 + 56.0 - 21.0);
        assert_eq!((ic[1].w, ic[1].h), (18.0, 18.0));
    }

    #[test]
    fn grip_is_rightmost_handle_width() {
        let g = grip_rect(&item_rect(0, 0.0, W));
        assert_eq!((g.x, g.w), (12.0 + 300.0 - 34.0, 34.0));
    }

    #[test]
    fn hit_test_zones() {
        let r = item_rect(0, 0.0, W);
        let g = grip_rect(&r);
        let ic = icon_rects(&r);
        assert_eq!(hit_test(g.x + 5.0, g.y + 5.0, 3, 0.0, W), Hit::Item(0, Zone::Grip));
        assert_eq!(hit_test(ic[1].x + 1.0, ic[1].y + 1.0, 3, 0.0, W), Hit::Item(0, Zone::Duplicate));
        assert_eq!(hit_test(ic[0].x + 1.0, ic[0].y + 1.0, 3, 0.0, W), Hit::Item(0, Zone::Edit));
        assert_eq!(hit_test(ic[2].x + 1.0, ic[2].y + 1.0, 3, 0.0, W), Hit::Item(0, Zone::Delete));
        assert_eq!(hit_test(20.0, 50.0, 3, 0.0, W), Hit::Item(0, Zone::Body));
        assert_eq!(hit_test(20.0, 44.0 + 56.0 + 4.0, 3, 0.0, W), Hit::None); // in the gap
        assert_eq!(hit_test(20.0, 44.0 + 5.0 * 64.0, 3, 0.0, W), Hit::None); // past the last item
    }

    #[test]
    fn header_buttons_right_aligned_minimize_first() {
        let min = header_button_rect(HeaderButton::Minimize, W);
        let pin = header_button_rect(HeaderButton::Pin, W);
        let info = header_button_rect(HeaderButton::Info, W);
        assert_eq!((min.x, min.y, min.w), (W - 6.0 - 24.0, 4.0, 24.0));
        assert_eq!(pin.x, min.x - 26.0);
        assert_eq!(info.x, pin.x - 26.0);
    }

    #[test]
    fn plus_button_bottom_right() {
        let p = plus_rect(W, 300.0);
        assert_eq!((p.x, p.y, p.w, p.h), (W - 30.0 - 10.0, 300.0 - 44.0 + 7.0, 30.0, 30.0));
    }

    #[test]
    fn content_height_empty_list_reserves_one_row() {
        assert_eq!(content_height(0), 32.0 + 12.0 + 64.0 + 44.0);
        assert_eq!(content_height(4), 32.0 + 12.0 + 4.0 * 64.0 + 44.0);
    }

    #[test]
    fn drop_index_rounds_to_nearest_slot() {
        assert_eq!(drop_index(item_rect(2, 0.0, W).y, 4, 0.0), 2);
        assert_eq!(drop_index(item_rect(2, 0.0, W).y + 20.0, 4, 0.0), 2);
        assert_eq!(drop_index(item_rect(2, 0.0, W).y + 40.0, 4, 0.0), 3);
        assert_eq!(drop_index(5000.0, 4, 0.0), 3);
        assert_eq!(drop_index(-500.0, 4, 0.0), 0);
    }

    #[test]
    fn spring_settles_on_target() {
        let mut s = Spring { pos: 0.0, vel: 0.0 };
        let mut n = 0;
        while !s.step(100.0) {
            n += 1;
            assert!(n < 200, "spring never settled");
        }
        assert_eq!((s.pos, s.vel), (100.0, 0.0));
    }

    #[test]
    fn spring_overshoots_slightly() {
        let mut s = Spring { pos: 0.0, vel: 0.0 };
        let mut max: f64 = 0.0;
        while !s.step(100.0) {
            max = max.max(s.pos);
        }
        // Same constants and update rule as Windows (0.30 / 0.70): ~31% bounce, never wild.
        assert!(max > 100.0 && max < 140.0, "max {max}");
    }
}

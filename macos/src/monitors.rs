//! Turning the user's chosen Mac displays into the monitor layout sent to the RDP
//! server, and locating each monitor inside the remote framebuffer.

use crate::session::framebuffer::Region;

/// AppKit global coordinates (origin bottom-left of the main display), in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayInfo {
    /// Stable identity used in `Connection::monitors`.
    pub key: String,
    pub name: String,
    pub frame: Rect,
    pub visible_frame: Rect,
    pub is_main: bool,
}

/// One remote monitor; coordinates are relative to the primary's top-left, y down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteMonitor {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

impl RemoteMonitor {
    fn right(&self) -> i32 {
        self.left + self.width as i32
    }

    fn bottom(&self) -> i32 {
        self.top + self.height as i32
    }
}

const MIN_SIZE: u32 = 200;
const MAX_SIZE: u32 = 8192;

/// RDP monitor widths must be even and within 200..=8192.
pub fn even_size(w: u32) -> u32 {
    w.clamp(MIN_SIZE, MAX_SIZE) & !1
}

fn height_size(h: u32) -> u32 {
    h.clamp(MIN_SIZE, MAX_SIZE)
}

/// Saved display keys → connected displays, in saved order; falls back to the main display.
pub fn resolve_selection(saved: &[String], available: &[DisplayInfo]) -> Vec<DisplayInfo> {
    let chosen: Vec<DisplayInfo> = saved
        .iter()
        .filter_map(|key| available.iter().find(|d| &d.key == key).cloned())
        .collect();
    if !chosen.is_empty() {
        return chosen;
    }
    available
        .iter()
        .find(|d| d.is_main)
        .or_else(|| available.first())
        .cloned()
        .into_iter()
        .collect()
}

/// `displays[0]` becomes the primary monitor at (0,0); others keep their physical
/// arrangement. If any monitor would not touch another, they are packed side by side.
pub fn remote_layout(displays: &[DisplayInfo]) -> Vec<RemoteMonitor> {
    let Some(p) = displays.first().map(|d| d.frame) else {
        return Vec::new();
    };
    let arranged: Vec<RemoteMonitor> = displays
        .iter()
        .enumerate()
        .map(|(i, d)| RemoteMonitor {
            left: (d.frame.x - p.x).round() as i32,
            top: ((p.y + p.h) - (d.frame.y + d.frame.h)).round() as i32,
            width: even_size(d.frame.w.round() as u32),
            height: height_size(d.frame.h.round() as u32),
            primary: i == 0,
        })
        .collect();
    if all_adjacent(&arranged) {
        arranged
    } else {
        pack_side_by_side(displays, arranged)
    }
}

fn touches(a: &RemoteMonitor, b: &RemoteMonitor) -> bool {
    let overlaps = |a0: i32, a1: i32, b0: i32, b1: i32| a0 < b1 && b0 < a1;
    let near = |x: i32, y: i32| (x - y).abs() <= 1;
    ((near(a.right(), b.left) || near(b.right(), a.left)) && overlaps(a.top, a.bottom(), b.top, b.bottom()))
        || ((near(a.bottom(), b.top) || near(b.bottom(), a.top)) && overlaps(a.left, a.right(), b.left, b.right()))
}

fn all_adjacent(layout: &[RemoteMonitor]) -> bool {
    layout.len() < 2
        || layout
            .iter()
            .enumerate()
            .all(|(i, a)| layout.iter().enumerate().any(|(j, b)| i != j && touches(a, b)))
}

fn pack_side_by_side(displays: &[DisplayInfo], mut layout: Vec<RemoteMonitor>) -> Vec<RemoteMonitor> {
    let mut order: Vec<usize> = (0..layout.len()).collect();
    order.sort_by(|&a, &b| displays[a].frame.x.total_cmp(&displays[b].frame.x));
    let mut x = 0;
    for &i in &order {
        layout[i].left = x;
        layout[i].top = 0;
        x += layout[i].width as i32;
    }
    let shift = layout[0].left;
    for m in &mut layout {
        m.left -= shift;
    }
    layout
}

/// Bounding box of all monitors: (left, top, width, height).
pub fn virtual_desktop(layout: &[RemoteMonitor]) -> (i32, i32, u32, u32) {
    let left = layout.iter().map(|m| m.left).min().unwrap_or(0);
    let top = layout.iter().map(|m| m.top).min().unwrap_or(0);
    let right = layout.iter().map(RemoteMonitor::right).max().unwrap_or(0);
    let bottom = layout.iter().map(RemoteMonitor::bottom).max().unwrap_or(0);
    (left, top, (right - left) as u32, (bottom - top) as u32)
}

/// Where monitor `index` sits in the framebuffer (origin = bounding box top-left).
pub fn monitor_region(layout: &[RemoteMonitor], index: usize) -> Region {
    let (left, top, _, _) = virtual_desktop(layout);
    let m = layout[index];
    let x = (m.left - left) as u16;
    let y = (m.top - top) as u16;
    Region { left: x, top: y, right: x + m.width as u16 - 1, bottom: y + m.height as u16 - 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(key: &str, x: f64, y: f64, w: f64, h: f64) -> DisplayInfo {
        let frame = Rect { x, y, w, h };
        DisplayInfo { key: key.into(), name: key.into(), frame, visible_frame: frame, is_main: false }
    }

    fn main_display() -> DisplayInfo {
        DisplayInfo { is_main: true, ..ext("builtin", 0., 0., 1512., 982.) }
    }

    fn mon(left: i32, top: i32, width: u32, height: u32, primary: bool) -> RemoteMonitor {
        RemoteMonitor { left, top, width, height, primary }
    }

    #[test]
    fn single_display_is_primary_at_origin() {
        let layout = remote_layout(&[ext("a", 0., 0., 1920., 1200.)]);
        assert_eq!(layout, vec![mon(0, 0, 1920, 1200, true)]);
    }

    #[test]
    fn two_side_by_side_externals() {
        let layout = remote_layout(&[ext("a", 0., 0., 1920., 1200.), ext("b", 1920., 0., 1920., 1200.)]);
        assert_eq!(layout, vec![mon(0, 0, 1920, 1200, true), mon(1920, 0, 1920, 1200, false)]);
        assert_eq!(virtual_desktop(&layout), (0, 0, 3840, 1200));
    }

    #[test]
    fn display_above_primary_gets_negative_top() {
        let layout = remote_layout(&[main_display(), ext("a", 0., 982., 1920., 1200.)]);
        assert_eq!(layout[1].top, -1200);
        assert_eq!(layout[1].left, 0);
    }

    #[test]
    fn display_left_of_primary_gets_negative_left() {
        let layout = remote_layout(&[ext("p", 0., 0., 1920., 1200.), ext("a", -1920., 0., 1920., 1200.)]);
        assert_eq!(layout[1].left, -1920);
        assert_eq!(monitor_region(&layout, 0).left, 1920);
        assert_eq!(monitor_region(&layout, 1).left, 0);
        assert_eq!(monitor_region(&layout, 0).right, 3839);
    }

    #[test]
    fn odd_width_is_rounded_down_to_even() {
        assert_eq!(even_size(1513), 1512);
        assert_eq!(even_size(50), 200);
        assert_eq!(even_size(9001), 8192);
    }

    #[test]
    fn non_adjacent_selection_is_packed_side_by_side() {
        let layout = remote_layout(&[ext("a", 0., 0., 1920., 1200.), ext("c", 3432., 0., 1920., 1200.)]);
        assert_eq!(layout, vec![mon(0, 0, 1920, 1200, true), mon(1920, 0, 1920, 1200, false)]);
    }

    #[test]
    fn packing_keeps_primary_at_origin() {
        // primary is the right-hand display; the other is far to the left with a gap
        let layout = remote_layout(&[ext("p", 0., 0., 1920., 1200.), ext("a", -5000., 0., 1920., 1200.)]);
        assert_eq!(layout[0], mon(0, 0, 1920, 1200, true));
        assert_eq!(layout[1], mon(-1920, 0, 1920, 1200, false));
    }

    #[test]
    fn unplugged_selection_falls_back_to_main() {
        let available = vec![ext("dell-2", 1512., 0., 1920., 1200.), main_display()];
        assert_eq!(resolve_selection(&["dell-1".into()], &available), vec![main_display()]);
        assert_eq!(resolve_selection(&[], &available), vec![main_display()]);
    }

    #[test]
    fn resolve_selection_keeps_saved_order() {
        let (a, b) = (ext("a", 0., 0., 1920., 1200.), ext("b", 1920., 0., 1920., 1200.));
        let got = resolve_selection(&["b".into(), "a".into()], &[a.clone(), b.clone()]);
        assert_eq!(got, vec![b, a]);
    }
}

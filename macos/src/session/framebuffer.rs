//! The remote desktop image, shared between the session engine (writer) and the
//! session windows (reader). Pixels are `0x00RRGGBB`.

/// Inclusive pixel rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
}

#[derive(Debug, Default)]
pub struct Framebuffer {
    width: u16,
    height: u16,
    pixels: Vec<u32>,
    dirty: bool,
}

impl Framebuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// Copy a tightly packed `region` into the image, resizing (to black) first if the
    /// remote desktop size changed. Returns true if the image was clean before.
    pub fn apply(&mut self, width: u16, height: u16, region: Region, packed: &[u32]) -> bool {
        if width != self.width || height != self.height {
            self.width = width;
            self.height = height;
            self.pixels = vec![0; usize::from(width) * usize::from(height)];
        }
        let region_w = usize::from(region.right.saturating_sub(region.left)) + 1;
        let fb_w = usize::from(width);
        let right = usize::from(region.right).min(fb_w.saturating_sub(1));
        let bottom = usize::from(region.bottom).min(usize::from(height).saturating_sub(1));
        let left = usize::from(region.left);
        let top = usize::from(region.top);
        if left <= right && top <= bottom && fb_w > 0 {
            let copy_w = right - left + 1;
            for (row, y) in (top..=bottom).enumerate() {
                let src_start = row * region_w;
                let Some(src) = packed.get(src_start..src_start + copy_w) else {
                    break;
                };
                let dst_start = y * fb_w + left;
                self.pixels[dst_start..dst_start + copy_w].copy_from_slice(src);
            }
        }
        self.mark_dirty()
    }

    /// Replace the whole image. Returns true if the image was clean before.
    pub fn replace(&mut self, width: u16, height: u16, pixels: Vec<u32>) -> bool {
        self.width = width;
        self.height = height;
        self.pixels = pixels;
        self.mark_dirty()
    }

    /// True once per batch of updates; the reader calls this when it redraws.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    fn mark_dirty(&mut self) -> bool {
        !std::mem::replace(&mut self.dirty, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_blits_region() {
        let mut fb = Framebuffer::new();
        fb.apply(4, 4, Region { left: 1, top: 1, right: 2, bottom: 2 }, &[1, 2, 3, 4]);
        let p = fb.pixels();
        assert_eq!((p[5], p[6], p[9], p[10]), (1, 2, 3, 4));
        assert_eq!(p.iter().filter(|v| **v != 0).count(), 4);
    }

    #[test]
    fn apply_resizes_and_clears() {
        let mut fb = Framebuffer::new();
        fb.apply(2, 2, Region { left: 0, top: 0, right: 1, bottom: 1 }, &[9; 4]);
        fb.apply(3, 3, Region { left: 0, top: 0, right: 0, bottom: 0 }, &[7]);
        assert_eq!((fb.width(), fb.height(), fb.pixels().len()), (3, 3, 9));
        assert_eq!(fb.pixels()[0], 7);
        assert!(fb.pixels()[1..].iter().all(|v| *v == 0));
    }

    #[test]
    fn apply_reports_clean_to_dirty_once() {
        let mut fb = Framebuffer::new();
        let r = Region { left: 0, top: 0, right: 0, bottom: 0 };
        assert!(fb.apply(1, 1, r, &[1]));
        assert!(!fb.apply(1, 1, r, &[2]));
        assert!(fb.take_dirty());
        assert!(!fb.take_dirty());
        assert!(fb.apply(1, 1, r, &[3]));
    }

    #[test]
    fn region_outside_framebuffer_is_clipped() {
        let mut fb = Framebuffer::new();
        fb.apply(2, 2, Region { left: 1, top: 1, right: 5, bottom: 5 }, &[7; 25]);
        assert_eq!(fb.pixels(), &[0, 0, 0, 7]);
    }

    #[test]
    fn short_packed_buffer_copies_only_covered_rows() {
        let mut fb = Framebuffer::new();
        fb.apply(2, 2, Region { left: 0, top: 0, right: 1, bottom: 1 }, &[5, 6]);
        assert_eq!(fb.pixels(), &[5, 6, 0, 0]);
    }

    #[test]
    fn replace_swaps_whole_image() {
        let mut fb = Framebuffer::new();
        assert!(fb.replace(2, 1, vec![1, 2]));
        assert_eq!((fb.width(), fb.height(), fb.pixels()), (2, 1, &[1u32, 2][..]));
    }
}

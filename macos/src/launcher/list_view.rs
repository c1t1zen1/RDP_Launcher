//! `ConnectionListView`: the whole launcher content — header strip, connection
//! buttons (owner-drawn like the Windows app), drag-to-reorder, and the + strip.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBezierPath, NSEvent, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSGradient,
    NSGraphicsContext, NSImage, NSImageSymbolConfiguration, NSLineBreakMode, NSMenu, NSMenuItem,
    NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSResponder, NSStringDrawing, NSTrackingArea,
    NSTrackingAreaOptions, NSView,
};
use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString, NSTimer};

use super::geometry::{
    content_height, drop_index, grip_rect, header_button_rect, hit_test, icon_rects, item_rect, plus_rect,
    HeaderButton, Hit, Spring, Zone, BTN_H, HANDLE_W, HEADER_H, MARGIN, STRIP_H, R,
};
use super::palette::{self, button_colors, color};
use super::ItemStatus;

/// What the user asked for; the controller turns these into app actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Connect(usize),
    Edit(usize),
    Duplicate(usize),
    Delete(usize),
    Add,
    Reordered { from: usize, to: usize },
    Minimize,
    TogglePin,
    About,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Item(usize, Zone),
    Header(HeaderButton),
    Plus,
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    from: usize,
    idx: usize,
    grab_dy: f64,
    top: f64,
}

struct Item {
    name: String,
    host: String,
    status: ItemStatus,
    spring: Spring,
}

pub struct ListIvars {
    items: RefCell<Vec<Item>>,
    scroll: Cell<f64>,
    hot: Cell<Option<Target>>,
    pressed: Cell<Option<Target>>,
    drag: Cell<Option<Drag>>,
    pinned: Cell<bool>,
    timer: RefCell<Option<Retained<NSTimer>>>,
    tooltips: RefCell<Vec<Retained<NSString>>>,
    tracking: RefCell<Option<Retained<NSTrackingArea>>>,
    on_action: RefCell<Option<Rc<dyn Fn(Action)>>>,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSView, NSResponder))]
    #[thread_kind = MainThreadOnly]
    #[name = "RdpLauncherConnectionListView"]
    #[ivars = ListIvars]
    pub struct ConnectionListView;

    impl ConnectionListView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            if let Some(old) = self.ivars().tracking.borrow_mut().take() {
                self.removeTrackingArea(&old);
            }
            let options = NSTrackingAreaOptions::MouseMoved
                | NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect;
            let area = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    NSTrackingArea::alloc(),
                    self.bounds(),
                    options,
                    Some(self),
                    None,
                )
            };
            self.addTrackingArea(&area);
            *self.ivars().tracking.borrow_mut() = Some(area);
            let _: () = unsafe { msg_send![super(self), updateTrackingAreas] };
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let t = self.target_at(self.point(event));
            if t != self.ivars().hot.get() {
                self.ivars().hot.set(t);
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.ivars().hot.set(None);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let p = self.point(event);
            match self.target_at(p) {
                Some(Target::Item(i, Zone::Grip)) => {
                    let top = self.ivars().items.borrow()[i].spring.pos;
                    self.ivars().drag.set(Some(Drag { from: i, idx: i, grab_dy: p.y - top, top }));
                    self.start_animation();
                }
                t => self.ivars().pressed.set(t),
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let p = self.point(event);
            if let Some(mut d) = self.ivars().drag.get() {
                d.top = p.y - d.grab_dy;
                let n = self.ivars().items.borrow().len();
                let target = drop_index(d.top, n, self.ivars().scroll.get());
                if target != d.idx {
                    let mut items = self.ivars().items.borrow_mut();
                    let item = items.remove(d.idx);
                    items.insert(target, item);
                    d.idx = target;
                }
                self.ivars().drag.set(Some(d));
                self.start_animation();
            } else {
                let t = self.target_at(p);
                if t != self.ivars().hot.get() {
                    self.ivars().hot.set(t);
                }
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let p = self.point(event);
            let action = if let Some(d) = self.ivars().drag.take() {
                self.start_animation();
                self.rebuild_tooltips();
                (d.from != d.idx).then_some(Action::Reordered { from: d.from, to: d.idx })
            } else {
                let pressed = self.ivars().pressed.take();
                match (pressed, self.target_at(p)) {
                    (Some(a), Some(b)) if a == b => action_for(a),
                    _ => None,
                }
            };
            self.setNeedsDisplay(true);
            if let Some(a) = action {
                self.fire(a);
            }
        }

        #[unsafe(method_id(menuForEvent:))]
        fn menu_for_event(&self, event: &NSEvent) -> Option<Retained<NSMenu>> {
            match self.target_at(self.point(event)) {
                Some(Target::Item(i, _)) => Some(self.context_menu(i)),
                _ => None,
            }
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let n = self.ivars().items.borrow().len();
            let max = (content_height(n) - self.bounds().size.height).max(0.0);
            let s = (self.ivars().scroll.get() - event.scrollingDeltaY()).clamp(0.0, max);
            if s != self.ivars().scroll.get() {
                self.ivars().scroll.set(s);
                self.snap_springs();
                self.rebuild_tooltips();
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(animTick:))]
        fn anim_tick(&self, _timer: &NSTimer) {
            let drag = self.ivars().drag.get();
            let scroll = self.ivars().scroll.get();
            let width = self.bounds().size.width;
            let mut settled = true;
            for (i, item) in self.ivars().items.borrow_mut().iter_mut().enumerate() {
                if let Some(d) = drag.filter(|d| d.idx == i) {
                    item.spring = Spring { pos: d.top, vel: 0.0 };
                    continue;
                }
                settled &= item.spring.step(item_rect(i, scroll, width).y);
            }
            if settled && drag.is_none() {
                if let Some(t) = self.ivars().timer.borrow_mut().take() {
                    t.invalidate();
                }
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(menuConnect:))]
        fn menu_connect(&self, sender: &NSMenuItem) {
            self.fire(Action::Connect(sender.tag() as usize));
        }

        #[unsafe(method(menuEdit:))]
        fn menu_edit(&self, sender: &NSMenuItem) {
            self.fire(Action::Edit(sender.tag() as usize));
        }

        #[unsafe(method(menuDuplicate:))]
        fn menu_duplicate(&self, sender: &NSMenuItem) {
            self.fire(Action::Duplicate(sender.tag() as usize));
        }

        #[unsafe(method(menuDelete:))]
        fn menu_delete(&self, sender: &NSMenuItem) {
            self.fire(Action::Delete(sender.tag() as usize));
        }
    }
);

fn action_for(t: Target) -> Option<Action> {
    Some(match t {
        Target::Item(i, Zone::Body) => Action::Connect(i),
        Target::Item(i, Zone::Edit) => Action::Edit(i),
        Target::Item(i, Zone::Duplicate) => Action::Duplicate(i),
        Target::Item(i, Zone::Delete) => Action::Delete(i),
        Target::Item(_, Zone::Grip) => return None,
        Target::Header(HeaderButton::Minimize) => Action::Minimize,
        Target::Header(HeaderButton::Pin) => Action::TogglePin,
        Target::Header(HeaderButton::Info) => Action::About,
        Target::Plus => Action::Add,
    })
}

fn ns_rect(r: &R) -> NSRect {
    NSRect::new(NSPoint::new(r.x, r.y), NSSize::new(r.w, r.h))
}

fn attrs(font: &NSFont, rgb: u32, truncate: bool) -> Retained<NSDictionary<NSString, AnyObject>> {
    let color = color(rgb);
    let para = NSMutableParagraphStyle::new();
    if truncate {
        para.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    }
    let keys: [&NSString; 3] = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName] };
    let values: [&AnyObject; 3] = [font.as_ref(), color.as_ref(), para.as_ref()];
    NSDictionary::from_slices(&keys, &values)
}

fn draw_text(text: &str, rect: R, font: &NSFont, rgb: u32) {
    let s = NSString::from_str(text);
    let a = attrs(font, rgb, true);
    unsafe { s.drawInRect_withAttributes(ns_rect(&rect), Some(&a)) };
}

fn symbol(name: &str, point_size: f64, rgb: u32) -> Option<Retained<NSImage>> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)?;
    let size = NSImageSymbolConfiguration::configurationWithPointSize_weight(point_size, 0.0);
    let tint = NSImageSymbolConfiguration::configurationWithHierarchicalColor(&color(rgb));
    image.imageWithSymbolConfiguration(&size.configurationByApplyingConfiguration(&tint))
}

/// Draw an SF Symbol centred in `rect`, keeping its aspect ratio.
fn draw_symbol(name: &str, rect: R, point_size: f64, rgb: u32) {
    let Some(img) = symbol(name, point_size, rgb) else { return };
    let s = img.size();
    let r = R { x: rect.x + (rect.w - s.width) / 2.0, y: rect.y + (rect.h - s.height) / 2.0, w: s.width, h: s.height };
    img.drawInRect(ns_rect(&r));
}

fn fill_rect(r: R, rgb: u32) {
    color(rgb).setFill();
    NSBezierPath::fillRect(ns_rect(&r));
}

fn hline(x0: f64, x1: f64, y: f64, rgb: u32, width: f64) {
    let p = NSBezierPath::bezierPath();
    p.moveToPoint(NSPoint::new(x0, y));
    p.lineToPoint(NSPoint::new(x1, y));
    p.setLineWidth(width);
    color(rgb).setStroke();
    p.stroke();
}

fn draw_button_body(r: R, top: u32, bottom: u32, border: u32) {
    let inset = R { x: r.x + 0.5, y: r.y + 0.5, w: r.w - 1.0, h: r.h - 1.0 };
    let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(ns_rect(&inset), 3.0, 3.0);
    let gradient = NSGradient::initWithStartingColor_endingColor(NSGradient::alloc(), &color(top), &color(bottom));
    if let Some(g) = gradient {
        // Flipped view: 90° runs top → bottom on screen.
        g.drawInBezierPath_angle(&path, 90.0);
    }
    color(border).setStroke();
    path.setLineWidth(1.0);
    path.stroke();
}

impl ConnectionListView {
    pub fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ListIvars {
            items: RefCell::new(Vec::new()),
            scroll: Cell::new(0.0),
            hot: Cell::new(None),
            pressed: Cell::new(None),
            drag: Cell::new(None),
            pinned: Cell::new(true),
            timer: RefCell::new(None),
            tooltips: RefCell::new(Vec::new()),
            tracking: RefCell::new(None),
            on_action: RefCell::new(None),
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    pub fn set_on_action(&self, f: Rc<dyn Fn(Action)>) {
        *self.ivars().on_action.borrow_mut() = Some(f);
    }

    /// Replace the list (name, host, status). Positions snap; a drag in progress is cancelled.
    pub fn set_items(&self, items: Vec<(String, String, ItemStatus)>) {
        self.ivars().drag.set(None);
        let n = items.len();
        let max = (content_height(n) - self.bounds().size.height).max(0.0);
        self.ivars().scroll.set(self.ivars().scroll.get().min(max));
        *self.ivars().items.borrow_mut() = items
            .into_iter()
            .map(|(name, host, status)| Item { name, host, status, spring: Spring { pos: 0.0, vel: 0.0 } })
            .collect();
        self.snap_springs();
        self.rebuild_tooltips();
        self.setNeedsDisplay(true);
    }

    pub fn set_status(&self, index: usize, status: ItemStatus) {
        if let Some(item) = self.ivars().items.borrow_mut().get_mut(index) {
            item.status = status;
        }
        self.setNeedsDisplay(true);
    }

    pub fn set_pinned(&self, on: bool) {
        self.ivars().pinned.set(on);
        self.rebuild_tooltips();
        self.setNeedsDisplay(true);
    }

    pub fn len(&self) -> usize {
        self.ivars().items.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Re-place every button at its slot (after resize / scroll / new list).
    pub fn snap_springs(&self) {
        let scroll = self.ivars().scroll.get();
        let width = self.bounds().size.width;
        for (i, item) in self.ivars().items.borrow_mut().iter_mut().enumerate() {
            item.spring = Spring { pos: item_rect(i, scroll, width).y, vel: 0.0 };
        }
    }

    fn fire(&self, action: Action) {
        let cb = self.ivars().on_action.borrow().clone();
        if let Some(cb) = cb {
            cb(action);
        }
    }

    fn point(&self, event: &NSEvent) -> NSPoint {
        self.convertPoint_fromView(event.locationInWindow(), None)
    }

    fn target_at(&self, p: NSPoint) -> Option<Target> {
        let b = self.bounds().size;
        for button in [HeaderButton::Minimize, HeaderButton::Pin, HeaderButton::Info] {
            if header_button_rect(button, b.width).contains(p.x, p.y) {
                return Some(Target::Header(button));
            }
        }
        if plus_rect(b.width, b.height).contains(p.x, p.y) {
            return Some(Target::Plus);
        }
        if p.y < HEADER_H || p.y >= b.height - STRIP_H {
            return None;
        }
        match hit_test(p.x, p.y, self.len(), self.ivars().scroll.get(), b.width) {
            Hit::Item(i, z) => Some(Target::Item(i, z)),
            Hit::None => None,
        }
    }

    fn start_animation(&self) {
        if self.ivars().timer.borrow().is_some() {
            return;
        }
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                0.016,
                self,
                sel!(animTick:),
                None,
                true,
            )
        };
        *self.ivars().timer.borrow_mut() = Some(timer);
    }

    fn context_menu(&self, index: usize) -> Retained<NSMenu> {
        let mtm = MainThreadMarker::from(self);
        let menu = NSMenu::new(mtm);
        let entries: [(&str, &str, Sel); 4] = [
            ("Connect", "display", sel!(menuConnect:)),
            ("Edit / Rename…", "pencil", sel!(menuEdit:)),
            ("Duplicate", "doc.on.doc", sel!(menuDuplicate:)),
            ("Delete", "trash", sel!(menuDelete:)),
        ];
        for (title, icon, action) in entries {
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    Some(action),
                    &NSString::from_str(""),
                )
            };
            unsafe { item.setTarget(Some(self)) };
            item.setTag(index as isize);
            item.setImage(NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(icon), None).as_deref());
            menu.addItem(&item);
        }
        menu
    }

    fn add_tip(&self, r: R, text: &str) {
        let s = NSString::from_str(text);
        // An NSString owner supplies its own description as the tooltip text.
        unsafe { self.addToolTipRect_owner_userData(ns_rect(&r), &s, std::ptr::null_mut()) };
        self.ivars().tooltips.borrow_mut().push(s);
    }

    fn rebuild_tooltips(&self) {
        self.removeAllToolTips();
        self.ivars().tooltips.borrow_mut().clear();
        let b = self.bounds().size;
        let pin_tip = if self.ivars().pinned.get() {
            "Always on top: ON\nThe window stays above other windows.\nClick to let other windows cover it."
        } else {
            "Always on top: OFF\nOther windows can cover this one, like a normal app.\nClick to keep it above everything."
        };
        self.add_tip(header_button_rect(HeaderButton::Minimize, b.width), "Hide the launcher\nClick the menu-bar icon to bring it back");
        self.add_tip(header_button_rect(HeaderButton::Pin, b.width), pin_tip);
        self.add_tip(header_button_rect(HeaderButton::Info, b.width), "About RDP Launcher and user manual");
        self.add_tip(plus_rect(b.width, b.height), "Add a new RDP connection");
        let names: Vec<String> = self.ivars().items.borrow().iter().map(|i| i.name.clone()).collect();
        let scroll = self.ivars().scroll.get();
        for (i, name) in names.iter().enumerate() {
            let item = item_rect(i, scroll, b.width);
            if item.bottom() <= HEADER_H || item.y >= b.height - STRIP_H {
                continue;
            }
            let icons = icon_rects(&item);
            let body = R { x: item.x, y: item.y, w: icons[0].x - 2.0 - item.x, h: item.h };
            self.add_tip(body, &format!("Click to connect to {name}\nRight-click for more options"));
            self.add_tip(icons[0], "Edit this connection (name, host, username, password)");
            self.add_tip(icons[1], "Duplicate this connection");
            self.add_tip(icons[2], "Delete this connection");
            self.add_tip(grip_rect(&item), "Drag up or down to reorder the list");
        }
    }

    fn draw(&self) {
        let b = self.bounds().size;
        fill_rect(R { x: 0.0, y: 0.0, w: b.width, h: b.height }, palette::WINDOW_BG);

        // Connection buttons, clipped to the area between header and strip.
        NSGraphicsContext::saveGraphicsState_class();
        NSBezierPath::bezierPathWithRect(ns_rect(&R { x: 0.0, y: HEADER_H, w: b.width, h: b.height - HEADER_H - STRIP_H })).addClip();
        let drag = self.ivars().drag.get();
        let items = self.ivars().items.borrow();
        for (i, item) in items.iter().enumerate() {
            if drag.is_some_and(|d| d.idx == i) {
                continue;
            }
            self.draw_item(i, item, b.width, false);
        }
        if let Some(d) = drag {
            if let Some(item) = items.get(d.idx) {
                self.draw_item(d.idx, item, b.width, true);
            }
        }
        drop(items);
        NSGraphicsContext::restoreGraphicsState_class();

        self.draw_header(b.width);
        self.draw_strip(b.width, b.height);
    }

    fn draw_item(&self, i: usize, item: &Item, width: f64, dragging: bool) {
        let r = R { x: MARGIN, y: item.spring.pos, w: width - 2.0 * MARGIN, h: BTN_H };
        let connected = item.status != ItemStatus::Idle;
        let hot = self.ivars().hot.get();
        let pressed = dragging || self.ivars().pressed.get().is_some_and(|t| matches!(t, Target::Item(j, Zone::Body) if j == i));
        let is_hot = matches!(hot, Some(Target::Item(j, _)) if j == i);
        let c = button_colors(connected, pressed, is_hot);
        draw_button_body(r, c.top, c.bottom, c.border);

        let name = if item.name.is_empty() { item.host.as_str() } else { item.name.as_str() };
        let icons = icon_rects(&r);
        draw_text(name, R { x: r.x + 10.0, y: r.y + 8.0, w: r.right() - HANDLE_W - 4.0 - r.x - 10.0, h: 22.0 }, &NSFont::boldSystemFontOfSize(14.0), c.text);
        let sub = match item.status {
            ItemStatus::Idle => item.host.clone(),
            ItemStatus::Connecting => format!("{}  •  connecting…", item.host),
            ItemStatus::Connected => format!("{}  •  connected", item.host),
        };
        draw_text(&sub, R { x: r.x + 10.0, y: r.y + 33.0, w: icons[0].x - 4.0 - r.x - 10.0, h: 18.0 }, &NSFont::systemFontOfSize(11.0), c.sub);

        for (rect, (name, zone)) in icons.iter().zip([("pencil", Zone::Edit), ("doc.on.doc", Zone::Duplicate), ("trash", Zone::Delete)]) {
            let rgb = if hot == Some(Target::Item(i, zone)) { palette::PIN_ON } else { c.sub };
            draw_symbol(name, *rect, 11.0, rgb);
        }

        let g = grip_rect(&r);
        let cx = g.x + g.w / 2.0;
        let cy = g.y + g.h / 2.0;
        for dy in [-5.0, 0.0, 5.0] {
            hline(cx - 7.0, cx + 7.0, cy + dy, c.grip, 2.0);
        }
    }

    fn draw_flat_button(&self, rect: R, target: Target) {
        let bg = if self.ivars().pressed.get() == Some(target) {
            Some(palette::FLAT_PRESSED)
        } else if self.ivars().hot.get() == Some(target) {
            Some(palette::FLAT_HOT)
        } else {
            None
        };
        if let Some(bg) = bg {
            color(bg).setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(ns_rect(&rect), 4.0, 4.0).fill();
        }
    }

    fn draw_header(&self, width: f64) {
        fill_rect(R { x: 0.0, y: 0.0, w: width, h: HEADER_H }, palette::HEADER_BG);
        hline(0.0, width, HEADER_H - 0.5, palette::HEADER_LINE, 1.0);
        let n = self.len();
        let label = match n {
            0 => "No connections yet - click + to add one".to_owned(),
            1 => "1 connection".to_owned(),
            _ => format!("{n} connections"),
        };
        let info_x = header_button_rect(HeaderButton::Info, width).x;
        draw_text(&label, R { x: 10.0, y: 8.0, w: info_x - 14.0, h: 18.0 }, &NSFont::systemFontOfSize(12.0), palette::HEADER_TEXT);

        let pinned = self.ivars().pinned.get();
        let buttons = [
            (HeaderButton::Minimize, "minus", palette::ICON_DARK),
            (HeaderButton::Pin, if pinned { "pin.fill" } else { "pin.slash" }, if pinned { palette::PIN_ON } else { palette::PIN_OFF }),
            (HeaderButton::Info, "info.circle", palette::ICON_DARK),
        ];
        for (button, icon, rgb) in buttons {
            let r = header_button_rect(button, width);
            self.draw_flat_button(r, Target::Header(button));
            draw_symbol(icon, r, 13.0, rgb);
        }
    }

    fn draw_strip(&self, width: f64, height: f64) {
        fill_rect(R { x: 0.0, y: height - STRIP_H, w: width, h: STRIP_H }, palette::WINDOW_BG);
        hline(0.0, width, height - STRIP_H + 0.5, palette::HEADER_LINE, 1.0);
        let r = plus_rect(width, height);
        let pressed = self.ivars().pressed.get() == Some(Target::Plus);
        let hot = self.ivars().hot.get() == Some(Target::Plus);
        let c = button_colors(false, pressed, hot);
        draw_button_body(r, c.top, c.bottom, c.border);
        draw_symbol("plus", r, 14.0, palette::ICON_DARK);
    }
}

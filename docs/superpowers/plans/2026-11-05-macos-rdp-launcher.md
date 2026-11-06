# RDP Launcher for macOS — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `RDP Launcher.app` for Apple Silicon, a launcher with the same features as Windows v1.2.0 plus a built-in pure-Rust RDP client (IronRDP), packaged as a drag-to-Applications `.dmg`.

**Architecture:** One Rust process. The launcher and every session window are raw AppKit windows (via `objc2`) on the main thread. Each RDP session is an `ironrdp_client::rdp::RdpClient` running on a shared multi-threaded `tokio` runtime. Engine events cross to the main thread via `dispatch2`. The framebuffer lives behind `Arc<Mutex<_>>`, and the UI coalesces redraws. Pure logic (model, keymap, layout math, certs, framebuffer, clipboard codec) lives in a library crate with unit tests. AppKit code is verified by running the app.

**Tech Stack:** Rust 1.99 stable (`aarch64-apple-darwin`); `ironrdp-client` and companion crates from git at a pinned commit; `tokio`; `objc2` 0.6, `objc2-app-kit`/`objc2-foundation`/`objc2-quartz-core`/`objc2-core-graphics` 0.3 (default features); `dispatch2` 0.3; `security-framework` 3; `serde`/`serde_json`; `uuid`; `sha2`; `tracing`.

**Spec:** `docs/superpowers/specs/2026-11-05-macos-rdp-launcher-design.md`

## Global Constraints

- **Never modify** the Windows code: root `Cargo.toml`, `Cargo.lock`, `build.rs`, `src/`, `.cargo/`, `tools/`, `assets/`. All new code goes under `macos/` (a standalone Cargo package with its own `Cargo.lock`). Task 17 may add a section to the root `README.md`.
- Run all cargo commands from `macos/` (`cd macos && cargo …`). Load the toolchain with `. "$HOME/.cargo/env"` if `cargo` is not on PATH.
- **IronRDP source:** every `ironrdp-*` dependency comes from `git = "https://github.com/Devolutions/IronRDP", rev = "da016ce860040c6cb37943cfc857d836735d252c"`. Never mix in crates.io `ironrdp*` crates, because duplicate type families won't unify. Reference sources live in `~/.cargo/git/checkouts/ironrdp-*/da016ce/crates/` after the first build.
- `ironrdp-client` features: `rustls`, `sound`, `clipboard`, `rdpdr`, `openh264`. Set these explicitly in every session config: `with_audio_capture(false)` (a microphone request without a usage string crashes under TCC), `with_printers(false)`, `with_ports(false)`, and `with_certificate_validation(CertificateValidation::Strict)` plus our trust-on-first-use callback.
- Target: `aarch64-apple-darwin`, `LSMinimumSystemVersion` 13.0. Bundle id `com.c1t1zen.rdplauncher`, display name `RDP Launcher`, version `1.0.0`.
- Data dir `~/Library/Application Support/RdpLauncher/` (`connections.json`, `settings.json`). Keychain generic password, service `RDP Launcher`, account = connection `id`.
- Passwords never touch JSON, temp files, logs or `Debug` output. Log file: `~/Library/Logs/RDP Launcher/rdp-launcher.log`.
- Remote resolution in points at 100% scale. RDP client name `Mac`. Keyboard layout `0x409`. TCP connect timeout 10 s.
- Launcher geometry (points): `BTN_W 300, BTN_H 56, MARGIN 12, GAP 8, HEADER_H 32, HDR_BTN 24, STRIP_H 44, PLUS_SIZE 30, HANDLE_W 34`. Max window height = 85% of the screen's visible frame. Spring `STIFFNESS 0.30`, `DAMPING 0.70`, 16 ms timer.
- Windowed sessions open at 1280×800 points, clamped to 90% of the display's visible frame. Resize debounce 300 ms. Clipboard poll 500 ms.

## Review Focus

1. **Stuck keys after focus loss.** The user holds ⌘ or Shift and ⌘Tabs away. Every key held down on the remote must be released, and the ⌘ state must reset. Test: `keymap::tests::reset_after_focus_loss_forgets_cmd` (Task 2) and `engine_e2e::release_all_releases_held_key` (Task 6).
2. **Corrupt or hand-edited `connections.json`** (for example pasted from Windows with a typo). The app must start and preserve the bad file instead of silently overwriting it. Test: `model::tests::corrupt_file_is_backed_up` (Task 1).
3. **Odd host input:** whitespace, `host:port`, `[::1]:3390`, a bare IPv6 address, or a bad port. The input must parse correctly or show a validation message, never crash at connect time. Test: `model::tests::parse_host_*` (Task 1).
4. **Display changes:** saved monitors unplugged (laptop undocked), or a non-adjacent selection. The session must fall back to the main display or pack the displays side by side, never fail to connect. Test: `monitors::tests::{unplugged_selection_falls_back_to_main, non_adjacent_selection_is_packed_side_by_side}` (Task 4).
5. **Server drops the session**, whether from a reboot, a network loss or a logoff. The app must report it once, leave other sessions untouched, clear the green glow, and never panic. Test: `engine_e2e::server_disconnect_reports_end` (Task 6).

---

## File Structure

```
macos/
├── Cargo.toml, .gitignore, README.md
├── resources/Info.plist, resources/AppIcon.icns
├── scripts/make-icon.sh, scripts/build-app.sh
├── src/
│   ├── lib.rs                    module declarations (everything below except main.rs)
│   ├── main.rs                   NSApplication bootstrap only
│   ├── app.rs                    AppDelegate, main menu (App / Edit / Session / Window), lifecycle
│   ├── logging.rs                tracing → log file
│   ├── model.rs                  Connection, Settings, Store, parse_host, split_username   [pure]
│   ├── keychain.rs               Keychain wrapper
│   ├── certs.rs                  fingerprints + trust decision                           [pure]
│   ├── monitors.rs               display selection → remote monitor layout               [pure]
│   ├── displays.rs               NSScreen enumeration → monitors::DisplayInfo
│   ├── launcher/
│   │   ├── mod.rs                LauncherController (window, header, autosize, actions)
│   │   ├── geometry.rs           rects, hit-testing, drop index, Spring                  [pure]
│   │   ├── palette.rs            Windows-parity colors
│   │   ├── list_view.rs          ConnectionListView (NSView subclass): draw, hover, drag, menu, tooltips
│   │   ├── edit_dialog.rs        add/edit panel
│   │   ├── password_prompt.rs    "Password for X" + Remember
│   │   ├── manual.rs             About/manual text                                       [pure]
│   │   ├── about.rs              About window
│   │   └── status_item.rs        menu-bar icon
│   └── session/
│       ├── mod.rs                SessionManager (id → Session), status callbacks, focus-existing
│       ├── engine.rs             start_session / SessionHandle / SessionEvent (wraps RdpClient)
│       ├── framebuffer.rs        Framebuffer + Region                                    [pure]
│       ├── geometry.rs           view→remote mapping, contents rect, scroll accumulator  [pure]
│       ├── keymap.rs             mac keycode → scancode, KeyTranslator (⌘ rules)         [pure]
│       ├── view.rs               RdpView (NSView subclass): render + input
│       ├── window.rs             SessionWindows: one or more NSWindows per session, modes, resize
│       ├── alerts.rs             certificate / error / password sheets
│       ├── clipboard_codec.rs    UTF-16 text codec + PasteboardWatcher                   [pure]
│       ├── clipboard.rs          MacClipboard (CliprdrBackend) + factory
│       └── drive.rs              shared-folder RDPDR factory
└── tests/
    ├── engine_e2e.rs             in-process ironrdp-server ↔ our engine
    └── certs/server-cert.pem, server-key.pem   (copied from IronRDP testsuite at the pinned rev)
```

---

## Phase 1 — Foundation

### Task 1: Crate scaffold, data model and storage

**Files:**
- Create: `macos/Cargo.toml`, `macos/.gitignore`, `macos/src/lib.rs`, `macos/src/model.rs`

**Interfaces:**
- Produces (`rdp_launcher::model`):
  - `pub const DEFAULT_RDP_PORT: u16 = 3389;`
  - `#[serde(rename_all = "lowercase")] pub enum DisplayMode { #[default] Fullscreen, Windowed }`
  - `pub struct Connection { pub id: String, pub name: String, pub host: String, pub username: String, pub display: DisplayMode, pub monitors: Vec<String>, pub shared_folder: Option<PathBuf>, pub sound: bool, pub cert_fingerprint: Option<String> }`, with `Clone, Debug, PartialEq, Serialize, Deserialize`. Every field except `name` and `host` uses `#[serde(default)]`. `sound` defaults to `true`. `id` defaults to a fresh `uuid::Uuid::new_v4().to_string()`. Unknown fields such as Windows' `password_enc` are ignored.
  - `impl Connection { pub fn new(name: &str, host: &str) -> Self; pub fn duplicate(&self) -> Self; pub fn endpoint(&self) -> Result<(String, u16), HostError>; pub fn credentials(&self) -> (String, Option<String>) }`
  - `pub enum HostError { Empty, InvalidPort }` (implements `Display`: "Enter a host name or IP address." / "The port must be a number from 1 to 65535.")
  - `pub fn parse_host(input: &str) -> Result<(String, u16), HostError>`
  - `pub fn split_username(input: &str) -> (String, Option<String>)`
  - `pub struct Settings { pub always_on_top: bool }` (default `true`)
  - `pub struct Store { dir: PathBuf }`, `impl Store { pub fn new(dir: PathBuf) -> Self; pub fn default_dir() -> PathBuf; pub fn load_connections(&self) -> Vec<Connection>; pub fn save_connections(&self, conns: &[Connection]) -> std::io::Result<()>; pub fn load_settings(&self) -> Settings; pub fn save_settings(&self, s: &Settings) -> std::io::Result<()> }`

- [ ] **Step 1: Create `macos/Cargo.toml`.** `[package] name = "rdp-launcher-macos"`, `version = "1.0.0"`, `edition = "2021"`, `license = "MIT"`. Declare `[lib] name = "rdp_launcher"` and `[[bin]] name = "rdp-launcher"`, `path = "src/main.rs"`. Add a `[workspace]` table (empty) so Cargo never associates the package with the root package.

  Dependencies:
  - IronRDP, all at the pinned git rev: `ironrdp-client` (features above), `ironrdp-input`, `ironrdp-pdu`, `ironrdp-displaycontrol`, `ironrdp-tls` (features `rustls`), `ironrdp-cliprdr`, `ironrdp-rdpdr`, `ironrdp-rdpdr-native`, `ironrdp-graphics`.
  - Runtime and app crates: `tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time"] }`, `objc2 = "0.6"`, `objc2-foundation = "0.3"`, `objc2-app-kit = "0.3"`, `objc2-quartz-core = "0.3"`, `objc2-core-graphics = "0.3"`, `objc2-core-foundation = "0.3"`, `dispatch2 = "0.3"`, `security-framework = "3"`.
  - Utilities: `serde = { version = "1", features = ["derive"] }`, `serde_json = "1"`, `uuid = { version = "1", features = ["v4"] }`, `sha2 = "0.10"`, `anyhow = "1"`, `tracing = "0.1"`, `tracing-subscriber = { version = "0.3", features = ["fmt"] }`, `smallvec = "1"`.
  - `[dev-dependencies]`: `ironrdp-server` (git, same rev), `tempfile = "3"`, `tokio` with `"full"`.
  - `[profile.release]`: `opt-level = 3`, `lto = true`, `codegen-units = 1`, `strip = true`.

  `macos/.gitignore` contains `target/` and `dist/`.

- [ ] **Step 2: Write the failing tests in `macos/src/model.rs` (`#[cfg(test)] mod tests`).**

```rust
#[test] fn parse_host_plain_ip() { assert_eq!(parse_host(" 192.168.0.161 ").unwrap(), ("192.168.0.161".into(), 3389)); }
#[test] fn parse_host_with_port() { assert_eq!(parse_host("server:3390").unwrap(), ("server".into(), 3390)); }
#[test] fn parse_host_bracketed_ipv6() { assert_eq!(parse_host("[::1]:3390").unwrap(), ("::1".into(), 3390)); }
#[test] fn parse_host_bare_ipv6() { assert_eq!(parse_host("fe80::1").unwrap(), ("fe80::1".into(), 3389)); }
#[test] fn parse_host_rejects_bad_port() { assert_eq!(parse_host("host:abc"), Err(HostError::InvalidPort)); assert_eq!(parse_host("host:0"), Err(HostError::InvalidPort)); }
#[test] fn parse_host_rejects_empty() { assert_eq!(parse_host("   "), Err(HostError::Empty)); }
#[test] fn split_username_forms() {
    assert_eq!(split_username("pi"), ("pi".into(), None));
    assert_eq!(split_username("CORP\\kevin"), ("kevin".into(), Some("CORP".into())));
    assert_eq!(split_username("kevin@corp.local"), ("kevin@corp.local".into(), None));
}
#[test] fn windows_format_loads_with_defaults() {
    // exact shape written by the Windows app
    let json = r#"[{"name":"Pi5-16GB","host":"192.168.0.161","username":"pi","password_enc":"01ab"}]"#;
    let c: Vec<Connection> = serde_json::from_str(json).unwrap();
    assert_eq!(c[0].display, DisplayMode::Fullscreen);
    assert!(c[0].sound && c[0].monitors.is_empty() && c[0].shared_folder.is_none());
    assert_eq!(c[0].id.len(), 36);
}
#[test] fn duplicate_gets_new_id_and_copy_suffix() {
    let a = Connection::new("Media Server", "192.168.0.201");
    let b = a.duplicate();
    assert_ne!(a.id, b.id); assert_eq!(b.name, "Media Server (copy)"); assert_eq!(b.host, a.host);
}
#[test] fn save_then_load_round_trips_and_preserves_order() { /* tempdir Store; save [a,b,c]; load == [a,b,c] */ }
#[test] fn missing_ids_are_generated_and_persisted() { /* write Windows-format file; load twice; ids equal across loads */ }
#[test] fn corrupt_file_is_backed_up() {
    /* write "{not json" to connections.json; load → empty vec;
       connections.json.bak exists with "{not json"; original file untouched until next save */
}
#[test] fn settings_default_always_on_top() { /* empty tempdir → Settings { always_on_top: true } */ }
```

- [ ] **Step 3: Run tests to verify they fail.**
Run: `cd macos && cargo test --lib model`
Expected: compile errors or FAIL, because `parse_host` and the others don't exist yet. The first build downloads IronRDP from git; this takes a few minutes.

- [ ] **Step 4: Implement `model.rs`.**
  - `parse_host`: trim the input. `[addr]:port` gives `addr`. If there is exactly one `:`, split it into host and port. Two or more `:` without brackets means a bare IPv6 address on the default port. The port must parse as `u16 > 0`.
  - `Store::save_connections`: `create_dir_all`, write `connections.json.tmp`, then `rename`.
  - `load_connections`: a missing file returns empty. On a parse error, copy the file to `connections.json.bak`, log a warning and return empty. If any entry lacked an `id` (detect by checking for `"id"` in the raw `serde_json::Value`), save immediately.
  - `default_dir()` uses `$HOME/Library/Application Support/RdpLauncher`.
  - `lib.rs` declares `pub mod model;`.

- [ ] **Step 5: Run tests to verify they pass.**
Run: `cd macos && cargo test --lib model`
Expected: all model tests PASS.

- [ ] **Step 6: Commit.**
```bash
git add macos/Cargo.toml macos/Cargo.lock macos/.gitignore macos/src/lib.rs macos/src/model.rs
git commit -m "macos: scaffold crate with connection model and JSON store"
```

### Task 2: Key mapping and ⌘ translation

**Files:**
- Create: `macos/src/session/mod.rs` (just `pub mod keymap;` for now), `macos/src/session/keymap.rs`
- Modify: `macos/src/lib.rs` (`pub mod session;`)

**Interfaces:**
- Consumes: `ironrdp_input::{Operation, Scancode}`
- Produces (`rdp_launcher::session::keymap`):
  - `pub fn scancode_for_keycode(keycode: u16) -> Option<Scancode>`
  - `pub struct KeyTranslator`, `impl KeyTranslator { pub fn new() -> Self; pub fn key_down(&mut self, keycode: u16) -> Vec<Operation>; pub fn key_up(&mut self, keycode: u16) -> Vec<Operation>; pub fn modifier_changed(&mut self, keycode: u16, pressed: bool) -> Vec<Operation>; pub fn reset(&mut self) }`
  - `pub fn ctrl_alt_del() -> Vec<Operation>` (press LCtrl, LAlt, extended Delete `E0 53`, then release in reverse order) and `pub fn windows_key() -> Vec<Operation>` (press and release `E0 5B`)
  - Keycode constants: `KC_CMD_L = 0x37, KC_CMD_R = 0x36, KC_SHIFT_L = 0x38, KC_SHIFT_R = 0x3C, KC_OPT_L = 0x3A, KC_OPT_R = 0x3D, KC_CTRL_L = 0x3B, KC_CTRL_R = 0x3E, KC_CAPS = 0x39, KC_FN = 0x3F`

Scancode table (macOS `kVK_*` → PC set-1; `E0` means extended):

```
00 A→1E  01 S→1F  02 D→20  03 F→21  04 H→23  05 G→22  06 Z→2C  07 X→2D  08 C→2E  09 V→2F
0A §(ISO)→56  0B B→30  0C Q→10  0D W→11  0E E→12  0F R→13  10 Y→15  11 T→14
12 1→02  13 2→03  14 3→04  15 4→05  16 6→07  17 5→06  18 =→0D  19 9→0A  1A 7→08  1B -→0C
1C 8→09  1D 0→0B  1E ]→1B  1F O→18  20 U→16  21 [→1A  22 I→17  23 P→19  24 Return→1C
25 L→26  26 J→24  27 '→28  28 K→25  29 ;→27  2A \→2B  2B ,→33  2C /→35  2D N→31  2E M→32
2F .→34  30 Tab→0F  31 Space→39  32 `→29  33 Backspace→0E  35 Esc→01
38 LShift→2A  39 Caps→3A  3A LOpt→38  3B LCtrl→1D  3C RShift→36  3D ROpt→E0 38  3E RCtrl→E0 1D
40 F17→68  41 KP.→53  43 KP*→37  45 KP+→4E  47 KPClear→45  4B KP/→E0 35  4C KPEnter→E0 1C
4E KP-→4A  4F F18→69  50 F19→6A  51 KP=→59  52 KP0→52  53 KP1→4F  54 KP2→50  55 KP3→51
56 KP4→4B  57 KP5→4C  58 KP6→4D  59 KP7→47  5A F20→6B  5B KP8→48  5C KP9→49
5D JIS¥→7D  5E JIS_→73  5F JIS,→7E  60 F5→3F  61 F6→40  62 F7→41  63 F3→3D  64 F8→42
65 F9→43  66 Eisu→7B  67 F11→57  68 Kana→79  69 F13→64  6A F16→67  6B F14→65  6D F10→44
6E Menu→E0 5D  6F F12→58  71 F15→66  72 Help/Insert→E0 52  73 Home→E0 47  74 PgUp→E0 49
75 FwdDelete→E0 53  76 F4→3E  77 End→E0 4F  78 F2→3C  79 PgDn→E0 51  7A F1→3B
7B Left→E0 4B  7C Right→E0 4D  7D Down→E0 50  7E Up→E0 48
```

⌘ rules:
- ⌘ down sends nothing and records that ⌘ is held.
- A non-modifier key while ⌘ is held sends LCtrl (`1D`) down once per hold, then the key.
- On ⌘ up: if Ctrl was injected, send LCtrl up. Otherwise, if no key was pressed during the hold, send the Windows key (press and release).
- Ctrl, Shift and Option press and release their own scancodes.
- Caps Lock: every change sends press and release of `3A`.
- `KC_FN` and unknown keycodes are ignored.

- [ ] **Step 1: Write the failing tests.**

```rust
use ironrdp_input::{Operation::*, Scancode};
fn sc(ext: bool, c: u8) -> Scancode { Scancode::from_u8(ext, c) }
#[test] fn letter_a_maps_to_1e() { assert_eq!(scancode_for_keycode(0x00), Some(sc(false, 0x1E))); }
#[test] fn arrows_are_extended() { assert_eq!(scancode_for_keycode(0x7E), Some(sc(true, 0x48))); }
#[test] fn function_key_is_ignored() { assert_eq!(scancode_for_keycode(KC_FN), None); }
#[test] fn cmd_c_sends_ctrl_c() {
    let mut t = KeyTranslator::new();
    assert!(t.modifier_changed(KC_CMD_L, true).is_empty());
    assert_eq!(t.key_down(0x08), vec![KeyPressed(sc(false,0x1D)), KeyPressed(sc(false,0x2E))]);
    assert_eq!(t.key_up(0x08), vec![KeyReleased(sc(false,0x2E))]);
    assert_eq!(t.modifier_changed(KC_CMD_L, false), vec![KeyReleased(sc(false,0x1D))]);
}
#[test] fn cmd_with_two_keys_presses_ctrl_once() { /* ⌘ down, C down/up, V down → only one KeyPressed(1D) total */ }
#[test] fn tapping_cmd_alone_sends_windows_key() {
    let mut t = KeyTranslator::new();
    t.modifier_changed(KC_CMD_R, true);
    assert_eq!(t.modifier_changed(KC_CMD_R, false), vec![KeyPressed(sc(true,0x5B)), KeyReleased(sc(true,0x5B))]);
}
#[test] fn option_maps_to_alt() { /* modifier_changed(KC_OPT_L,true) == [KeyPressed(38)] */ }
#[test] fn right_option_is_extended_alt() { /* KC_OPT_R → sc(true,0x38) */ }
#[test] fn caps_lock_change_sends_press_and_release() { /* either direction → [KeyPressed(3A), KeyReleased(3A)] */ }
#[test] fn reset_after_focus_loss_forgets_cmd() {
    let mut t = KeyTranslator::new();
    t.modifier_changed(KC_CMD_L, true); t.reset();
    assert!(t.modifier_changed(KC_CMD_L, false).is_empty()); // no stray Windows key
}
#[test] fn ctrl_alt_del_sequence() { /* 6 ops: 1D,38,E0 53 pressed then released in reverse */ }
```

- [ ] **Step 2: Run them to verify they fail.** Run: `cd macos && cargo test --lib keymap`. Expected: FAIL (items undefined).
- [ ] **Step 3: Implement `keymap.rs`** with a `match` on the table above. `KeyTranslator` holds `cmd_down: bool, cmd_used: bool, ctrl_injected: bool`.
- [ ] **Step 4: Run them to verify they pass.** Run: `cd macos && cargo test --lib keymap`. Expected: PASS.
- [ ] **Step 5: Commit.** `git add macos/src && git commit -m "macos: mac keycode to RDP scancode mapping with Cmd translation"`

### Task 3: Framebuffer and view geometry

**Files:**
- Create: `macos/src/session/framebuffer.rs`, `macos/src/session/geometry.rs`
- Modify: `macos/src/session/mod.rs`

**Interfaces:**
- Produces (`session::framebuffer`):
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct Region { pub left: u16, pub top: u16, pub right: u16, pub bottom: u16 }` (inclusive)
  - `pub struct Framebuffer` (`width: u16, height: u16, pixels: Vec<u32>` holding `0x00RRGGBB`, plus `dirty: bool`)
  - `impl Framebuffer { pub fn new() -> Self; pub fn width(&self) -> u16; pub fn height(&self) -> u16; pub fn pixels(&self) -> &[u32]; pub fn apply(&mut self, width: u16, height: u16, region: Region, packed: &[u32]) -> bool; pub fn replace(&mut self, width: u16, height: u16, pixels: Vec<u32>) -> bool; pub fn take_dirty(&mut self) -> bool }`
  - `apply` and `replace` return `true` only if the buffer was clean before the call. The caller emits one redraw notification per clean→dirty transition. If `width`/`height` differ from the current size, `apply` reallocates (black) first. Regions are clipped to the framebuffer. If `packed` is shorter than the region, only the rows it covers are copied.
- Produces (`session::geometry`):
  - `#[derive(Clone, Copy, Debug, PartialEq)] pub struct ViewMapping { pub view_w: f64, pub view_h: f64, pub src: Region }`. `src` is the framebuffer region this view shows: the whole framebuffer for single-display sessions, or one monitor's region for multi-display.
  - `pub fn view_to_remote(m: &ViewMapping, x: f64, y_from_top: f64) -> (u16, u16)`: scales the view point into `src` and clamps it inside `src`.
  - `pub fn contents_rect_top_left(m: &ViewMapping, fb_w: u16, fb_h: u16) -> (f64, f64, f64, f64)`: `src` as unit coordinates `(x, y, w, h)` with a top-left origin.
  - `pub struct ScrollAccumulator`, `impl ScrollAccumulator { pub fn new() -> Self; pub fn push(&mut self, delta: f64, precise: bool) -> i16 }`. Non-precise: `round(delta * 120)`. Precise (trackpad): accumulate `delta * 10.0`, emit the whole part and keep the remainder. The result is clamped to `i16`.

- [ ] **Step 1: Write the failing tests.**

```rust
// framebuffer
#[test] fn apply_blits_region() { /* new; apply(4,4, Region{1,1,2,2}, &[1,2,3,4]) → pixels[5]==1, [6]==2, [9]==3, [10]==4, others 0 */ }
#[test] fn apply_resizes_and_clears() { /* 2x2 filled, then apply(3,3, ...) → len 9, untouched pixels 0 */ }
#[test] fn apply_reports_clean_to_dirty_once() { /* apply→true, apply→false, take_dirty()→true, apply→true */ }
#[test] fn region_outside_framebuffer_is_clipped() { /* apply(2,2, Region{1,1,5,5}, &[7;25]) does not panic; pixels[3]==7 */ }
// geometry
#[test] fn view_to_remote_scales() { let m = ViewMapping{view_w:640.,view_h:400.,src:Region{left:0,top:0,right:1279,bottom:799}}; assert_eq!(view_to_remote(&m,320.,200.),(640,400)); }
#[test] fn view_to_remote_offsets_into_monitor_region() { /* src left=1920 → (0,0) maps to (1920,0) */ }
#[test] fn view_to_remote_clamps() { /* (-5, 9999) → (src.left, src.bottom) */ }
#[test] fn contents_rect_for_right_half() { /* fb 3840x1200, src left=1920..3839 → (0.5,0.0,0.5,1.0) */ }
#[test] fn wheel_notch_is_120_units() { assert_eq!(ScrollAccumulator::new().push(1.0,false),120); }
#[test] fn precise_deltas_accumulate_fractional() { let mut s=ScrollAccumulator::new(); assert_eq!(s.push(0.05,true),0); assert_eq!(s.push(0.05,true),1); }
```

- [ ] **Step 2: Run them to verify they fail.** Run: `cd macos && cargo test --lib session::framebuffer session::geometry`. Expected: FAIL.
- [ ] **Step 3: Implement both modules.**
- [ ] **Step 4: Run them to verify they pass.** Same command. Expected: PASS.
- [ ] **Step 5: Commit.** `git add macos/src && git commit -m "macos: framebuffer and view/remote geometry"`

### Task 4: Monitor layout math

**Files:**
- Create: `macos/src/monitors.rs`. Modify: `macos/src/lib.rs`

**Interfaces:**
- Consumes: `session::framebuffer::Region`
- Produces (`rdp_launcher::monitors`):
  - `#[derive(Clone, Copy, Debug, PartialEq)] pub struct Rect { pub x: f64, pub y: f64, pub w: f64, pub h: f64 }`: AppKit global coordinates (bottom-left origin), in points.
  - `#[derive(Clone, Debug, PartialEq)] pub struct DisplayInfo { pub key: String, pub name: String, pub frame: Rect, pub visible_frame: Rect, pub is_main: bool }`
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct RemoteMonitor { pub left: i32, pub top: i32, pub width: u32, pub height: u32, pub primary: bool }`
  - `pub fn resolve_selection(saved: &[String], available: &[DisplayInfo]) -> Vec<DisplayInfo>`: saved order, skipping disconnected displays. If the result is empty, use the main display (or the first display if none is main).
  - `pub fn remote_layout(displays: &[DisplayInfo]) -> Vec<RemoteMonitor>`: `displays[0]` is primary at `(0,0)`. Others are placed relative to it with Y flipped: `left = round(d.x - p.x)`, `top = round((p.y + p.h) - (d.y + d.h))`. `width = even_size(round(w))`, `height = clamp(round(h), 200, 8192)`. If any monitor shares no edge (within 1 pt) with another, re-pack left to right in `frame.x` order at `top = 0`, then shift so the primary sits at `left = 0`.
  - `pub fn even_size(w: u32) -> u32`: clamp to 200..=8192 and round down to even.
  - `pub fn virtual_desktop(layout: &[RemoteMonitor]) -> (i32, i32, u32, u32)`: the bounding box as `(left, top, width, height)`.
  - `pub fn monitor_region(layout: &[RemoteMonitor], index: usize) -> Region`: that monitor in framebuffer coordinates (the framebuffer origin is the bounding box's top-left).

- [ ] **Step 1: Write the failing tests.** Use these helpers: `ext(key, x, y, w, h)` builds a `DisplayInfo`, and `main_display()` returns the built-in screen `frame (0,0,1512,982)`, key `"builtin"`.

```rust
#[test] fn single_display_is_primary_at_origin() { /* [ext(1920x1200)] → [RemoteMonitor{0,0,1920,1200,true}] */ }
#[test] fn two_side_by_side_externals() { /* A(0,0,1920,1200), B(1920,0,1920,1200) → B.left==1920, B.top==0; virtual_desktop==(0,0,3840,1200) */ }
#[test] fn display_above_primary_gets_negative_top() { /* P(0,0,1512,982), A(0,982,1920,1200) → A.top == -1200 */ }
#[test] fn display_left_of_primary_gets_negative_left() { /* A.x = -1920 → left == -1920; monitor_region(layout,0).left == 1920 */ }
#[test] fn odd_width_is_rounded_down_to_even() { assert_eq!(even_size(1513), 1512); }
#[test] fn non_adjacent_selection_is_packed_side_by_side() { /* A(0,0,1920,1200), C(3432,0,1920,1200) (gap) → C.left==1920 */ }
#[test] fn unplugged_selection_falls_back_to_main() { /* saved ["dell-1"], available [main_display()] → [main_display()] */ }
#[test] fn resolve_selection_keeps_saved_order() { /* saved ["b","a"] → [b, a] */ }
```

- [ ] **Step 2: Run them to verify they fail.** Run: `cd macos && cargo test --lib monitors`. Expected: FAIL.
- [ ] **Step 3: Implement `monitors.rs`.**
- [ ] **Step 4: Run them to verify they pass.** Expected: PASS.
- [ ] **Step 5: Commit.** `git commit -m "macos: display selection to RDP monitor layout"`

### Task 5: Certificate fingerprints and trust decisions

**Files:**
- Create: `macos/src/certs.rs`. Modify: `macos/src/lib.rs`

**Interfaces:**
- Produces (`rdp_launcher::certs`):
  - `pub fn fingerprint(der: &[u8]) -> String`: `"sha256:"` followed by 64 lowercase hex characters.
  - `pub fn display_fingerprint(fp: &str) -> String`: uppercase hex pairs joined by `:`, without the prefix.
  - `#[derive(Debug, PartialEq)] pub enum Trust { Trusted, FirstUse, Changed { previous: String } }`
  - `pub fn check(stored: Option<&str>, presented: &str) -> Trust`

- [ ] **Step 1: Write the failing tests.**

```rust
#[test] fn fingerprint_is_sha256_hex() { assert_eq!(fingerprint(b"abc"), "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"); }
#[test] fn display_fingerprint_groups_pairs() { assert!(display_fingerprint(&fingerprint(b"abc")).starts_with("BA:78:16:BF")); }
#[test] fn trust_decisions() {
    assert_eq!(check(None, "sha256:aa"), Trust::FirstUse);
    assert_eq!(check(Some("sha256:aa"), "sha256:aa"), Trust::Trusted);
    assert_eq!(check(Some("sha256:aa"), "sha256:bb"), Trust::Changed { previous: "sha256:aa".into() });
}
```

- [ ] **Step 2: Run them to verify they fail.** Run: `cd macos && cargo test --lib certs`. Expected: FAIL.
- [ ] **Step 3: Implement it with `sha2::Sha256`.**
- [ ] **Step 4: Run them to verify they pass.** Expected: PASS.
- [ ] **Step 5: Commit.** `git commit -m "macos: certificate fingerprint trust-on-first-use logic"`

### Task 6: Session engine with end-to-end tests

**Files:**
- Create: `macos/src/session/engine.rs`, `macos/tests/engine_e2e.rs`, `macos/tests/certs/server-cert.pem`, `macos/tests/certs/server-key.pem`
- Modify: `macos/src/session/mod.rs`

**Interfaces:**
- Consumes:
  - `ironrdp_client::rdp::{RdpClient, RdpInputEvent, RdpInputSender, RdpOutputEvent}` and `ironrdp_client::output_channel::output_channel`
  - `ironrdp_client::config::{ConfigBuilder, Destination, ClipboardType}` and `ironrdp_tls::CertificateValidation`
  - `ironrdp_input::{Database, Operation}`
  - `framebuffer::{Framebuffer, Region}`, `monitors::{RemoteMonitor, virtual_desktop}` and `certs::{fingerprint, check, Trust}`
- Produces (`session::engine`):
  - `pub struct SessionRequest { pub host: String, pub port: u16, pub username: String, pub domain: Option<String>, pub password: String, pub trusted_fingerprint: Option<String>, pub layout: Vec<RemoteMonitor>, pub sound: bool, pub shared_folder: Option<std::path::PathBuf>, pub clipboard: Option<Box<dyn ironrdp_cliprdr::backend::CliprdrBackendFactory + Send>> }`. Implement `Debug` by hand and redact the password.
  - `pub enum PointerUpdate { Default, Hidden, Bitmap(std::sync::Arc<ironrdp_graphics::pointer::DecodedPointer>) }`
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum FailureKind { BadCredentials, Unreachable, Other }`
  - `pub enum SessionEvent { Connected, FrameReady, Pointer(PointerUpdate), CertificateUntrusted { fingerprint: String, previous: Option<String> }, Failed { kind: FailureKind, message: String }, Ended }`. `Ended` means a graceful end: logoff, a server disconnect, or the user disconnecting.
  - `pub struct SessionHandle` (cloneable: `Arc` inner)
  - `impl SessionHandle { pub fn send_operations(&self, ops: Vec<Operation>); pub fn release_all(&self); pub fn resize(&self, width: u16, height: u16); pub fn set_monitor_layout(&self, layout: &[RemoteMonitor]); pub fn input_sender(&self) -> RdpInputSender; pub fn disconnect(&self); pub fn framebuffer(&self) -> std::sync::Arc<std::sync::Mutex<Framebuffer>> }`
  - `pub fn start_session(rt: &tokio::runtime::Handle, req: SessionRequest, on_event: Box<dyn Fn(SessionEvent) + Send + Sync>) -> SessionHandle`
  - `pub fn classify_failure(err: &ironrdp_connector::ConnectorError) -> FailureKind`:
    - `BadCredentials` for `ConnectorErrorKind::Credssp(_)` or `AccessDenied`.
    - `Unreachable` when the error's `Display`/source chain contains `"TCP connect"` or an `io::Error` of kind `ConnectionRefused`, `TimedOut`, `HostUnreachable` or `NetworkUnreachable`.
    - Otherwise `Other`.

How `start_session` works:
- Build the config with `ConfigBuilder`, applying every setting in Global Constraints plus `with_sound(req.sound)`, `with_clipboard(if req.clipboard.is_some() { ClipboardType::Enable } else { ClipboardType::Disable })` and `with_certificate_validation_callback(cb)`, where `cb` is an `ironrdp_tls::CertificateValidationCallback`, i.e. `Arc<dyn Fn(&[u8] /*leaf DER*/, &str, &str) -> bool + Send + Sync>`. The desktop size is `virtual_desktop(&layout)`. When `layout.len() > 1`, also call `with_monitor_layout` (a `ClientMonitorData` built from `layout`).
- The certificate callback stores `fingerprint(der)` in an `Arc<Mutex<Option<String>>>` slot and returns `check(trusted.as_deref(), &fp) == Trust::Trusted`.
- Create `RdpClient::new(config, tx).with_desktop_updates()`. Add `.with_cliprdr_backend_factory(f)` when `clipboard` is `Some`. Add `.with_rdpdr_backend_factory(drive::factory(path))` from Task 15; until then, pass `with_rdpdr(false)`.
- Spawn `run()`, then spawn a pump task that maps `RdpOutputEvent`s:
  - `DesktopUpdate` → `framebuffer.apply(...)`, and `FrameReady` if it returned true.
  - `Image` → `replace`.
  - `Connected` → `Connected`.
  - Pointer events → `Pointer`.
  - `ConnectionFailure(e)` → `CertificateUntrusted` if the slot holds a fingerprint that isn't trusted. Otherwise `Failed { kind: classify_failure(&e), message: e.to_string() }`.
  - `Terminated(Ok(_))` → `Ended`. `Terminated(Err(e))` → `Failed`.
  - Ignore everything else.
- Read `crates/ironrdp-client/src/rdp.rs` and `config.rs` at the pinned rev for exact signatures (`output_channel(capacity)`, the `OutputEventReceiver` receive method, the `with_certificate_validation_callback` argument type).
- `send_operations`: lock a `Mutex<Database>`, run `apply(ops)`, and send the result as `RdpInputEvent::FastPath` via `try_send`. `release_all` uses `Database::release_all()`.

- [ ] **Step 1: Copy the test certificates.** Copy `server-cert.pem` and `server-key.pem` from `~/.cargo/git/checkouts/ironrdp-*/da016ce/crates/ironrdp-testsuite-extra/tests/certs/` to `macos/tests/certs/`, and add a line to `macos/tests/certs/README.md` giving their source and licence (MIT/Apache-2.0, IronRDP).
- [ ] **Step 2: Write the failing e2e tests in `macos/tests/engine_e2e.rs`.**
  - The server helper `spawn_server(display: (u16, u16), color: u32) -> (SocketAddr, ServerControl)` builds an `ironrdp_server::RdpServer` on `127.0.0.1:0` with TLS from `tests/certs`. Mirror `crates/ironrdp-testsuite-extra/tests/e2e.rs` at the pinned rev for the builder, TLS acceptor and display handler.
  - Its display handler sends one full-screen bitmap filled with `color`. Its input handler forwards keyboard events to an `mpsc` channel exposed by `ServerControl`, and `ServerControl::stop()` drops the server.
  - The client side uses `start_session` with an `on_event` that pushes into a `tokio::sync::mpsc::unbounded_channel`. Wait with a helper that applies a 10 s timeout per event.

```rust
#[tokio::test(flavor = "multi_thread")] async fn first_connection_reports_untrusted_certificate() {
    // trusted_fingerprint: None → CertificateUntrusted { fingerprint == certs::fingerprint(der_of("tests/certs/server-cert.pem")), previous: None }
}
#[tokio::test(flavor = "multi_thread")] async fn changed_certificate_reports_previous() {
    // trusted_fingerprint: Some("sha256:00…00") → CertificateUntrusted { previous: Some("sha256:00…00"), .. }
}
#[tokio::test(flavor = "multi_thread")] async fn trusted_certificate_connects_and_receives_frame() {
    // trusted = real fingerprint; server color 0x00FF0000 → Connected, then FrameReady; framebuffer().lock().pixels()[0] == 0x00FF0000
}
#[tokio::test(flavor = "multi_thread")] async fn keyboard_input_reaches_server() {
    // after Connected: send_operations(vec![KeyPressed(Scancode::from_u8(false,0x1E))]) → server sees key 0x1E pressed
}
#[tokio::test(flavor = "multi_thread")] async fn release_all_releases_held_key() {
    // press 0x1E, then release_all() → server sees key 0x1E released
}
#[tokio::test(flavor = "multi_thread")] async fn closed_port_fails_as_unreachable() {
    // bind+drop a TcpListener to get a free port → Failed { kind: Unreachable, .. }
}
#[tokio::test(flavor = "multi_thread")] async fn server_disconnect_reports_end() {
    // after Connected, ServerControl::stop() → exactly one of Ended | Failed within 10 s; no panic
}
```

- [ ] **Step 3: Run them to verify they fail.** Run: `cd macos && cargo test --test engine_e2e`. Expected: FAIL to compile (`start_session` is missing).
- [ ] **Step 4: Implement `engine.rs`** as described above.
- [ ] **Step 5: Run them to verify they pass.** Run: `cd macos && cargo test --test engine_e2e`. Expected: 7 passed. If the test server cannot send a bitmap through the public API, assert `Connected` plus a non-empty framebuffer instead, and note this in the commit message.
- [ ] **Step 6: Commit.** `git add macos && git commit -m "macos: IronRDP session engine with TOFU certificates and e2e tests"`

### Task 7: First session window (spike checkpoint)

**Files:**
- Create: `macos/src/main.rs`, `macos/src/app.rs`, `macos/src/logging.rs`, `macos/src/session/view.rs`, `macos/src/session/window.rs`
- Modify: `macos/src/lib.rs`, `macos/src/session/mod.rs`

**Interfaces:**
- Consumes: `engine::{start_session, SessionHandle, SessionEvent, PointerUpdate}`, `keymap::KeyTranslator`, `geometry::{ViewMapping, view_to_remote, contents_rect_top_left, ScrollAccumulator}`
- Produces:
  - `logging::init()`: tracing to the log file, creating the directory if needed.
  - `app::run()`: builds `NSApplication`, sets `NSApplicationActivationPolicy::Regular`, installs `AppDelegate`, builds the main menu (App menu with About/Quit; Window menu with `setWindowsMenu`), then runs.
  - `app::runtime() -> &'static tokio::runtime::Runtime`: a lazily built multi-thread runtime.
  - `view::RdpView`: a `define_class!` subclass of `NSView`, `MainThreadOnly`, with ivars `handle: SessionHandle`, `translator: RefCell<KeyTranslator>`, `mapping: Cell<ViewMapping>`, `scroll: RefCell<ScrollAccumulator>`. Constructor: `RdpView::new(mtm, handle, mapping) -> Retained<RdpView>`. Method: `RdpView::present(&self, image: &CGImage, fb_w: u16, fb_h: u16)`.
  - `window::SessionWindows`: owns one or more `(NSWindow, RdpView)`. Constructor: `SessionWindows::open_windowed(mtm, title: &str, handle: SessionHandle) -> Rc<SessionWindows>`. Method: `present_frame(&self)`, which locks the framebuffer, calls `take_dirty`, builds one `CGImage` and calls `present` on every view.
  - `window::make_cg_image(fb: &Framebuffer) -> Option<CFRetained<CGImage>>`: copies the pixels into a `CGDataProvider`, using `kCGBitmapByteOrder32Little | kCGImageAlphaNoneSkipFirst` because `0x00RRGGBB` is stored little-endian as B,G,R,X.

RdpView behaviour:
- **Basics:** `isFlipped` returns YES, `acceptsFirstResponder` returns YES, and the view is layer-backed with `contentsGravity = kCAGravityResize`.
- **Keys:** `keyDown:` and `keyUp:` go through `translator` and then `handle.send_operations`. `flagsChanged:` reads `keyCode` and derives `pressed` from the matching `NSEventModifierFlags` bit (for Caps Lock, any change).
- **Shortcuts:** `performKeyEquivalent:` returns NO for ⌘Q and ⌃⌘F so the menus get them. Every other ⌘ combination is handled as a key and returns YES.
- **Mouse:** left/right/other down/up/dragged and `mouseMoved:` (tracking area with `NSTrackingMouseMoved | ActiveInKeyWindow | InVisibleRect`) map through `view_to_remote` to `MouseMove` plus button operations. `scrollWheel:` goes through the accumulator into `WheelRotations` (vertical, and horizontal from `scrollingDeltaX`).
- **Focus loss:** `resignFirstResponder` and window resign-key call `handle.release_all()` and `translator.reset()`.
- **Cursor:** `Pointer(Hidden)` hides the cursor inside the view (empty `NSCursor`). `Default` gives the arrow. `Bitmap` builds an `NSCursor` from the RGBA pointer image, using its hotspot.

Dev entry point, for debug builds only (`#[cfg(debug_assertions)]`):
- If `RDPL_DEV_HOST` is set, `app.rs` starts a windowed session at 1280×800 with `RDPL_DEV_USER`/`RDPL_DEV_PASS`, sound on, and the clipboard off.
- On `CertificateUntrusted`, it logs the fingerprint and restarts the session with that fingerprint trusted.
- Every `SessionEvent` hops to the main thread with `dispatch2::DispatchQueue::main().exec_async`.
- Task 12 deletes this path.

- [ ] **Step 1: Implement the files above.** Build: `cd macos && cargo build`. Expected: it compiles without warnings from our crate.
- [ ] **Step 2: Smoke-run without a server.** Run: `cd macos && RDPL_DEV_HOST=127.0.0.1:1 cargo run`. Expected: a window titled "127.0.0.1:1" opens, and the log file contains a `Failed { kind: Unreachable }` line within about 10 s. The app stays alive.
- [ ] **Step 3: User checkpoint against a real server.** Ask the user to run `cd macos && RDPL_DEV_HOST=<host[:port]> RDPL_DEV_USER=<user> RDPL_DEV_PASS='<pw>' cargo run --release` against one of their servers. Expected:
  - (a) the desktop renders within about 10 s
  - (b) mouse movement, clicks, right-click and scroll work
  - (c) typing works, ⌘C/⌘V act as Ctrl+C/Ctrl+V, and tapping ⌘ opens Start
  - (d) a remote sound plays on the Mac
  - (e) closing the window leaves the app running with nothing in the log beyond `Ended`

  Record the outcome in the commit message. If (d) fails, open a fix task before Phase 2: replace sound with our own `rdpsnd` handler on `cpal`, attached via `ConfigBuilder::with_static_channel`.
- [ ] **Step 4: Commit.** `git add macos && git commit -m "macos: first AppKit session window (spike verified: <results>)"`

---

## Phase 2 — Launcher parity

### Task 8: Keychain

**Files:** Create `macos/src/keychain.rs`. Modify `macos/src/lib.rs`.

**Interfaces:**
- Produces: `pub struct Keychain { service: String }`, `#[derive(Debug)] pub struct KeychainError(pub String)` (implements `Display`)
- `impl Keychain { pub fn new(service: &str) -> Self; pub fn app() -> Self /* "RDP Launcher" */; pub fn get(&self, id: &str) -> Option<String>; pub fn set(&self, id: &str, password: &str) -> Result<(), KeychainError>; pub fn delete(&self, id: &str); pub fn copy(&self, from: &str, to: &str) -> Result<(), KeychainError> }`. `set` with an empty password deletes the entry.

- [ ] **Step 1: Write the failing tests.** Use service `"RDP Launcher Tests"` and `uuid` account ids, and delete the entries at the end of every test.
```rust
#[test] fn set_get_delete_round_trip() { /* set "p@ss wörd" → get == Some; delete → get == None */ }
#[test] fn set_empty_deletes() { /* set "x" then set "" → get == None */ }
#[test] fn copy_duplicates_entry() { /* set a; copy(a,b) → get(b) == get(a) */ }
#[test] fn get_missing_is_none() { /* fresh id → None */ }
```
- [ ] **Step 2: Run them to verify they fail.** Run: `cd macos && cargo test --lib keychain`. Expected: FAIL.
- [ ] **Step 3: Implement it** with `security_framework::passwords::{get_generic_password, set_generic_password, delete_generic_password}`.
- [ ] **Step 4: Run them to verify they pass.** Expected: PASS, with no Keychain dialog (the binary created the items).
- [ ] **Step 5: Commit.** `git commit -m "macos: Keychain password storage"`

### Task 9: Launcher window and connection list

**Files:**
- Create: `macos/src/launcher/{mod.rs, geometry.rs, palette.rs, list_view.rs}`
- Modify: `macos/src/lib.rs`, `macos/src/app.rs`

**Interfaces:**
- Produces (`launcher::geometry`, pure; flipped coordinates with y downward and the origin at the content view's top-left):
  - Constants from Global Constraints.
  - `pub struct R { pub x: f64, pub y: f64, pub w: f64, pub h: f64 }` with `contains(px, py)`
  - `pub fn item_rect(i: usize, scroll: f64, width: f64) -> R`: `x = MARGIN`, `y = HEADER_H + MARGIN + i*(BTN_H+GAP) - scroll`, `w = width - 2*MARGIN`, `h = BTN_H`.
  - `pub fn icon_rects(item: &R) -> [R; 3]` (edit, duplicate, delete): `right_edge = item.right - HANDLE_W - 2`, `left_k = right_edge - (3-k)*20`, `y = item.bottom - 21`, `w = 18`, `h = 18`.
  - `pub fn grip_rect(item: &R) -> R`: the rightmost `HANDLE_W` of the item.
  - `pub enum Zone { Body, Edit, Duplicate, Delete, Grip }`, `pub enum Hit { Item(usize, Zone), None }`
  - `pub fn hit_test(px: f64, py: f64, n: usize, scroll: f64, width: f64) -> Hit`
  - `pub fn content_height(n: usize) -> f64 = HEADER_H + MARGIN + max(n,1)*(BTN_H+GAP) + STRIP_H`
  - `pub fn drop_index(drag_top_y: f64, n: usize, scroll: f64) -> usize`: the index whose slot centre is nearest `drag_top_y + BTN_H/2`, clamped to `0..n`.
  - `pub struct Spring { pub pos: f64, pub vel: f64 }`, `impl Spring { pub fn step(&mut self, target: f64) -> bool }`: `vel = (vel + (target-pos)*STIFFNESS)*DAMPING; pos += vel`. It returns true when settled (`|target-pos| <= 0.5 && |vel| <= 0.5`), snapping `pos = target, vel = 0`.
- Produces (`launcher::palette`): `pub struct ButtonColors { pub top: u32, pub bottom: u32, pub border: u32, pub text: u32, pub sub: u32, pub grip: u32 }` and `pub fn button_colors(connected: bool, pressed: bool, hot: bool) -> ButtonColors`, using the Windows values:
  - connected: `top DFF6DF`, `bottom 9FDD9F`, `border 4C9A4C`, `text 0F3D0F`, `sub 2E6B2E`, `grip 4C9A4C`
  - connected + pressed: `top B8E6B8`, `bottom 7CC77C`, `border 3E8E3E`, otherwise the same as connected
  - pressed: `top C9D3E4`, `bottom AEBBD2`, `border 5A6C8C`, `text 101A2B`, `sub 5A667A`, `grip 8A97AC`
  - hot: `top FFFFFF`, `bottom E3E9F3`, `border 6C7E9B`, otherwise the same as pressed
  - idle: `top F4F7FB`, `bottom D5DDEA`, `border 7A8AA5`, otherwise the same as pressed

  Fixed colors:
  - `WINDOW_BG EFF2F7`
  - `HEADER_BG E7ECF4`
  - `HEADER_LINE C5CEDC`
  - `HEADER_TEXT 445066`
  - `ICON_DARK 2B3A55`
  - `PIN_ON 0B63CE`
  - `PIN_OFF 5A667A`
  - flat button states: hot `DDE5F1`, pressed `BFCBDE`
- Produces (`launcher`):
  - `pub enum ItemStatus { Idle, Connecting, Connected }`
  - `pub struct LauncherCallbacks { pub connect: Box<dyn Fn(usize)>, pub edit: Box<dyn Fn(usize)>, pub duplicate: Box<dyn Fn(usize)>, pub delete: Box<dyn Fn(usize)>, pub add: Box<dyn Fn()>, pub reordered: Box<dyn Fn(usize, usize)> }`
  - `pub struct LauncherController`, constructed with `LauncherController::new(mtm, callbacks, always_on_top: bool) -> Rc<LauncherController>`
  - Methods: `set_items(&self, items: Vec<(String /*name*/, String /*host*/, ItemStatus)>)`, `set_status(&self, index: usize, s: ItemStatus)`, `show(&self)`, `hide(&self)`, `toggle(&self)`, `is_visible(&self) -> bool`, `set_always_on_top(&self, on: bool)`, `window(&self) -> &NSWindow`

Behaviour (`list_view.rs`, `ConnectionListView`, flipped `NSView` subclass):
- **Drawing:** each item is a rounded rect (radius 3) with a vertical `NSGradient` from `top` to `bottom` and a 1 pt `border`. The name is bold system font 14 pt at `(x+10, y+8)`, truncated with an ellipsis before the grip. The subtitle is the host in 11 pt (`"{host}  •  connected"` when connected). A ≡ grip is three 2 pt lines in `grip` color, centred in the grip rect. SF Symbol icons `pencil`, `doc.on.doc` and `trash` go in `icon_rects`.
- **Interaction:**
  - Hover is tracked with `mouseMoved` and drives `hot`.
  - A click in `Body` calls `connect`. A click on an icon calls `edit`, `duplicate` or `delete`.
  - `mouseDown` in `Grip` starts a drag: the item follows the cursor and the others spring toward the order implied by `drop_index` on a 16 ms `NSTimer`. On mouse up, `reordered(from, to)` fires.
  - Right-click opens an `NSMenu` with Connect, Edit / Rename…, Duplicate, Delete (with SF Symbol images).
  - The scroll wheel scrolls silently when content is taller than the window.
- **Tooltips:** register per-zone rects with `addToolTipRect:owner:userData:`, rebuilt on every layout. Text matches Windows:
  - body: `"Click to connect to {name}\nRight-click for more options"`
  - edit: `"Edit this connection (name, host, username, password)"`
  - duplicate: `"Duplicate this connection"`
  - delete: `"Delete this connection"`
  - grip: `"Drag up or down to reorder the list"`
  - plus: `"Add a new RDP connection"`
  - minimize: `"Hide the launcher\nClick the menu-bar icon to bring it back"`
  - info: `"About RDP Launcher and user manual"`
  - pin: `"Always on top: ON\nThe window stays above other windows.\nClick to let other windows cover it."` or `"…OFF\nOther windows can cover this one, like a normal app.\nClick to keep it above everything."`
- **Window:** titled, closable and miniaturizable, not resizable. Content width is `BTN_W + 2*MARGIN`; height is `min(content_height(n), 0.85 * visibleFrame.height)`, resizing while keeping the top edge fixed.
  - The header strip (`HEADER_BG`, bottom line `HEADER_LINE`) shows the count label in `HEADER_TEXT`: `"No connections yet - click + to add one"` / `"1 connection"` / `"{n} connections"`.
  - At the right of the header sit three `HDR_BTN` flat buttons: minimize (`minus`) hides, pin (`pin.fill`/`pin.slash`) toggles, info (`info.circle`).
  - The **+** button (`plus`, `PLUS_SIZE`) sits bottom-right in the `STRIP_H` strip.
  - The close and minimize traffic lights hide the window (`windowShouldClose` returns NO and calls `hide`).
  - Always on top is `NSFloatingWindowLevel`.

- [ ] **Step 1: Write failing tests for `launcher::geometry`.**
```rust
#[test] fn item_rect_first_item() { let r = item_rect(0, 0., 324.); assert_eq!((r.x,r.y,r.w,r.h),(12.,44.,300.,56.)); }
#[test] fn icons_sit_left_of_grip() { let r=item_rect(0,0.,324.); let ic=icon_rects(&r); assert_eq!(ic[2].x, 12.+300.-34.-2.-20.); assert_eq!(ic[0].y, 44.+56.-21.); }
#[test] fn hit_test_zones() { /* point in grip → Hit::Item(0, Zone::Grip); in icon 1 → Duplicate; between items → None */ }
#[test] fn content_height_empty_list_reserves_one_row() { assert_eq!(content_height(0), 32.+12.+64.+44.); }
#[test] fn drop_index_rounds_to_nearest_slot() { /* drag_top at slot 2's y → 2; far below → n-1; above → 0 */ }
#[test] fn spring_settles_on_target() { let mut s=Spring{pos:0.,vel:0.}; let mut n=0; while !s.step(100.) { n+=1; assert!(n<200); } assert_eq!(s.pos,100.); }
#[test] fn spring_overshoots_slightly() { /* max pos during settle > 100.0 (bouncy, as on Windows) */ }
```
- [ ] **Step 2: Run them to verify they fail.** Run: `cd macos && cargo test --lib launcher::geometry`. Expected: FAIL.
- [ ] **Step 3: Implement `geometry.rs`, `palette.rs`, `list_view.rs` and `mod.rs`.** In `app.rs`, create the `LauncherController` at launch with `Store` data: `connect` logs for now, `add`/`edit` are stubs until Task 10, and `duplicate`, `delete` (with an `NSAlert` reading *Delete "Name"?*, Yes/No) and `reordered` update the `Store` and `Keychain` (`copy`/`delete`).
- [ ] **Step 4: Run the tests.** Run: `cd macos && cargo test --lib launcher`. Expected: PASS.
- [ ] **Step 5: Manual check.** Seed `~/Library/Application Support/RdpLauncher/connections.json` with the Windows sample of four connections (Pi5-16GB, Pi4-4GB, Media Server, Work Desktop), then run `cd macos && cargo run`. Expected:
  - the launcher matches the Windows screenshot's layout and colors
  - hover highlights
  - duplicate inserts "(copy)" below
  - delete asks first
  - dragging the grip springs the other items with a small bounce, and the order persists after a restart
  - pin toggles floating
  - the close button hides the window
  - tooltips appear

  Take a screenshot with `screencapture -l<windowid>` into the scratchpad and view it.
- [ ] **Step 6: Commit.** `git commit -m "macos: launcher window with animated, reorderable connection list"`

### Task 10: Edit dialog and password prompt

**Files:** Create `macos/src/launcher/edit_dialog.rs`, `macos/src/launcher/password_prompt.rs`, `macos/src/displays.rs`. Modify `macos/src/app.rs` and `macos/src/lib.rs`.

**Interfaces:**
- Produces:
  - `displays::current_displays(mtm) -> Vec<monitors::DisplayInfo>`. This is `NSScreen::screens`. The key is `"{vendor:x}-{model:x}-{serial:x}"` from `CGDisplayVendorNumber`/`ModelNumber`/`SerialNumber` of `deviceDescription["NSScreenNumber"]`, falling back to `localizedName`. `name = localizedName`. `frame`/`visible_frame` come from `NSScreen`. `is_main` is true for `screens[0]` (the menu-bar screen).
  - `edit_dialog::show(mtm, title: &str, initial: &Connection, has_password: bool, displays: &[DisplayInfo]) -> Option<EditResult>`
  - `pub struct EditResult { pub connection: Connection, pub password: PasswordChange }` and `pub enum PasswordChange { Keep, Set(String), Clear }`
  - `password_prompt::ask(mtm, parent: Option<&NSWindow>, connection_name: &str, username: &str) -> Option<(String, bool /*remember*/)>`

Edit dialog (an `NSPanel` run with `NSApp.runModalForWindow`). Field labels match Windows:
- `Name`
- `Host / IP  (optionally host:port)`
- `Username  (DOMAIN\user or user)`
- `Password`: `NSSecureTextField`, with placeholder "Saved — leave blank to keep" when `has_password`. A small "Clear saved password" checkbox appears only when `has_password`.
- `Display`: `NSPopUpButton` with Full screen / Windowed.
- `Monitors`: one checkbox per display, labelled `"{name} — {w}×{h}"`, ticked when its key is in `monitors` (or the main display when `monitors` is empty). The section is disabled when Display is Windowed.
- `Shared folder`: `NSPopUpButton` with None / the current path / Choose… (`NSOpenPanel`, directories only).
- `Sound`: checkbox.
- Save and Cancel buttons. Save validates with `parse_host` and shows the `HostError` message inline in red without closing. Storing `monitors` is empty when only the main display is ticked.

Password prompt (`NSAlert` with an accessory view): `"Password for {username} on {name}"`, a secure field, a "Remember in Keychain" checkbox (on by default), and Connect / Cancel.

- [ ] **Step 1: Implement the three modules** and wire **+** and edit in `app.rs`: save to `Store`, apply `PasswordChange` to the `Keychain`, and refresh the list.
- [ ] **Step 2: Build.** Run: `cd macos && cargo build`. Expected: OK.
- [ ] **Step 3: Manual check.** Run `cargo run`. Expected:
  - **+** opens "New Connection" with focus in Name
  - an empty host shows "Enter a host name or IP address."; `host:99999` shows the port message
  - saving adds the item
  - Edit pre-fills every field
  - the password field shows the "Saved…" placeholder after a password is set
  - **Keychain Access** shows an item with service "RDP Launcher" and account = the id
  - the Monitors section lists all three displays
  - Choose… picks a folder
- [ ] **Step 4: Commit.** `git commit -m "macos: connection edit dialog, display enumeration, password prompt"`

### Task 11: Menu-bar icon, About/manual, app lifecycle

**Files:** Create `macos/src/launcher/{status_item.rs, manual.rs, about.rs}`. Modify `macos/src/app.rs`.

**Interfaces:**
- Produces:
  - `manual::manual_text(version: &str) -> String`: the Windows manual ported to macOS. Same 8 sections and headings; replace mstsc/tray/DPAPI/`%APPDATA%` with built-in client / menu-bar icon / Keychain / `~/Library/Application Support/RdpLauncher`. Document full screen (Spaces, ⌃⌘F), Windowed, Monitors, Shared folder, Sound, the ⌘ key rules, the Session menu, the certificate prompt and the Local Network prompt.
  - `about::show(mtm)`: a singleton window with a monospaced, read-only, scrollable `NSTextView` and an OK button.
  - `status_item::install(mtm, on_toggle: Box<dyn Fn()>, on_about: Box<dyn Fn()>, on_quit: Box<dyn Fn()>) -> Retained<NSStatusItem>`: a template SF Symbol `display` button. `sendActionOn(LeftMouseUp | RightMouseUp)`; a left click calls `on_toggle`, and a right click (or ⌃-click) pops a menu with Open / About / Quit.
  - `AppDelegate`:
    - `applicationShouldHandleReopen` shows the launcher.
    - `applicationShouldTerminateAfterLastWindowClosed` returns NO.
    - `applicationShouldTerminate`: when `SessionManager::count() > 0` (stubbed as 0 until Task 12), it shows an `NSAlert` reading `"Disconnect {n} session(s) and quit?"` with Quit and Cancel.
    - The App menu has About RDP Launcher, Quit (⌘Q). The Edit menu has standard Cut/Copy/Paste/Select All, which the launcher dialogs need. The Window menu is standard.

- [ ] **Step 1: Write the failing test** `manual::tests::manual_mentions_version_and_sections`: the text contains `"RDP Launcher 1.0.0"`, `"1. NEW TO REMOTE DESKTOP?"`, `"8. GOOD PRACTICE"`, `"Keychain"` and `"menu-bar icon"`, and does not contain `"mstsc"` or `"%APPDATA%"`.
- [ ] **Step 2: Run it to verify it fails.** Run: `cd macos && cargo test --lib manual`. Expected: FAIL.
- [ ] **Step 3: Implement the modules and wire them in `app.rs`.**
- [ ] **Step 4: Run the tests and the app.** Run `cargo test --lib manual` (expected PASS), then `cargo run`. Expected:
  - the menu-bar icon toggles the launcher, and right-click shows Open/About/Quit
  - ⓘ and About open the manual
  - clicking the Dock icon re-shows a hidden launcher
  - the pin state survives a restart
- [ ] **Step 5: Commit.** `git commit -m "macos: menu-bar icon, About/manual window, app lifecycle"`

---

## Phase 3 — Sessions from the launcher

### Task 12: SessionManager, status glow, prompts and errors

**Files:** Modify `macos/src/session/mod.rs`, `macos/src/app.rs`, `macos/src/session/window.rs`. Create `macos/src/session/alerts.rs`.

**Interfaces:**
- Consumes: `engine::*`, `window::SessionWindows`, `keychain::Keychain`, `model::{Store, Connection}`, `password_prompt::ask`, `edit_dialog::show`, `launcher::{LauncherController, ItemStatus}`, `displays::current_displays`, `monitors::{resolve_selection, remote_layout}`
- Produces:
  - `pub struct SessionManager`. Constructor: `SessionManager::new(mtm, on_status: Box<dyn Fn(&str /*conn id*/, ItemStatus)>, on_edit_request: Box<dyn Fn(&str)>, on_trust: Box<dyn Fn(&str, String /*fingerprint*/)>) -> Rc<SessionManager>`
  - Methods: `connect(&self, conn: &Connection)`, `count(&self) -> usize`, `disconnect_all(&self)`, `is_open(&self, id: &str) -> bool`
  - `alerts::confirm_certificate(window: &NSWindow, conn_name: &str, fingerprint: &str, previous: Option<&str>, done: Box<dyn Fn(bool)>)`
  - `alerts::connection_failed(window: &NSWindow, conn_name: &str, kind: FailureKind, message: &str, done: Box<dyn Fn(FailureChoice)>)` with `pub enum FailureChoice { Reconnect, Edit, Close }`

How `connect` works:
1. **Already open:** if a session for `conn.id` exists, bring all its windows forward (`makeKeyAndOrderFront` plus `NSApp.activate`) and return. A session that is still connecting also ignores the click.
2. **Password:** get it from `Keychain::app()`. If it's missing and the username is non-empty, ask with `password_prompt::ask`; Cancel aborts, and Remember stores the password.
3. **Start:** compute the layout. Use `resolve_selection(&conn.monitors, &current_displays)` for full screen. For windowed, use a single 1280×800 `RemoteMonitor` clamped as in Global Constraints. Open the windows (Task 13 adds the modes; use `open_windowed` until then), build a `SessionRequest`, start, and report `Connecting`.
4. **Events:**
   - `Connected` gives `ItemStatus::Connected`.
   - `FrameReady` calls `present_frame`.
   - `CertificateUntrusted` shows a sheet. On accept, `on_trust(id, fp)` saves `cert_fingerprint` to the `Store`, and the session restarts in the same windows. On reject, the windows close.
   - `Failed` shows the failure sheet (title by kind: "Couldn't sign in to {name}", "Can't reach {name}", "Connection to {name} failed"). Reconnect restarts, Edit closes and calls `on_edit_request(id)`, and Close closes.
   - `Ended` closes the windows.
   - Every close removes the session and reports `Idle`.
5. **Close button:** `windowShouldClose` calls `handle.disconnect()` and returns YES. Closing any window of a multi-window session closes all of them.

Certificate sheet text:
- **First use:** `"First connection to {name}"` / `"This server's identity certificate isn't from a trusted authority. Confirm its fingerprint, then trust it for future connections.\n\nSHA-256: {display_fingerprint}"`, with buttons "Trust and Connect" / "Cancel".
- **Changed:** `"WARNING: {name}'s identity changed"` / `"The certificate is different from the one you trusted before. This can mean the server was reinstalled — or that someone is intercepting the connection.\n\nNew SHA-256: {new}\nPrevious: {old}"`, with buttons "Cancel" (default) / "Trust New Certificate".

Remove the Task 7 `RDPL_DEV_*` dev path.

- [ ] **Step 1: Implement and wire.** The launcher's `connect(i)` calls `manager.connect(&conns[i])`, and `on_status` maps id to index and calls `launcher.set_status`. `applicationShouldTerminate` uses `manager.count()` and `disconnect_all()`.
- [ ] **Step 2: Build.** Run: `cd macos && cargo build`. Expected: OK.
- [ ] **Step 3: Manual check.** Use a connection whose host is a closed port, such as `127.0.0.1:1`, and then the user's real servers. Expected:
  - **closed port:** the "Can't reach" sheet appears, Reconnect retries, and Edit opens the dialog
  - **real server, first time:** the trust sheet shows the fingerprint, and after trusting, the connection proceeds and the fingerprint is saved in `connections.json`
  - **second connect:** no prompt
  - **status:** the button glows green while connected, and clicking it again brings the window forward without a second session
  - **closing:** closing the window clears the glow
  - **quitting** with two sessions asks "Disconnect 2 session(s) and quit?"
- [ ] **Step 4: Commit.** `git commit -m "macos: launch sessions from launcher with TOFU, errors, status glow"`

### Task 13: Display modes, auto-resize and the Session menu

**Files:** Modify `macos/src/session/window.rs`, `macos/src/session/view.rs`, `macos/src/app.rs`, `macos/src/session/mod.rs`.

**Interfaces:**
- Produces (`window::SessionWindows`):
  - `open(mtm, title: &str, handle: SessionHandle, mode: DisplayMode, displays: &[DisplayInfo], layout: &[RemoteMonitor]) -> Rc<SessionWindows>`
  - `toggle_full_screen(&self)`, `focus(&self)`, `close_all(&self)`, `windows(&self) -> Vec<Retained<NSWindow>>`

Modes:
- **Full screen with one display:** a titled, resizable window with `NSWindowCollectionBehavior::FullScreenPrimary`. It is placed on that display's screen, then `toggleFullScreen:` is called after it is ordered front. Remote size = that display's `frame` size.
- **Windowed:** a titled, resizable window at the initial size from Global Constraints, centred on the main display. Window resizing (`windowDidResize`) restarts a 300 ms debounce timer. When it fires, call `handle.resize(even_size(w), clamp(h, 200, 8192))` using the content size in points. Entering or leaving native full screen triggers the same path.
- **Title:** `"{name} — {host}"`. The remote image fills the view (`kCAGravityResize`), so mouse mapping stays a plain scale.

Session menu, inserted between Edit and Window in `app.rs`:
- **Items:** Toggle Full Screen (⌃⌘F), Send Ctrl+Alt+Del, Send Windows Key, Disconnect (⌘⇧W).
- **Wiring:** the actions are selectors implemented on `RdpView`, which sits in the first-responder chain (`toggleSessionFullScreen:`, `sendCtrlAltDel:`, `sendWindowsKey:`, `disconnectSession:`). `validateMenuItem:` returns YES only when an `RdpView` is first responder, so the menu greys out over the launcher.
- **Key handling:** `RdpView::performKeyEquivalent:` passes ⌘Q, ⌃⌘F and ⌘⇧W to the menu.

- [ ] **Step 1: Implement the modes, the debounce and the Session menu.** Thread `DisplayMode` through `SessionManager::connect`.
- [ ] **Step 2: Build.** Run: `cd macos && cargo build`. Expected: OK.
- [ ] **Step 3: Manual check on a real server.** Expected:
  - a Full screen connection opens in its own Space on the selected display, Mission Control shows the window title, and a three-finger swipe moves between two full-screen sessions
  - ⌃⌘F leaves full screen and the remote desktop resizes to the window
  - a Windowed connection opens at 1280×800, and dragging the window edge resizes the remote desktop about 0.3 s after release
  - Session → Send Ctrl+Alt+Del shows the Windows security screen
  - Disconnect closes the session, and the Session menu is greyed out while the launcher is focused
- [ ] **Step 4: Commit.** `git commit -m "macos: full screen and windowed modes, auto-resize, Session menu"`

### Task 14: Clipboard

**Files:** Create `macos/src/session/clipboard_codec.rs` and `macos/src/session/clipboard.rs`. Modify `macos/src/session/mod.rs`.

**Interfaces:**
- Produces (`clipboard_codec`, pure):
  - `pub const CF_UNICODETEXT: u32 = 13;`
  - `pub fn encode_unicode_text(s: &str) -> Vec<u8>`: converts `\n` to `\r\n` (leaving existing `\r\n` alone), then UTF-16LE, then a trailing `0x0000`.
  - `pub fn decode_unicode_text(bytes: &[u8]) -> String`: UTF-16LE (ignoring an odd trailing byte), stopping at the first NUL, converting `\r\n` to `\n`, lossy.
  - `pub struct PasteboardWatcher { last_seen: isize, suppressed: Option<isize> }`
  - `impl PasteboardWatcher { pub fn new(current: isize) -> Self; pub fn observe(&mut self, change_count: isize) -> bool /* true = local user copied */; pub fn suppress_next(&mut self, change_count_after_our_write: isize) }`
- Produces (`clipboard`):
  - `pub struct MacClipboard`, implementing `ironrdp_cliprdr::backend::CliprdrBackend`
  - `pub struct MacClipboardFactory`, implementing `CliprdrBackendFactory`
  - `pub fn new_clipboard(mtm) -> (MacClipboardFactory, ClipboardLink)`
  - `pub struct ClipboardLink`, with `attach(&self, sender: RdpInputSender)` and `detach(&self)`. It owns the 500 ms main-thread `NSTimer` that polls `NSPasteboard.generalPasteboard.changeCount`.

Flow (text only):
- **Mac to remote:** on a local copy (`observe` returns true and the pasteboard has `NSPasteboardTypeString`), cache the string in shared state and send `ClipboardMessage::SendInitiateCopy(vec![ClipboardFormat::new(ClipboardFormatId::new(CF_UNICODETEXT))])` via `send_clipboard`. `on_format_data_request(CF_UNICODETEXT)` replies with `SendFormatData` of `encode_unicode_text(cached)`.
- **Remote to Mac:** `on_remote_copy(formats)` containing `CF_UNICODETEXT` sends `SendInitiatePaste(CF_UNICODETEXT)`. `on_format_data_response(ok)` decodes the text, then hops to the main thread to `clearContents` plus `setString`, and calls `suppress_next(newChangeCount)` so the write isn't echoed back.
- **Ignore:** non-text formats, file lists and locks (no-op).
- **Reference:** the trait method names live in `crates/ironrdp-cliprdr/src/backend.rs`; follow `crates/ironrdp-cliprdr-native/src/linux/` at the pinned rev for the request and response choreography.

- [ ] **Step 1: Write the failing codec tests.**
```rust
#[test] fn encode_adds_crlf_and_nul() { assert_eq!(encode_unicode_text("a\nb"), vec![b'a',0,b'\r',0,b'\n',0,b'b',0,0,0]); }
#[test] fn decode_strips_nul_and_crlf() { assert_eq!(decode_unicode_text(&encode_unicode_text("héllo\nwörld 🎉")), "héllo\nwörld 🎉"); }
#[test] fn decode_tolerates_odd_length() { assert_eq!(decode_unicode_text(&[b'a',0,b'b']), "a"); }
#[test] fn watcher_reports_user_copy_once() { let mut w=PasteboardWatcher::new(5); assert!(!w.observe(5)); assert!(w.observe(6)); assert!(!w.observe(6)); }
#[test] fn watcher_ignores_our_own_write() { let mut w=PasteboardWatcher::new(5); w.suppress_next(6); assert!(!w.observe(6)); assert!(w.observe(7)); }
```
- [ ] **Step 2: Run them to verify they fail.** Run: `cd macos && cargo test --lib clipboard_codec`. Expected: FAIL.
- [ ] **Step 3: Implement both modules.** `SessionManager` creates one `(factory, link)` per session, passes `Some(Box::new(factory))` in `SessionRequest.clipboard`, calls `link.attach(handle.input_sender())` on `Connected`, and calls `detach` on close.
- [ ] **Step 4: Run the tests and check manually.** `cargo test --lib clipboard_codec` should PASS. Then, against a real server:
  - copying text on the Mac and pasting with ⌘V in remote Notepad works
  - copying in remote Notepad and pasting in a Mac TextEdit window works
  - emoji survive
  - copying an image on the Mac does nothing remotely and logs no error
- [ ] **Step 5: Commit.** `git commit -m "macos: text clipboard sync via NSPasteboard cliprdr backend"`

### Task 15: Shared folder

**Files:** Create `macos/src/session/drive.rs`. Modify `macos/src/session/engine.rs` and `macos/src/session/mod.rs`.

**Interfaces:**
- Produces:
  - `pub fn drive_name(folder: &Path) -> String`: the folder's file name, or `"Mac"` for `/`. Use `NSFileManager displayNameAtPath` only in the UI, not here.
  - `pub fn factory(folder: &Path) -> Box<dyn ironrdp_rdpdr::RdpdrBackendFactory + Send>`: `NixRdpdrBackendFactory::new(folder_string).with_drive(1, drive_name(folder))`.
- In `engine.rs`, when `shared_folder` is `Some(p)` and `p` is an existing directory, call `with_rdpdr(true)` and `.with_rdpdr_backend_factory(drive::factory(p))`. If the folder is missing, log a warning and connect without it.

- [ ] **Step 1: Write the failing tests.**
```rust
#[test] fn drive_name_uses_folder_name() { assert_eq!(drive_name(Path::new("/Users/me/Downloads")), "Downloads"); }
#[test] fn drive_name_root_is_mac() { assert_eq!(drive_name(Path::new("/")), "Mac"); }
#[test] fn factory_builds_product_with_one_drive() { /* tempdir → factory(dir).build_rdpdr_backend() is Ok */ }
```
- [ ] **Step 2: Run them to verify they fail.** Run: `cd macos && cargo test --lib drive`. Expected: FAIL.
- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run the tests and check manually.** The tests should PASS. Then, with Shared folder set to `~/Downloads` on a Windows server, Explorer → This PC shows a "Downloads on Mac" drive, and files can be opened, created and deleted from it. macOS may ask once for Downloads access; allow it.
- [ ] **Step 5: Commit.** `git commit -m "macos: shared folder drive redirection"`

### Task 16: Multi-monitor sessions

**Files:** Modify `macos/src/session/window.rs`, `macos/src/session/view.rs`, `macos/src/session/mod.rs`.

**Interfaces:**
- Consumes: `monitors::{remote_layout, monitor_region, virtual_desktop}` and `geometry::contents_rect_top_left`
- Behaviour, when `mode == Fullscreen && displays.len() > 1`, in `SessionWindows::open`:
  - Create one borderless `NSWindow` per display (`styleMask = Borderless`, `level = NSNormalWindowLevel`, frame = the display's `frame`, `collectionBehavior = Managed | FullScreenNone`).
  - Each window gets an `RdpView` whose `ViewMapping.src = monitor_region(layout, i)`, and every view presents the same `CGImage` with its own `contentsRect`.
  - While any of these windows is key, set `NSApp.presentationOptions = AutoHideMenuBar | AutoHideDock`. Restore the default when focus moves to a non-session window.
  - Toggle Full Screen in a multi-display session is a no-op, with a tooltip on the menu item: "Multi-monitor sessions are always full screen".

Verify `contentsRect` orientation: on macOS a layer's unit space may be bottom-left. If the halves appear swapped vertically, flip with `y' = 1 - y - h` in `RdpView::present` and keep `contents_rect_top_left` unchanged.

- [ ] **Step 1: Implement it.** Build: `cd macos && cargo build`. Expected: OK.
- [ ] **Step 2: Manual check on two external 1920×1200 displays against a Windows server.** Expected:
  - both displays are covered and the menu bar and Dock auto-hide
  - Windows shows two monitors (Settings → Display shows 1 and 2), each with its own taskbar
  - maximizing a remote window fills one monitor
  - the mouse crosses between monitors at the correct position
  - unplugging one display before connecting falls back to the main display (Review Focus #4)
- [ ] **Step 3: Commit.** `git commit -m "macos: multi-monitor full-screen sessions across selected displays"`

---

## Phase 4 — Ship

### Task 17: App bundle, DMG and documentation

**Files:** Create `macos/resources/Info.plist`, `macos/scripts/make-icon.sh`, `macos/resources/AppIcon.icns` (generated), `macos/scripts/build-app.sh`, `macos/README.md`. Modify root `README.md`, adding only a "macOS edition" section.

**Interfaces:**
- `Info.plist` keys:
  - `CFBundleName`/`CFBundleDisplayName` = `RDP Launcher`, `CFBundleIdentifier` = `com.c1t1zen.rdplauncher`, `CFBundleExecutable` = `rdp-launcher`
  - `CFBundleShortVersionString` = `1.0.0`, `CFBundleVersion` = `1`, `CFBundleIconFile` = `AppIcon`, `CFBundlePackageType` = `APPL`
  - `LSMinimumSystemVersion` = `13.0`, `NSHighResolutionCapable` = true, `NSPrincipalClass` = `NSApplication`
  - `NSLocalNetworkUsageDescription` = "RDP Launcher connects to Remote Desktop servers on your local network."
  - `NSHumanReadableCopyright` = "Copyright (c) 2026 c1t1zen. MIT License."
- `make-icon.sh`: use `sips -s format png ../assets/app.ico` (256 px), produce an `AppIcon.iconset` with sizes 16, 32 (16@2x and 32), 64 (32@2x), 128, 256 (128@2x and 256), then `iconutil -c icns` into `resources/AppIcon.icns`.
- `build-app.sh` (`set -euo pipefail`; run from `macos/`):
  1. `cargo build --release --target aarch64-apple-darwin`
  2. Assemble `dist/RDP Launcher.app/Contents/{MacOS/rdp-launcher, Info.plist, Resources/AppIcon.icns}`.
  3. `codesign --force --deep --sign - "dist/RDP Launcher.app"`
  4. Stage the app plus an `Applications -> /Applications` symlink, then `hdiutil create -volname "RDP Launcher" -srcfolder <stage> -ov -format UDZO "dist/RDP-Launcher-1.0.0.dmg"`.
  5. Print both paths and sizes.

- [ ] **Step 1: Write the scripts and plist, and generate the icon.** Run: `cd macos && ./scripts/make-icon.sh && ./scripts/build-app.sh`. Expected: `dist/RDP Launcher.app` and `dist/RDP-Launcher-1.0.0.dmg` exist. `codesign --verify --verbose "dist/RDP Launcher.app"` reports "valid on disk", and the DMG is roughly 5–20 MB.
- [ ] **Step 2: Install test.** `open dist/RDP-Launcher-1.0.0.dmg`, drag the app to Applications, and launch it from Launchpad. Expected: the launcher appears with the correct icon in the Dock and the menu bar. Connecting to a LAN host shows the macOS Local Network prompt once, and after allowing it the session connects. `connections.json` from development is reused.
- [ ] **Step 3: Write `macos/README.md`** covering build (install rustup, `./scripts/build-app.sh`), install (drag from the DMG, right-click → Open for downloaded copies), features, keyboard rules, storage and security, and troubleshooting (Local Network permission, certificate prompts, xrdp servers). Add a short "macOS edition" section to the root `README.md` linking to it, without changing any other root content.
- [ ] **Step 4: Run the full manual checklist** (spec §8) against the installed app and record the results in the commit message. Also run `cd macos && cargo test && cargo clippy --all-targets -- -D warnings`. Expected: all tests pass and clippy is clean.
- [ ] **Step 5: Commit.** `git add macos README.md && git commit -m "macos: app bundle, DMG packaging, and docs"`

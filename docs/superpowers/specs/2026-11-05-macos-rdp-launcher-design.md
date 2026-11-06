# RDP Launcher for macOS — Design

**Date:** 2026-11-05
**Status:** Approved in conversation; awaiting written-spec review
**Branch:** `macos-port`

## 1. Goal

A macOS (Apple Silicon) edition of RDP Launcher with feature parity to the
Windows app (v1.2.0), delivered as a single `RDP Launcher.app` installed by
dragging it into `/Applications`.

Unlike Windows, macOS has no built-in RDP client, so the app contains its own
**pure-Rust RDP client built on IronRDP**. Clicking a connection opens a
native session window (full screen or windowed) for that server; several
servers can be open at once, each in its own window / full-screen Space.

### Success criteria

- One click on a saved connection opens a working RDP session (picture,
  keyboard, mouse) without any other app installed.
- Every Windows launcher feature has a Mac equivalent (see §4).
- In-session: copy/paste, remote sound, shared local folder, auto-resize,
  session menu, and multi-monitor spanning across chosen displays.
- Works against Windows servers (NLA/CredSSP) and xrdp/Linux servers (TLS).
- Built locally into a drag-to-install `.dmg`.

### Constraints

- **The Windows code is not touched:** root `Cargo.toml`, `Cargo.lock`,
  `build.rs`, `src/`, `.cargo/`, `tools/`. All Mac code lives under `macos/`
  as an independent Cargo package (the root package has no `[workspace]`, so
  a nested package builds independently).
- Apple Silicon (`aarch64-apple-darwin`), macOS 13 Ventura or later.
- Rust toolchain installed via `rustup` (user-local); Swift/Xcode not used.

### Non-goals (v1)

- Intel Macs / universal binary.
- Apple notarization / Developer ID signing (ad-hoc signed only).
- Clipboard images or files (text only).
- Microphone, printer, smart-card, USB redirection.
- RD Gateway, Azure/Entra sign-in.
- Importing Windows DPAPI-encrypted passwords.

## 2. Architecture

**Approach A — all-Rust, raw AppKit via `objc2`, single process.** Mirrors the
Windows edition's "raw platform API, no framework" philosophy.

- One process, one Dock icon. The launcher window and every session window are
  real `NSWindow`s. The Window menu lists sessions by name; ⌘\` cycles them.
- Each RDP session runs as an async task on a shared multi-threaded `tokio`
  runtime (background threads). AppKit stays on the main thread.
- Engine → UI: frame updates and state changes are posted to the main thread
  (main dispatch queue). UI → engine: input/resize/clipboard events go over a
  `tokio::sync::mpsc` channel.
- A failure in one session ends only that session (task-level error handling,
  panics caught at the task boundary); other sessions keep running.

### Key dependencies

| Purpose | Crate(s) |
|---|---|
| AppKit / Foundation / CoreAnimation | `objc2`, `objc2-foundation`, `objc2-app-kit`, `objc2-quartz-core`, `objc2-core-graphics` (exact set settled in the plan) |
| Main-thread dispatch | `dispatch2` |
| RDP | `ironrdp` 0.17 family: connector, session, graphics, input, cliprdr, rdpsnd, rdpdr + `ironrdp-rdpdr-native` (macOS/Linux `nix` backend), displaycontrol, dvc, svc; `ironrdp-tokio`; optionally `ironrdp-client` 0.1 engine if it allows injecting our own clipboard backend |
| Async runtime | `tokio` |
| TLS | `rustls` (via IronRDP feature) |
| Audio output | `cpal` (CoreAudio) — via `ironrdp-rdpsnd-native` if it works on macOS, else our own `rdpsnd` client handler on `cpal` |
| Keychain | `security-framework` |
| Config | `serde`, `serde_json`, `uuid` |
| H.264 (EGFX) | `openh264` feature, if it builds cleanly for arm64; otherwise RemoteFX/bitmap codecs only |

### Source layout

```
macos/
├── Cargo.toml
├── .gitignore               # target/, dist/
├── README.md                # Mac build/install/usage notes
├── src/
│   ├── main.rs              # NSApplication setup, app delegate, main menu, status item
│   ├── model.rs             # Connection, Settings, JSON load/save, defaults
│   ├── keychain.rs          # password get/set/delete/copy in macOS Keychain
│   ├── certs.rs             # trust-on-first-use server certificate fingerprints
│   ├── displays.rs          # enumerate NSScreens, stable display keys
│   ├── launcher/
│   │   ├── mod.rs           # launcher window, header strip, autosize, scroll
│   │   ├── list_view.rs     # custom NSView: buttons, hover, icons, drag + spring animation
│   │   ├── edit_dialog.rs   # add/edit connection panel
│   │   ├── password_prompt.rs
│   │   └── about.rs         # About / manual window
│   └── session/
│       ├── mod.rs           # Session registry: id → session handle, focus-existing logic
│       ├── engine.rs        # IronRDP connect + active-stage loop on tokio
│       ├── view.rs          # NSView/CALayer rendering of the framebuffer; one window per display
│       ├── keymap.rs        # macOS virtual keycode → RDP scancode; ⌘ translation
│       ├── clipboard.rs     # NSPasteboard ↔ cliprdr backend (text)
│       ├── audio.rs         # rdpsnd → cpal
│       ├── drive.rs         # shared folder via rdpdr nix backend
│       ├── monitors.rs      # chosen displays → ClientMonitorData / DisplayControl layout
│       └── menu.rs          # Session menu actions
├── resources/
│   ├── Info.plist
│   └── AppIcon.icns         # generated from ../assets/app.ico
├── tests/                   # integration test against an in-process IronRDP test server
└── scripts/
    └── build-app.sh         # cargo build → .app bundle → ad-hoc codesign → .dmg
```

## 3. Data model & storage

Location: `~/Library/Application Support/RdpLauncher/`.

`connections.json` — array, order = launcher order (one entry shown;
comments are illustrative only):

```json
{
  "id": "6f1c…uuid",
  "name": "Pi5-16GB",
  "host": "192.168.0.161",
  "username": "pi",
  "display": "fullscreen",          // "fullscreen" | "windowed"
  "monitors": ["<display-key>", …], // empty = main display
  "shared_folder": "/Users/me/Downloads", // null = none
  "sound": true,
  "cert_fingerprint": "sha256:…"    // null until first trusted connect
}
```

- All new fields use serde defaults, so a Windows `connections.json`
  (`name`, `host`, `username`, `password_enc`) loads; `password_enc` is
  ignored and a fresh `id` is assigned on first save.
- `settings.json`: `{ "always_on_top": true }` (default `true`, as on Windows).
- **Passwords** live only in the Keychain: generic password, service
  `RDP Launcher`, account = connection `id`. Delete connection → delete item;
  duplicate → copy item to the new id. Blank password → prompt on connect with
  a "Remember" checkbox.
- **Display keys:** stable identity from vendor/model/serial
  (`CGDisplayVendorNumber`/`ModelNumber`/`SerialNumber`), falling back to the
  screen's localized name. If no saved display is connected, the session uses
  the main display.
- Username forms: `user`, `DOMAIN\user`, `user@domain` (parsed into IronRDP
  username/domain).

## 4. Launcher (parity with Windows v1.2.0)

- **Layout:** header strip (minimize, pin, ⓘ) → one large button per
  connection (bold name, host below) → **+** bottom-right. Window auto-sizes to
  the list; scrolls silently if taller than the screen.
- **Look:** silver→blue gradient rounded buttons (Windows palette), SF system
  font, SF Symbols icons (pencil, doc.on.doc, trash, line.3.horizontal, pin,
  info.circle, minus, plus).
- **Connect:** click a button. If the connection already has a session, bring
  its window(s) forward (switching Space) instead of starting another. Repeated
  clicks while connecting are ignored.
- **Status:** button glows light green while its session is connecting or
  connected.
- **Quick actions:** ✏️ edit, 📋 duplicate (inserted directly below, name +
  " (copy)"), 🗑 delete (confirm *Delete "Name"?*). Right-click menu: Connect,
  Edit / Rename, Duplicate, Delete.
- **Drag reorder:** press the ≡ grip and drag; others spring into place
  (stiffness 0.30, damping 0.70, ~60 Hz timer). Order saved on drop.
- **Pin (always on top):** floating window level; persisted.
- **Minimize / close button:** hide the launcher (app keeps running).
- **Menu-bar status item** (replaces tray): click toggles launcher; menu has
  Open, About, Quit. Clicking the Dock icon also shows the launcher.
- **Quit** with open sessions asks *Disconnect N sessions and quit?*
- **Tooltips** on every control and button zone.
- **About (ⓘ):** version + user manual, rewritten for macOS (same sections as
  the Windows manual).
- **Edit dialog fields:** Name, Host / IP (`host[:port]`), Username, Password
  (secure field), Display (Full screen / Windowed), Monitors (checkbox per
  connected display, "name — resolution"; main display ticked by default),
  Shared folder (None / Choose… via `NSOpenPanel`), Sound (on by default).
  Clipboard is always on.

## 5. Session windows

### Display modes

- **Full screen, one display:** native macOS full screen → own Space per
  session (swipe between servers; Mission Control shows the connection name).
  Remote resolution = that display's size.
- **Full screen, 2+ displays:** one borderless window per selected display,
  each covering its display; menu bar and Dock auto-hide while a session
  window is key. The server receives a multi-monitor layout matching the
  physical arrangement (primary = first selected display at (0,0)); Windows
  sees real separate monitors. Each window shows its region of the single
  remote framebuffer (shared image, per-window `contentsRect`).
- **Windowed:** normal resizable window; remote resolution follows window size
  after ~300 ms of no resizing (DisplayControl channel). If the server lacks
  DisplayControl, the image scales to fit.
- **Resolution:** sessions use logical (point) resolution at 100% scale for
  maximum server compatibility.

### Session menu (main menu bar, active when a session window is key)

Toggle Full Screen (⌃⌘F) · Send Ctrl+Alt+Del · Send Windows Key · Disconnect.
The window close button disconnects.

### Keyboard & mouse

- Keys are mapped by physical position (macOS virtual keycode → PC scancode),
  so the remote keyboard layout applies.
- ⌥ → Alt, ⌃ → Ctrl, ⇧ → Shift.
- **⌘ + key → Ctrl + key** (⌘C, ⌘V, ⌘Z, ⌘S…). **Tapping ⌘ alone** (press and
  release with no other key) → Windows key.
- Reserved for the Mac: ⌘Tab, ⌘Space, ⌃⌘F, ⌘Q (asks before quitting).
- Mouse: move, left/right/middle buttons, vertical + horizontal scroll.

### In-session features

- **Clipboard:** Unicode text both directions (custom NSPasteboard backend
  for `cliprdr`; IronRDP has no macOS backend).
- **Sound:** remote audio (`rdpsnd`) to the current macOS output via `cpal`.
- **Shared folder:** appears on the remote as a drive named
  "*FolderName* on Mac", read/write (`rdpdr` drive redirection).
- **Errors:** wrong credentials, unreachable host, protocol error or
  certificate mismatch → alert on that session's window with **Reconnect** and
  **Edit Connection**. Other sessions unaffected.

## 6. Security

- Passwords only in the Keychain; never written to disk or temp files, never
  logged.
- **Trust-on-first-use certificates:** on first connect, show the server
  certificate SHA-256 fingerprint and ask to trust; store it per connection.
  If it later changes, block the connection with a warning until the user
  accepts the new certificate.
- NLA (CredSSP: NTLM/Kerberos) when the server offers it; TLS-only otherwise
  (xrdp).
- No telemetry; no network traffic except the RDP connections.

## 7. Packaging

`macos/scripts/build-app.sh`:

1. `cargo build --release --target aarch64-apple-darwin`
2. Assemble `macos/dist/RDP Launcher.app` (`Contents/MacOS`, `Info.plist`,
   `Resources/AppIcon.icns`).
3. Ad-hoc codesign (`codesign --sign - --deep`).
4. Create `macos/dist/RDP-Launcher-<version>.dmg` (`hdiutil`) containing the
   app and an `/Applications` symlink.

`Info.plist` includes `NSLocalNetworkUsageDescription` (macOS 15+ shows a
one-time Local Network prompt when connecting to LAN hosts),
`LSMinimumSystemVersion` 13.0, bundle id `com.c1t1zen.rdplauncher`, version
`1.0.0` (first macOS release). Release profile mirrors Windows (`opt-level`,
LTO, strip). Expected size ~10–15 MB.

Locally built apps run without Gatekeeper prompts; downloaded copies need
right-click → Open once (no notarization in v1).

Root `README.md` gets a short "macOS edition" section linking to
`macos/README.md` (documentation only; no Windows code changes).

## 8. Testing

- **Unit tests (`cargo test`):** keymap table and ⌘ translation; config
  load/save, defaults and Windows-format compatibility; username/domain
  parsing; display-key matching and monitor-layout math (positions, primary,
  adjacency); reorder index math and spring step; certificate trust decisions
  (new / match / mismatch).
- **Integration test:** in-process test server built on IronRDP's server
  crate; the session engine connects over TLS, receives a frame update, sends
  key/mouse input, and exchanges clipboard text — to the extent the server
  crate supports each channel.
- **Manual checklist** on the user's real servers: connect (Windows + xrdp),
  full screen, two-monitor span, windowed resize, clipboard both ways, sound,
  shared folder read/write, certificate prompt and change warning, reconnect,
  clicking an already-open connection, quit with open sessions.

## 9. Build order

1. **Foundation / spike:** install Rust; `macos/` crate; one hard-coded session
   window connects to a real server with picture + keyboard + mouse; verify
   audio playback path on macOS (highest-risk item).
2. **Launcher parity:** model + Keychain, launcher window, buttons, edit
   dialog, drag/spring, status item, pin, About, tooltips.
3. **Session features:** full screen/windowed + auto-resize, Session menu,
   clipboard, sound, shared folder, certificate TOFU, errors/reconnect.
4. **Multi-monitor.**
5. **Packaging:** `.app` + `.dmg`, README updates.

## 10. Risks

| Risk | Mitigation |
|---|---|
| `rdpsnd` playback untested on macOS | Proven in step 1; fallback: own `rdpsnd` handler on `cpal` |
| `ironrdp-client` engine may not allow a custom clipboard backend | Assemble the session from lower-level crates (`ironrdp-connector`, `ironrdp-session`, `ironrdp-tokio`) as the engine does |
| `openh264` build issues on arm64 | Ship without EGFX/H.264; RemoteFX/bitmap codecs still work |
| macOS full-screen Spaces cannot span displays | Multi-monitor uses per-display borderless windows (§5) |
| IronRDP API churn (0.x) | Pin exact versions in `Cargo.lock` |

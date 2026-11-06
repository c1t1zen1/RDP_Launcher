<div align="center">

# 🖥️ RDP Launcher for macOS

**One-click Remote Desktop launcher with a built-in RDP client — a single `.app`, drag to install.**

*by [c1t1zen](https://github.com/c1t1zen1)*

![macOS](https://img.shields.io/badge/platform-macOS%2013%2B%20·%20Apple%20Silicon-000000?logo=apple&logoColor=white)
![Version](https://img.shields.io/badge/version-1.0.0%20·%20testing-orange)
![Rust](https://img.shields.io/badge/built%20with-Rust-DEA584?logo=rust&logoColor=black)
![License](https://img.shields.io/badge/license-MIT-blue)

</div>

---

> **⚠️ First release — testing phase.** v1.0.0 is the first version of RDP Launcher for Mac
> and is still being tested. Expect some rough edges, and please
> [open an issue](https://github.com/c1t1zen1/RDP_Launcher/issues) if something doesn't work.

The macOS edition of [RDP Launcher](../README.md). Same launcher as on Windows — big
buttons, green while connected, drag to reorder — but macOS has no built-in Remote
Desktop client, so this one carries its own, built on the pure-Rust
[IronRDP](https://github.com/Devolutions/IronRDP) library. Nothing else to install.

## ✨ Features

- **One-click launch** — one button per server; it glows **green** while connected.
  Clicking an open server brings its window back instead of opening a second one.
- **Full screen or windowed, per connection**
  - Full screen gets its own Space: swipe with three fingers (or Ctrl+←/→) between servers.
  - Windowed sessions resize the remote desktop to fit as you drag the window edge.
- **Multi-monitor** — tick two or more displays and one remote desktop spans them, with a
  real monitor (and taskbar) on each.
- **Copy & paste** text both ways between the Mac and the remote computer.
- **Remote sound** plays on the Mac.
- **Shared folder** — pick a Mac folder; it appears on the remote PC as "*Folder* on Mac".
- **Session menu** — Toggle Full Screen (⌃⌘F), Send Ctrl+Alt+Del, Send Windows Key, Disconnect (⇧⌘W).
- **Mac-friendly keyboard** — ⌘C/⌘V/⌘Z… act as Ctrl+C/V/Z… remotely; tap ⌘ alone for the
  Windows key; ⌥ is Alt. ⌘Tab, ⌘Space, ⌃⌘F and ⌘Q stay with the Mac.
- **Launcher parity with Windows** — ✏️ 📋 🗑 on every button, right-click menu, animated drag
  to reorder, always-on-top pin, About/manual, tooltips, and a menu-bar icon.
- Works with **Windows** (Network Level Authentication) and **Linux/xrdp** servers.

## 🔐 Storage & security

| What | Where / how |
|---|---|
| Connection list | `~/Library/Application Support/RdpLauncher/connections.json` (same format as the Windows app's file) |
| Settings | `~/Library/Application Support/RdpLauncher/settings.json` |
| Passwords | macOS **Keychain** (service "RDP Launcher") — never in the JSON, logs or temp files |
| Server identity | Trust-on-first-use: the first connection shows the server certificate's SHA-256 fingerprint; if it ever changes you're warned **before** any password is sent |
| Log | `~/Library/Logs/RDP Launcher/rdp-launcher.log` (no passwords) |

No cloud, no telemetry: nothing leaves your Mac except the RDP connections themselves.
A Windows `connections.json` can be copied over and loads as-is (passwords must be re-entered).

## 📦 Install

1. Download `RDP-Launcher-1.0.0.dmg` from the
   [macOS v1.0.0 release](https://github.com/c1t1zen1/RDP_Launcher/releases/tag/macos-v1.0.0)
   (or build it yourself, below).
2. Open the `.dmg` and drag **RDP Launcher** into **Applications**.
3. Start it from Launchpad or Spotlight.
4. The first time you connect to a computer on your network, macOS asks to allow
   **Local Network** access — click **Allow**.

A copy you build yourself opens normally. A downloaded copy isn't notarized by Apple, so
the first launch is blocked with "Apple could not verify…":

- **macOS 15 or later:** click **Done**, then open System Settings → **Privacy & Security**,
  scroll down and click **Open Anyway** next to RDP Launcher.
- **macOS 13–14:** right-click the app → **Open**, then **Open** again.

## 🛠️ Build from source

Requires Apple Silicon, macOS 13+, the Xcode Command Line Tools and [Rust](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cd RDP_Launcher/macos
./scripts/build-app.sh
```

Output:

```
macos/dist/RDP Launcher.app
macos/dist/RDP-Launcher-1.0.0.dmg
```

`./scripts/make-icon.sh` regenerates `resources/AppIcon.icns` from `../assets/app.ico`.

### Testing without a server

```bash
cargo test                                # unit + end-to-end tests (in-process RDP server)
cargo run --example test_server           # local RDP server on 127.0.0.1:3390, user / password
```

Add a connection to `127.0.0.1:3390` (user `user`, password `password`) to try the app:
the test server shows a colour test pattern and prints keyboard, mouse, resize and
clipboard activity.

## 🧩 Tech notes

- **Rust + raw AppKit** via [`objc2`](https://github.com/madsmtm/objc2) — real `NSWindow`s,
  native menus, menu-bar icon, native full screen; no UI framework.
- **IronRDP** (`ironrdp-client`, pinned git revision) for the protocol: TLS + CredSSP/NLA,
  RemoteFX/EGFX (H.264 via OpenH264), display control, RDPSND, CLIPRDR, RDPDR.
- One process, one tokio runtime; each session's screen updates land in a shared
  framebuffer that the session window draws through a `CALayer`.

## 🧯 Troubleshooting

| Problem | Try |
|---|---|
| "Can't reach …" | Check the host name / IP, that Remote Desktop is on, the network or VPN, and System Settings → Privacy & Security → **Local Network** → RDP Launcher |
| "Couldn't sign in …" | Re-enter username / password; use `DOMAIN\user` for domain accounts |
| "Identity changed" warning | Expected after a server reinstall; otherwise don't connect |
| xrdp shows a login screen | Normal for xrdp without saved credentials |
| No sound | Check the connection's Sound option and the Mac's output device |

## 📄 License

[MIT](../LICENSE) © 2026 [c1t1zen](https://github.com/c1t1zen1)

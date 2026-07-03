<div align="center">

# 🖥️ RDP Launcher

**One-click RDP connection launcher for Windows — a single tiny native `.exe`, zero dependencies.**

*by [c1t1zen](https://github.com/c1t1zen1)*

![Windows](https://img.shields.io/badge/platform-Windows%2010%2F11-0078D6?logo=windows&logoColor=white)
![Rust](https://img.shields.io/badge/built%20with-Rust-DEA584?logo=rust&logoColor=black)
![Size](https://img.shields.io/badge/exe%20size-~400%20KB-brightgreen)
![License](https://img.shields.io/badge/license-MIT-blue)

<img src="assets/screenshot.png" width="340" alt="RDP Launcher screenshot">

</div>

---

## ✨ Features

- **One-click launch** — big rectangle buttons, one per saved connection. Click and you're in.
- **Auto-login** — saves host, username, and password, so `mstsc` connects without prompting.
- **Live status** — buttons glow **light green** while their RDP session is running.
- **Animated drag to reorder** — grab the `≡` handle on the right edge of any button and drag up/down; the other buttons spring out of the way with a smooth, slightly bouncy animation.
- **Quick actions on every button** — tiny ✏️ edit, 📋 duplicate, and 🗑️ delete icons in the bottom-right corner, plus a full right-click menu (🖥️ Connect, ✏️ Edit / Rename, 📋 Duplicate, 🗑️ Delete).
- **Always on top** — the launcher stays visible so you can open multiple sessions.
- **Auto-expanding, scrollbar-free** — the window grows and shrinks with your list (mouse-wheel scrolls silently if it ever outgrows the screen).
- **Native look** — pure Win32, silver/blue gradient buttons, Segoe UI, embedded Remote Desktop icon.

## 🚀 Getting started

1. Grab `rdp-launcher.exe` (build it yourself below, or download from [Releases](https://github.com/c1t1zen1/RDP_Launcher/releases)).
2. Run it — no installer, no services, no runtime dependencies. Pin it to your taskbar if you like.
3. Click the **+** button (bottom-right corner) and enter:
   - **Name** — any friendly label (rename anytime via right-click)
   - **Host / IP** — e.g. `192.168.0.161` or `server:3390`
   - **Username / Password** — optional; leave blank to be prompted on connect
4. Click the big button to launch the session. Done.

## 🔐 Storage & security

| What | Where / How |
|---|---|
| Connection list | `%APPDATA%\RdpLauncher\connections.json` |
| Passwords | Encrypted with **Windows DPAPI** — only your Windows user account can decrypt them |
| Login handoff | Credentials registered via `cmdkey` (`TERMSRV/<host>`), then `mstsc` is started with a generated `.rdp` file |

No cloud, no telemetry, nothing leaves your machine.

## 🛠️ Building from source

Requires only a stable [Rust](https://rustup.rs/) toolchain:

```powershell
git clone https://github.com/c1t1zen1/RDP_Launcher.git
cd RDP_Launcher
cargo build --release
```

The result is a single self-contained executable (static CRT — no DLLs, no installs):

```
target\release\rdp-launcher.exe
```

To regenerate the embedded icon from Windows' own Remote Desktop icon:

```powershell
powershell -ExecutionPolicy Bypass -File tools\make_icon.ps1
```

## 📁 Project layout

```
RDP_Launcher/
├── src/main.rs          # Entire app: pure Win32 GUI, owner-drawn buttons, DPAPI, launcher
├── build.rs             # Embeds icon + version info into the exe
├── assets/
│   ├── app.ico          # Multi-size application icon
│   └── screenshot.png   # README screenshot
├── tools/
│   ├── make_icon.ps1    # Extracts the Windows RDP icon into app.ico
│   └── screenshot.ps1   # Captures the app window for the README
├── .cargo/config.toml   # Static CRT + self-contained linker settings
└── Cargo.toml
```

## 🧩 Tech notes

- **Zero frameworks** — raw Win32 via [`windows-sys`](https://crates.io/crates/windows-sys); owner-drawn buttons with GDI gradients.
- **Tiny** — `opt-level = "z"`, LTO, stripped, `panic = "abort"` → a few hundred KB.
- **Drag-reorder** — button windows are subclassed; the `≡` handle captures the mouse while a 15 ms spring-physics timer (`stiffness 0.30, damping 0.70`) animates the other buttons into place with a subtle bounce.
- **Session tracking** — spawned `mstsc` process handles are polled every second to drive the green "connected" state.

## 📄 License

[MIT](LICENSE) © 2026 [c1t1zen](https://github.com/c1t1zen1)

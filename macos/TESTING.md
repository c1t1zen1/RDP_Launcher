# Manual test checklist (macOS edition)

Automated coverage: `cargo test` (unit tests plus end-to-end tests against an in-process
RDP server) and `cargo run --example test_server` for hands-on tries. The items below
need real servers, real displays or your eyes. Tick them off on a Windows server, and on
an xrdp server where noted.

## Install
- [ ] Open the `.dmg`, drag **RDP Launcher** to Applications, launch it from Launchpad.
- [ ] The Dock and menu bar show the RDP Launcher icon; the launcher window appears.
- [ ] The first connection to a LAN address shows macOS's **Local Network** prompt; after **Allow** it connects.

## Launcher
- [ ] **+** → New Connection: an empty host shows "Enter a host name or IP address."; `host:99999` shows the port message.
- [ ] Save adds a button; ✏️ edits (fields pre-filled); 📋 adds "(copy)" below; 🗑 asks *Delete "Name"?*.
- [ ] Right-click a button: Connect / Edit / Rename… / Duplicate / Delete all work.
- [ ] Drag the ≡ grip: the other buttons spring aside; the new order survives a restart.
- [ ] Hover shows tooltips on buttons, icons, grip, pin, ⓘ, minus and **+**.
- [ ] The pin toggles "always on top" and the choice survives a restart.
- [ ] Minus and the red close button hide the launcher; clicking the menu-bar icon or the Dock icon brings it back.
- [ ] Right-clicking the menu-bar icon gives Open / About / Quit; About shows the manual.
- [ ] **Keychain Access** shows an "RDP Launcher" item for each connection with a saved password.
- [ ] Save a password containing spaces and non-ASCII characters; reconnect after quitting/relaunching and confirm it is reused.
- [ ] Connect without a saved password, tick **Remember in Keychain**, then reconnect and confirm the launcher does not ask again.
- [ ] Deny Keychain access: the app reports that it cannot read/save the password instead of silently treating it as missing or saved.

## Sessions (Windows server; xrdp too)
- [ ] First connect shows the certificate fingerprint; **Trust and Connect** connects and the button turns green.
- [ ] Reconnecting doesn't ask again. A server with a changed certificate shows the **identity changed** warning.
- [ ] Wrong password → "Couldn't sign in…" with Reconnect / Edit Connection / Close.
- [ ] On TLS-enabled xrdp, saved username/password log in without retyping them at the xrdp login screen (subject to the server's authentication/session policy).
- [ ] On an xrdp server with `security_layer=rdp`, the error explains the TLS requirement and xrdp configuration fix. After enabling TLS with a readable certificate/key and restarting xrdp, the connection works.
- [ ] Unreachable host → "Can't reach…".
- [ ] Clicking a green button brings its window to the front (no second session).
- [ ] **Full screen** opens in its own Space on the chosen display; three-finger swipe moves between two servers.
- [ ] ⌃⌘F leaves full screen; the remote desktop resizes to the window.
- [ ] **Windowed**: dragging the window edge resizes the remote desktop about 0.3 s later.
- [ ] Typing works; ⌘C/⌘V/⌘Z act as Ctrl+C/V/Z; tapping ⌘ opens Start; ⌥ acts as Alt.
- [ ] Mouse: move, left/right/middle click, vertical and horizontal scroll; the remote cursor shape shows.
- [ ] After ⌘Tab away and back, no keys are stuck (e.g. Shift or Ctrl).
- [ ] Session → Send Ctrl+Alt+Del shows the Windows security screen; Send Windows Key opens Start.
- [ ] Copy text on the Mac and paste it remotely; copy remotely and paste on the Mac (including emoji).
- [ ] With two servers open: copy text on server A, paste it on server B.
- [ ] ⌘\` (and ⇧⌘\`) switch between session windows instead of going to the remote.
- [ ] Remote sound plays on the Mac (with Sound on).
- [ ] Shared folder (e.g. Downloads): Explorer → This PC shows **"Downloads on Mac"**; files open, save and delete.
- [ ] Closing the session window disconnects and clears the green glow.
- [ ] Quit (⌘Q) with two sessions open asks "Disconnect 2 session(s) and quit?".

## Multi-monitor (two external 1920×1200 displays)
- [ ] Tick both external displays in the connection's Monitors and connect.
- [ ] Both displays are covered; the menu bar and Dock auto-hide.
- [ ] Windows shows two monitors (Settings → Display shows 1 and 2) with a taskbar on each.
- [ ] Maximizing a remote window fills one monitor; the mouse crosses between monitors at the right place.
- [ ] With one of the ticked displays unplugged, the session falls back to the main display.

//! The About / user manual text (the Windows manual, rewritten for macOS).

const RULE: &str = "===============================================";

pub fn manual_text(version: &str) -> String {
    let heading = |n: &str| format!("{RULE}\n{n}\n{RULE}");
    [
        format!("RDP Launcher {version}   -   by c1t1zen"),
        "A one-click launcher with a built-in Remote Desktop client for your Mac.".into(),
        String::new(),
        heading("1. NEW TO REMOTE DESKTOP?"),
        "Remote Desktop (RDP) lets you sign in to another computer over the network
and use its desktop as if you were sitting in front of it. RDP Launcher keeps
your computers in a tidy list of buttons and opens each one in its own window
with the right settings and saved credentials. The RDP client is built in -
there is nothing else to install.

Before your first connection you need:
  * The remote computer's name or IP address (for example PC-OFFICE or
    192.168.1.50). If it uses a non-standard port, write host:port.
  * Remote Desktop enabled on that computer:
    Windows: Settings > System > Remote Desktop > On.
    Linux: an xrdp server.
  * A user account on that computer that is allowed to connect
    (an administrator, or a member of Remote Desktop Users).
  * Network access to it - same LAN, a VPN, or a forwarded port."
            .into(),
        String::new(),
        heading("2. QUICK START"),
        "  1) Click the + button in the bottom-right corner.
  2) Fill in the form:
       Name           - any label you like, e.g. Office PC
       Host / IP      - the computer's name, IP, or host:port
       Username       - user, DOMAIN\\user, or user@domain (optional)
       Password       - optional; leave blank to be asked when connecting
       Display        - Full screen or Windowed
       Monitors       - the Mac displays a full-screen session covers;
                        tick two or more to spread one desktop across them
       Shared folder  - a Mac folder that appears as a drive on the remote PC
       Sound          - play the remote computer's sound on this Mac
  3) Click Save. A button appears in the list.
  4) Click that button to connect.

The first time you connect to a computer, RDP Launcher shows the fingerprint of
its identity certificate and asks whether to trust it. If that certificate ever
changes you are warned before any password is sent.

The first time you connect to a computer on your local network, macOS asks
whether RDP Launcher may find devices on your Local Network. Click Allow.

A connection button turns green while its session is running. Clicking a green
button brings the open session back to the front instead of starting a second
one.

IN A SESSION
  * Full-screen sessions get their own Space: swipe left/right with three
    fingers (or press Ctrl+Left/Right arrow) to move between your servers.
  * The Session menu in the menu bar has Toggle Full Screen (Ctrl+Cmd+F),
    Send Ctrl+Alt+Del, Send Windows Key and Disconnect (Shift+Cmd+W).
  * Keyboard: Cmd+C, Cmd+V, Cmd+Z, Cmd+S ... act as Ctrl+C, Ctrl+V ... on the
    remote computer. Tap Cmd on its own for the Windows key (Start menu).
    Option is Alt. Cmd+Tab, Cmd+Space, Ctrl+Cmd+F and Cmd+Q stay with the Mac.
  * Copy and paste text between the Mac and the remote computer both ways.
  * Close the session window to disconnect."
            .into(),
        String::new(),
        heading("3. THE BUTTONS AT THE TOP"),
        "  Minimize (-)   Hides the launcher. Click the menu-bar icon to bring it back.
  Pin            Toggles 'always on top'. When it is ON (blue pin) the
                 launcher floats above every other window. When it is OFF
                 other windows can cover it, like a normal Mac app.
                 The choice is remembered between runs.
  i              Opens this About / manual window.

Hover over anything in the app to see a short description."
            .into(),
        String::new(),
        heading("4. MANAGING CONNECTIONS"),
        "Each connection button carries three small icons in its bottom-right corner:
  Pencil   Edit the connection (name, host, username, password, display...).
  Copy     Duplicate it - handy for several accounts on one server.
  Bin      Delete it (you will be asked to confirm).

Right-click a button for the same actions plus Connect.

To reorder the list, press and hold the three-line grip on the right edge of
a button and drag it up or down. The other buttons glide out of the way and
the new order is saved automatically.

If the list is taller than the window, scroll it with the mouse wheel or trackpad."
            .into(),
        String::new(),
        heading("5. MENU-BAR ICON"),
        "The app has an icon in the menu bar at the top right of the screen.
  Click the menu-bar icon        - show or hide the launcher.
  Right-click (or Ctrl-click)    - Open, About, or Quit.
Closing the launcher with its red button only hides it. Quit from the
menu-bar icon or with Cmd+Q. Clicking the Dock icon also brings it back."
            .into(),
        String::new(),
        heading("6. WHERE YOUR DATA IS STORED"),
        "  ~/Library/Application Support/RdpLauncher/connections.json   your connections
  ~/Library/Application Support/RdpLauncher/settings.json      always-on-top setting
  ~/Library/Logs/RDP Launcher/rdp-launcher.log                  diagnostic log

Passwords are never stored as plain text. They are kept in your macOS
Keychain (look for \"RDP Launcher\" in Keychain Access), which only your Mac
user account can read. Passwords never appear in the log."
            .into(),
        String::new(),
        heading("7. TROUBLESHOOTING"),
        "  'Can't reach' the computer
      Check the spelling of the host name, or use the IP address instead.
      Remote Desktop may be off, a firewall may block TCP port 3389, or you
      are not on the same network / VPN. Also check System Settings >
      Privacy & Security > Local Network > RDP Launcher is switched on.
  'Couldn't sign in'
      Edit the connection and re-enter username and password. Use the full
      DOMAIN\\user form for domain accounts. The account may lack Remote
      Desktop permission on the remote machine.
  'Identity changed' warning
      The server's certificate differs from the one you trusted. That is
      expected after reinstalling the server; otherwise do not connect.
  Session opens but the screen is black for a moment
      Normal - the remote desktop is still loading.
  No sound
      Check the connection's Sound option and the Mac's output device."
            .into(),
        String::new(),
        heading("8. GOOD PRACTICE"),
        "Never expose port 3389 directly to the internet. Use a VPN, an RD Gateway,
or SSH tunnelling, and always use strong passwords."
            .into(),
        String::new(),
        "-----------------------------------------------".into(),
        "Copyright (c) 2026 c1t1zen. Released under the MIT licence.".into(),
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_mentions_version_and_sections() {
        let m = manual_text("1.0.0");
        for needle in [
            "RDP Launcher 1.0.0",
            "1. NEW TO REMOTE DESKTOP?",
            "2. QUICK START",
            "3. THE BUTTONS AT THE TOP",
            "4. MANAGING CONNECTIONS",
            "5. MENU-BAR ICON",
            "6. WHERE YOUR DATA IS STORED",
            "7. TROUBLESHOOTING",
            "8. GOOD PRACTICE",
            "Keychain",
            "menu-bar icon",
            "Local Network",
            "Send Ctrl+Alt+Del",
        ] {
            assert!(m.contains(needle), "manual is missing {needle:?}");
        }
        for banned in ["mstsc", "%APPDATA%", "DPAPI", "system tray"] {
            assert!(!m.contains(banned), "manual still mentions {banned:?}");
        }
    }
}

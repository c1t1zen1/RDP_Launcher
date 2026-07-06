#![windows_subsystem = "windows"]
#![allow(clippy::missing_safety_doc)]

use std::ffi::c_void;
use std::fs;
use std::mem::{size_of, zeroed};
use std::path::PathBuf;
use std::ptr::{addr_of_mut, null, null_mut};

use serde::{Deserialize, Serialize};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Security::Cryptography::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_SELECTED};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetCapture, ReleaseCapture, SetActiveWindow, SetCapture, SetFocus,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

// ---------- constants ----------
const APP_TITLE: &str = "RDP Launcher";
const CLASS_MAIN: &str = "RdpLauncherMain";
const CLASS_DLG: &str = "RdpLauncherDlg";

const BTN_W: i32 = 300;
const BTN_H: i32 = 56;
const MARGIN: i32 = 12;
const GAP: i32 = 8;
const STRIP_H: i32 = 44; // bottom strip holding the + button
const PLUS_SIZE: i32 = 30;

const HANDLE_W: i32 = 34; // width of the ≡ drag-handle zone on each button

const ID_PLUS: i32 = 999;
const ID_BTN_BASE: i32 = 1000;
const IDM_EDIT: usize = 2001;
const IDM_DELETE: usize = 2004;
const IDM_DUP: usize = 2005;
const IDM_CONNECT: usize = 2006;
const IDM_ABOUT: usize = 0x0110; // system menu item (low 4 bits must be 0)
const IDM_TRAY_OPEN: usize = 4001;
const IDM_TRAY_ABOUT: usize = 4002;
const IDM_TRAY_EXIT: usize = 4003;

const IDC_NAME: i32 = 3001;
const IDC_HOST: i32 = 3002;
const IDC_USER: i32 = 3003;
const IDC_PASS: i32 = 3004;
const IDC_OK: i32 = 3010;
const IDC_CANCEL: i32 = 3011;

const TIMER_POLL: usize = 1;
const TIMER_ANIM: usize = 2;

const WM_APP_ACTION: u32 = WM_APP + 1; // wparam: action, lparam: index
const WM_APP_TRAY: u32 = WM_APP + 2; // tray icon callback
const TRAY_ICON_ID: u32 = 1;
const ACT_EDIT: usize = 1;
const ACT_DUP: usize = 2;
const ACT_DELETE: usize = 3;

// spring physics for reorder animation
const SPRING_STIFFNESS: f32 = 0.30;
const SPRING_DAMPING: f32 = 0.70;

// ---------- data ----------
#[derive(Clone, Serialize, Deserialize, Default)]
struct Connection {
    name: String,
    host: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password_enc: String, // DPAPI-protected, hex encoded
}

struct App {
    hwnd: HWND,
    hinst: HINSTANCE,
    conns: Vec<Connection>,
    buttons: Vec<HWND>,
    procs: Vec<HANDLE>, // 0 = not running
    plus_btn: HWND,
    font_name: HFONT,
    font_host: HFONT,
    font_plus: HFONT,
    scroll: i32,
    content_h: i32,
    ys: Vec<f32>,  // animated current y per button
    vys: Vec<f32>, // velocity per button
    drag: Option<Drag>,
    anim: bool,
}

#[derive(Clone, Copy)]
struct Drag {
    idx: usize,
    grab_dy: i32, // cursor offset within the dragged button
}

static mut OLD_BTN_PROC: isize = 0;

static mut APP: Option<App> = None;

fn app() -> &'static mut App {
    unsafe { (*addr_of_mut!(APP)).as_mut().unwrap() }
}

// dialog state
struct DlgState {
    hwnd: HWND,
    done: bool,
    result: Option<Connection>,
    initial: Connection,
}
static mut DLG: Option<DlgState> = None;

fn dlg() -> &'static mut DlgState {
    unsafe { (*addr_of_mut!(DLG)).as_mut().unwrap() }
}

// ---------- helpers ----------
fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn loword(v: usize) -> u16 {
    (v & 0xFFFF) as u16
}
fn hiword(v: usize) -> u16 {
    ((v >> 16) & 0xFFFF) as u16
}

fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

fn protect(pass: &str) -> String {
    if pass.is_empty() {
        return String::new();
    }
    unsafe {
        let bytes = pass.as_bytes();
        let input = CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            pbData: bytes.as_ptr() as *mut u8,
        };
        let mut output: CRYPT_INTEGER_BLOB = zeroed();
        if CryptProtectData(
            &input,
            null(),
            null(),
            null_mut(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        ) == 0
        {
            return String::new();
        }
        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let enc = hex_encode(slice);
        LocalFree(output.pbData as HLOCAL);
        enc
    }
}

fn unprotect(enc: &str) -> String {
    if enc.is_empty() {
        return String::new();
    }
    let Some(bytes) = hex_decode(enc) else {
        return String::new();
    };
    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            pbData: bytes.as_ptr() as *mut u8,
        };
        let mut output: CRYPT_INTEGER_BLOB = zeroed();
        if CryptUnprotectData(
            &input,
            null_mut(),
            null(),
            null_mut(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        ) == 0
        {
            return String::new();
        }
        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let s = String::from_utf8_lossy(slice).into_owned();
        LocalFree(output.pbData as HLOCAL);
        s
    }
}

// ---------- persistence ----------
fn config_path() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    let dir = PathBuf::from(base).join("RdpLauncher");
    let _ = fs::create_dir_all(&dir);
    dir.join("connections.json")
}

fn load_config() -> Vec<Connection> {
    fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config() {
    let a = app();
    if let Ok(json) = serde_json::to_string_pretty(&a.conns) {
        let _ = fs::write(config_path(), json);
    }
}

// ---------- process helpers ----------
unsafe fn spawn(cmdline: &str, hide: bool, wait: bool) -> HANDLE {
    let mut cl = w(cmdline);
    let mut si: STARTUPINFOW = zeroed();
    si.cb = size_of::<STARTUPINFOW>() as u32;
    if hide {
        si.dwFlags = STARTF_USESHOWWINDOW;
        si.wShowWindow = SW_HIDE as u16;
    }
    let mut pi: PROCESS_INFORMATION = zeroed();
    let flags = if hide { CREATE_NO_WINDOW } else { 0 };
    let ok = CreateProcessW(
        null(),
        cl.as_mut_ptr(),
        null(),
        null(),
        0,
        flags,
        null(),
        null(),
        &si,
        &mut pi,
    );
    if ok == 0 {
        return 0;
    }
    CloseHandle(pi.hThread);
    if wait {
        WaitForSingleObject(pi.hProcess, 10_000);
        CloseHandle(pi.hProcess);
        return 0;
    }
    pi.hProcess
}

fn launch_connection(idx: usize) {
    let a = app();
    let c = a.conns[idx].clone();
    let pass = unprotect(&c.password_enc);
    unsafe {
        // store credentials for the terminal server so mstsc auto-logs-in
        if !c.username.is_empty() {
            let bare_host = c.host.split(':').next().unwrap_or(&c.host);
            let cmd = format!(
                "cmdkey /generic:TERMSRV/{} /user:\"{}\" /pass:\"{}\"",
                bare_host, c.username, pass
            );
            spawn(&cmd, true, true);
        }
        // write a temp .rdp file (UTF-16LE with BOM)
        let mut content = String::new();
        content.push_str(&format!("full address:s:{}\r\n", c.host));
        if !c.username.is_empty() {
            content.push_str(&format!("username:s:{}\r\n", c.username));
            content.push_str("prompt for credentials:i:0\r\n");
        } else {
            content.push_str("prompt for credentials:i:1\r\n");
        }
        content.push_str("screen mode id:i:2\r\n");
        content.push_str("authentication level:i:2\r\n");
        content.push_str("redirectclipboard:i:1\r\n");

        let tmp = std::env::temp_dir().join(format!("rdp_launcher_{}.rdp", idx));
        let mut bytes: Vec<u8> = vec![0xFF, 0xFE];
        for u in content.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let _ = fs::write(&tmp, bytes);

        // close previous handle if any
        if a.procs[idx] != 0 {
            CloseHandle(a.procs[idx]);
        }
        let cmd = format!("mstsc.exe \"{}\"", tmp.display());
        a.procs[idx] = spawn(&cmd, false, false);
        if a.buttons.len() > idx {
            InvalidateRect(a.buttons[idx], null(), 1);
        }
    }
}

// ---------- layout ----------
fn rebuild_buttons() {
    let a = app();
    for &b in &a.buttons {
        unsafe { DestroyWindow(b) };
    }
    a.buttons.clear();
    a.procs.resize(a.conns.len(), 0);
    for (i, _) in a.conns.iter().enumerate() {
        let hb = unsafe {
            CreateWindowExW(
                0,
                w("BUTTON").as_ptr(),
                null(),
                WS_CHILD | WS_VISIBLE | (BS_OWNERDRAW as u32),
                0,
                0,
                BTN_W,
                BTN_H,
                a.hwnd,
                (ID_BTN_BASE + i as i32) as HMENU,
                a.hinst,
                null(),
            )
        };
        unsafe {
            let old = SetWindowLongPtrW(hb, GWLP_WNDPROC, btn_subclass_proc as *const () as isize);
            if OLD_BTN_PROC == 0 {
                OLD_BTN_PROC = old;
            }
        }
        a.buttons.push(hb);
    }
    relayout();
    autosize_window();
}

fn target_y(i: usize) -> i32 {
    MARGIN + (i as i32) * (BTN_H + GAP) - app().scroll
}

fn relayout() {
    let a = app();
    let mut rc: RECT = unsafe { zeroed() };
    unsafe { GetClientRect(a.hwnd, &mut rc) };
    let bw = (rc.right - MARGIN * 2).max(100);
    let n = a.buttons.len();
    a.ys.resize(n, 0.0);
    a.vys.resize(n, 0.0);
    for (i, &b) in a.buttons.iter().enumerate() {
        let y = target_y(i);
        a.ys[i] = y as f32;
        a.vys[i] = 0.0;
        unsafe { MoveWindow(b, MARGIN, y, bw, BTN_H, 1) };
    }
    unsafe {
        MoveWindow(
            a.plus_btn,
            rc.right - PLUS_SIZE - 10,
            rc.bottom - STRIP_H + (STRIP_H - PLUS_SIZE) / 2,
            PLUS_SIZE,
            PLUS_SIZE,
            1,
        )
    };
    a.content_h = MARGIN + (a.buttons.len() as i32) * (BTN_H + GAP) + MARGIN;
    clamp_scroll();
}

// ---------- reorder animation ----------
fn start_anim() {
    let a = app();
    if !a.anim {
        a.anim = true;
        unsafe { SetTimer(a.hwnd, TIMER_ANIM, 15, None) };
    }
}

fn anim_tick() {
    let a = app();
    let mut rc: RECT = unsafe { zeroed() };
    unsafe { GetClientRect(a.hwnd, &mut rc) };
    let bw = (rc.right - MARGIN * 2).max(100);
    let mut settled = true;
    unsafe {
        let hdwp = BeginDeferWindowPos(a.buttons.len() as i32);
        for i in 0..a.buttons.len() {
            if a.drag.map_or(false, |d| d.idx == i) {
                continue; // dragged button follows the mouse directly
            }
            let target = target_y(i) as f32;
            let dy = target - a.ys[i];
            a.vys[i] = (a.vys[i] + dy * SPRING_STIFFNESS) * SPRING_DAMPING;
            a.ys[i] += a.vys[i];
            if dy.abs() > 0.5 || a.vys[i].abs() > 0.5 {
                settled = false;
            } else {
                a.ys[i] = target;
                a.vys[i] = 0.0;
            }
            DeferWindowPos(
                hdwp,
                a.buttons[i],
                0,
                MARGIN,
                a.ys[i].round() as i32,
                bw,
                BTN_H,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS,
            );
        }
        EndDeferWindowPos(hdwp);
        if settled && a.drag.is_none() {
            KillTimer(a.hwnd, TIMER_ANIM);
            a.anim = false;
        }
    }
}

fn move_item(from: usize, to: usize) {
    let a = app();
    let c = a.conns.remove(from);
    a.conns.insert(to, c);
    let p = a.procs.remove(from);
    a.procs.insert(to, p);
    let b = a.buttons.remove(from);
    a.buttons.insert(to, b);
    let y = a.ys.remove(from);
    a.ys.insert(to, y);
    let v = a.vys.remove(from);
    a.vys.insert(to, v);
}

fn autosize_window() {
    let a = app();
    let n = a.conns.len().max(1) as i32;
    let desired_client_h = MARGIN + n * (BTN_H + GAP) + STRIP_H;
    let mut work: RECT = unsafe { zeroed() };
    unsafe {
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut RECT as *mut c_void, 0)
    };
    let max_h = ((work.bottom - work.top) as f32 * 0.85) as i32;

    let client_w = BTN_W + MARGIN * 2;
    let client_h = desired_client_h.min(max_h);

    let style = unsafe { GetWindowLongW(a.hwnd, GWL_STYLE) } as u32;
    let exstyle = unsafe { GetWindowLongW(a.hwnd, GWL_EXSTYLE) } as u32;
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: client_w,
        bottom: client_h,
    };
    unsafe { AdjustWindowRectEx(&mut rc, style, 0, exstyle) };
    let _ = desired_client_h;
    unsafe {
        SetWindowPos(
            a.hwnd,
            0,
            0,
            0,
            rc.right - rc.left,
            rc.bottom - rc.top,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
    clamp_scroll();
    relayout_plus_only();
}

fn relayout_plus_only() {
    let a = app();
    let mut rc: RECT = unsafe { zeroed() };
    unsafe { GetClientRect(a.hwnd, &mut rc) };
    unsafe {
        MoveWindow(
            a.plus_btn,
            rc.right - PLUS_SIZE - 10,
            rc.bottom - STRIP_H + (STRIP_H - PLUS_SIZE) / 2,
            PLUS_SIZE,
            PLUS_SIZE,
            1,
        )
    };
}

fn clamp_scroll() {
    let a = app();
    let mut rc: RECT = unsafe { zeroed() };
    unsafe { GetClientRect(a.hwnd, &mut rc) };
    let view_h = rc.bottom - STRIP_H;
    let max = (a.content_h - view_h).max(0);
    a.scroll = a.scroll.clamp(0, max);
}

fn do_scroll(delta: i32) {
    let a = app();
    let mut rc: RECT = unsafe { zeroed() };
    unsafe { GetClientRect(a.hwnd, &mut rc) };
    let view_h = rc.bottom - STRIP_H;
    let max = (a.content_h - view_h).max(0);
    let new = (a.scroll + delta).clamp(0, max);
    if new != a.scroll {
        a.scroll = new;
        relayout();
        unsafe { InvalidateRect(a.hwnd, null(), 1) };
    }
}

// ---------- owner draw ----------
unsafe fn draw_gradient(hdc: HDC, rc: &RECT, top: u32, bottom: u32) {
    let verts = [
        TRIVERTEX {
            x: rc.left,
            y: rc.top,
            Red: (((top >> 16) & 0xFF) << 8) as u16,
            Green: ((((top >> 8) & 0xFF) as u32) << 8) as u16,
            Blue: (((top & 0xFF) as u32) << 8) as u16,
            Alpha: 0,
        },
        TRIVERTEX {
            x: rc.right,
            y: rc.bottom,
            Red: (((bottom >> 16) & 0xFF) << 8) as u16,
            Green: ((((bottom >> 8) & 0xFF) as u32) << 8) as u16,
            Blue: (((bottom & 0xFF) as u32) << 8) as u16,
            Alpha: 0,
        },
    ];
    let mesh = GRADIENT_RECT {
        UpperLeft: 0,
        LowerRight: 1,
    };
    GradientFill(
        hdc,
        verts.as_ptr(),
        2,
        &mesh as *const GRADIENT_RECT as *const c_void,
        1,
        GRADIENT_FILL_RECT_V,
    );
}

fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

fn icon_rects(rc: &RECT) -> [RECT; 3] {
    let mut out = [RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    }; 3];
    let right_edge = rc.right - HANDLE_W - 2;
    for (k, slot) in out.iter_mut().enumerate() {
        let l = right_edge - (3 - k as i32) * 20;
        *slot = RECT {
            left: l,
            top: rc.bottom - 21,
            right: l + 18,
            bottom: rc.bottom - 3,
        };
    }
    out
}

unsafe fn draw_button(dis: &DRAWITEMSTRUCT) {
    let a = app();
    let hdc = dis.hDC;
    let rc = dis.rcItem;
    let pressed = (dis.itemState & ODS_SELECTED) != 0;

    let is_plus = dis.CtlID == ID_PLUS as u32;
    let idx = if is_plus {
        0
    } else {
        match a.buttons.iter().position(|&b| b == dis.hwndItem) {
            Some(i) => i,
            None => return,
        }
    };
    let active = !is_plus && idx < a.procs.len() && a.procs[idx] != 0;

    // colors packed as 0xRRGGBB for gradient helper
    let (top, bot, border, text_col) = if active {
        if pressed {
            (0xB8E6B8u32, 0x7CC77C, 0x3E8E3E, rgb(0x0F, 0x3D, 0x0F))
        } else {
            (0xDFF6DF, 0x9FDD9F, 0x4C9A4C, rgb(0x0F, 0x3D, 0x0F))
        }
    } else if pressed {
        (0xC9D3E4, 0xAEBBD2, 0x5A6C8C, rgb(0x10, 0x1A, 0x2B))
    } else {
        (0xF4F7FB, 0xD5DDEA, 0x7A8AA5, rgb(0x10, 0x1A, 0x2B))
    };

    draw_gradient(hdc, &rc, top, bot);

    let pen = CreatePen(PS_SOLID, 1, rgb((border >> 16) as u8, (border >> 8) as u8, border as u8));
    let old_pen = SelectObject(hdc, pen);
    let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
    RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, 6, 6);
    SelectObject(hdc, old_brush);
    SelectObject(hdc, old_pen);
    DeleteObject(pen);

    SetBkMode(hdc, TRANSPARENT as i32);

    if is_plus {
        SetTextColor(hdc, rgb(0x2B, 0x3A, 0x55));
        let old_font = SelectObject(hdc, a.font_plus);
        let mut trc = rc;
        let txt = w("+");
        DrawTextW(
            hdc,
            txt.as_ptr(),
            -1,
            &mut trc,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        SelectObject(hdc, old_font);
        return;
    }

    let c = &a.conns[idx];
    let name = if c.name.is_empty() {
        c.host.clone()
    } else {
        c.name.clone()
    };
    let sub = if active {
        format!("{}  •  connected", c.host)
    } else {
        c.host.clone()
    };

    SetTextColor(hdc, text_col);
    let old_font = SelectObject(hdc, a.font_name);
    let mut top_rc = RECT {
        left: rc.left + 10,
        top: rc.top + 8,
        right: rc.right - HANDLE_W - 4,
        bottom: rc.top + 32,
    };
    let nm = w(&name);
    DrawTextW(
        hdc,
        nm.as_ptr(),
        -1,
        &mut top_rc,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
    );

    SelectObject(hdc, a.font_host);
    SetTextColor(
        hdc,
        if active {
            rgb(0x2E, 0x6B, 0x2E)
        } else {
            rgb(0x5A, 0x66, 0x7A)
        },
    );
    let mut bot_rc = RECT {
        left: rc.left + 10,
        top: rc.top + 32,
        right: rc.right - HANDLE_W - 66,
        bottom: rc.bottom - 6,
    };
    let sb = w(&sub);
    DrawTextW(
        hdc,
        sb.as_ptr(),
        -1,
        &mut bot_rc,
        DT_LEFT | DT_TOP | DT_SINGLELINE | DT_END_ELLIPSIS,
    );
    SelectObject(hdc, old_font);

    // ≡ drag handle on the right side
    let grip_col = if active {
        rgb(0x4C, 0x9A, 0x4C)
    } else {
        rgb(0x8A, 0x97, 0xAC)
    };
    let gpen = CreatePen(PS_SOLID, 2, grip_col);
    let gold = SelectObject(hdc, gpen);
    let cx = rc.right - HANDLE_W / 2 - 4;
    let cy = (rc.top + rc.bottom) / 2;
    for dy in [-6i32, 0, 6] {
        MoveToEx(hdc, cx - 7, cy + dy, null_mut());
        LineTo(hdc, cx + 7, cy + dy);
    }
    SelectObject(hdc, gold);
    DeleteObject(gpen);

    // small action icons (edit / duplicate / delete) bottom-right
    let icons = icon_rects(&rc);
    let glyphs = ["\u{270F}", "\u{1F4CB}", "\u{1F5D1}"];
    SetTextColor(
        hdc,
        if active {
            rgb(0x3E, 0x7A, 0x3E)
        } else {
            rgb(0x7A, 0x86, 0x9C)
        },
    );
    let oldf = SelectObject(hdc, a.font_host);
    for (k, r) in icons.iter().enumerate() {
        let mut rr = *r;
        let g = w(glyphs[k]);
        DrawTextW(
            hdc,
            g.as_ptr(),
            -1,
            &mut rr,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOCLIP,
        );
    }
    SelectObject(hdc, oldf);
}

// ---------- drag-to-reorder + action icons (button subclass) ----------
unsafe fn cursor_and_rect(hwnd: HWND) -> (POINT, RECT) {
    let mut pt: POINT = zeroed();
    GetCursorPos(&mut pt);
    ScreenToClient(hwnd, &mut pt);
    let mut rc: RECT = zeroed();
    GetClientRect(hwnd, &mut rc);
    (pt, rc)
}

fn pt_in(pt: &POINT, r: &RECT) -> bool {
    pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
}

unsafe extern "system" fn btn_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_SETCURSOR => {
            let (pt, rc) = cursor_and_rect(hwnd);
            if pt.x >= rc.right - HANDLE_W && pt.y >= 0 && pt.y <= rc.bottom {
                SetCursor(LoadCursorW(0, IDC_SIZENS));
                return 1;
            }
            if icon_rects(&rc).iter().any(|r| pt_in(&pt, r)) {
                SetCursor(LoadCursorW(0, IDC_HAND));
                return 1;
            }
        }
        WM_LBUTTONDOWN => {
            let a = app();
            if let Some(idx) = a.buttons.iter().position(|&b| b == hwnd) {
                let (pt, rc) = cursor_and_rect(hwnd);
                // tiny action icons: edit / duplicate / delete
                for (k, r) in icon_rects(&rc).iter().enumerate() {
                    if pt_in(&pt, r) {
                        PostMessageW(a.hwnd, WM_APP_ACTION, k + 1, idx as isize);
                        return 0;
                    }
                }
                // ≡ drag handle: start smooth drag
                if pt.x >= rc.right - HANDLE_W {
                    a.drag = Some(Drag {
                        idx,
                        grab_dy: pt.y,
                    });
                    a.vys[idx] = 0.0;
                    SetCapture(hwnd);
                    SetWindowPos(
                        hwnd,
                        0, // HWND_TOP: dragged button floats above siblings
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                    start_anim();
                    return 0;
                }
            }
        }
        WM_MOUSEMOVE => {
            let a = app();
            if let Some(d) = a.drag {
                if GetCapture() == hwnd {
                    let mut pt: POINT = zeroed();
                    GetCursorPos(&mut pt);
                    ScreenToClient(a.hwnd, &mut pt);
                    let mut crc: RECT = zeroed();
                    GetClientRect(a.hwnd, &mut crc);
                    let bw = (crc.right - MARGIN * 2).max(100);
                    let n = a.conns.len() as i32;
                    let min_y = MARGIN - a.scroll - 6;
                    let max_y = MARGIN + (n - 1) * (BTN_H + GAP) - a.scroll + 6;
                    let y = (pt.y - d.grab_dy).clamp(min_y, max_y);
                    SetWindowPos(
                        hwnd,
                        0,
                        MARGIN,
                        y,
                        bw,
                        BTN_H,
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS,
                    );
                    a.ys[d.idx] = y as f32;
                    // which slot does the dragged button's center land in?
                    let center = y + BTN_H / 2;
                    let slot = ((center - MARGIN + a.scroll) / (BTN_H + GAP))
                        .clamp(0, n - 1) as usize;
                    if slot != d.idx {
                        move_item(d.idx, slot);
                        a.drag = Some(Drag {
                            idx: slot,
                            grab_dy: d.grab_dy,
                        });
                        start_anim(); // others spring out of the way
                    }
                    return 0;
                }
            }
        }
        WM_LBUTTONUP => {
            let a = app();
            if a.drag.is_some() {
                a.drag = None;
                ReleaseCapture();
                save_config();
                start_anim(); // dragged button settles into its slot with a bounce
                // repaint everything to clear any leftover drag artifacts
                InvalidateRect(a.hwnd, null(), 1);
                for &b in &a.buttons {
                    InvalidateRect(b, null(), 1);
                }
                return 0;
            }
        }
        _ => {}
    }
    CallWindowProcW(
        std::mem::transmute::<isize, WNDPROC>(OLD_BTN_PROC),
        hwnd,
        msg,
        wparam,
        lparam,
    )
}

// ---------- edit dialog ----------
unsafe fn dlg_add_label(hwnd: HWND, hinst: HINSTANCE, text: &str, x: i32, y: i32, w_: i32) {
    let h = CreateWindowExW(
        0,
        w("STATIC").as_ptr(),
        w(text).as_ptr(),
        WS_CHILD | WS_VISIBLE,
        x,
        y,
        w_,
        18,
        hwnd,
        0 as HMENU,
        hinst,
        null(),
    );
    SendMessageW(h, WM_SETFONT, app().font_host as usize, 1);
}

unsafe fn dlg_add_edit(hwnd: HWND, hinst: HINSTANCE, id: i32, x: i32, y: i32, w_: i32, pass: bool) -> HWND {
    let mut style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | (ES_AUTOHSCROLL as u32);
    if pass {
        style |= ES_PASSWORD as u32;
    }
    let h = CreateWindowExW(
        WS_EX_CLIENTEDGE,
        w("EDIT").as_ptr(),
        null(),
        style,
        x,
        y,
        w_,
        24,
        hwnd,
        id as HMENU,
        hinst,
        null(),
    );
    SendMessageW(h, WM_SETFONT, app().font_host as usize, 1);
    h
}

unsafe fn dlg_get_text(hwnd_dlg: HWND, id: i32) -> String {
    let h = GetDlgItem(hwnd_dlg, id);
    let mut buf = [0u16; 512];
    let n = GetWindowTextW(h, buf.as_mut_ptr(), 512);
    String::from_utf16_lossy(&buf[..n as usize])
}

unsafe extern "system" fn dlg_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let hinst = app().hinst;
            let d = dlg();
            let lx = 16;
            let ex = 16;
            let ew = 300;
            dlg_add_label(hwnd, hinst, "Name", lx, 12, 200);
            dlg_add_edit(hwnd, hinst, IDC_NAME, ex, 32, ew, false);
            dlg_add_label(hwnd, hinst, "Host / IP  (optionally host:port)", lx, 64, 260);
            dlg_add_edit(hwnd, hinst, IDC_HOST, ex, 84, ew, false);
            dlg_add_label(hwnd, hinst, "Username  (DOMAIN\\user or user)", lx, 116, 260);
            dlg_add_edit(hwnd, hinst, IDC_USER, ex, 136, ew, false);
            dlg_add_label(hwnd, hinst, "Password", lx, 168, 200);
            dlg_add_edit(hwnd, hinst, IDC_PASS, ex, 188, ew, true);

            let mk_btn = |text: &str, id: i32, x: i32, def: bool| {
                let style = WS_CHILD
                    | WS_VISIBLE
                    | WS_TABSTOP
                    | if def { BS_DEFPUSHBUTTON as u32 } else { 0 };
                let h = CreateWindowExW(
                    0,
                    w("BUTTON").as_ptr(),
                    w(text).as_ptr(),
                    style,
                    x,
                    228,
                    90,
                    28,
                    hwnd,
                    id as HMENU,
                    hinst,
                    null(),
                );
                SendMessageW(h, WM_SETFONT, app().font_host as usize, 1);
            };
            mk_btn("Save", IDC_OK, 130, true);
            mk_btn("Cancel", IDC_CANCEL, 226, false);

            let init = d.initial.clone();
            SetWindowTextW(GetDlgItem(hwnd, IDC_NAME), w(&init.name).as_ptr());
            SetWindowTextW(GetDlgItem(hwnd, IDC_HOST), w(&init.host).as_ptr());
            SetWindowTextW(GetDlgItem(hwnd, IDC_USER), w(&init.username).as_ptr());
            SetWindowTextW(
                GetDlgItem(hwnd, IDC_PASS),
                w(&unprotect(&init.password_enc)).as_ptr(),
            );
            SetFocus(GetDlgItem(hwnd, IDC_NAME));
            0
        }
        WM_COMMAND => {
            let id = loword(wparam) as i32;
            if id == IDC_OK {
                let host = dlg_get_text(hwnd, IDC_HOST).trim().to_string();
                if host.is_empty() {
                    MessageBoxW(
                        hwnd,
                        w("Host / IP is required.").as_ptr(),
                        w(APP_TITLE).as_ptr(),
                        MB_OK | MB_ICONWARNING,
                    );
                    return 0;
                }
                let name = dlg_get_text(hwnd, IDC_NAME).trim().to_string();
                let user = dlg_get_text(hwnd, IDC_USER).trim().to_string();
                let pass = dlg_get_text(hwnd, IDC_PASS);
                dlg().result = Some(Connection {
                    name: if name.is_empty() { host.clone() } else { name },
                    host,
                    username: user,
                    password_enc: protect(&pass),
                });
                DestroyWindow(hwnd);
            } else if id == IDC_CANCEL {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            dlg().done = true;
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn show_edit_dialog(initial: Connection, title: &str) -> Option<Connection> {
    let a = app();
    unsafe {
        (*addr_of_mut!(DLG)) = Some(DlgState {
            hwnd: 0,
            done: false,
            result: None,
            initial,
        });

        let mut rc = RECT {
            left: 0,
            top: 0,
            right: 332,
            bottom: 268,
        };
        let style = WS_CAPTION | WS_SYSMENU | WS_POPUP;
        AdjustWindowRectEx(&mut rc, style, 0, WS_EX_DLGMODALFRAME);

        // center over parent
        let mut prc: RECT = zeroed();
        GetWindowRect(a.hwnd, &mut prc);
        let x = prc.left + 20;
        let y = prc.top + 60;

        EnableWindow(a.hwnd, 0);
        let hdlg = CreateWindowExW(
            WS_EX_DLGMODALFRAME | WS_EX_TOPMOST,
            w(CLASS_DLG).as_ptr(),
            w(title).as_ptr(),
            style,
            x,
            y,
            rc.right - rc.left,
            rc.bottom - rc.top,
            a.hwnd,
            0 as HMENU,
            a.hinst,
            null(),
        );
        dlg().hwnd = hdlg;
        ShowWindow(hdlg, SW_SHOW);

        let mut msg: MSG = zeroed();
        while !dlg().done && GetMessageW(&mut msg, 0, 0, 0) > 0 {
            if IsDialogMessageW(hdlg, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        EnableWindow(a.hwnd, 1);
        SetActiveWindow(a.hwnd);

        let result = dlg().result.take();
        (*addr_of_mut!(DLG)) = None;
        result
    }
}

// ---------- system tray ----------
unsafe fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid: NOTIFYICONDATAW = zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_ICON_ID;
    nid
}

unsafe fn tray_add(hwnd: HWND, hinst: HINSTANCE) {
    let mut nid = tray_data(hwnd);
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_APP_TRAY;
    nid.hIcon = LoadIconW(hinst, 1 as *const u16);
    let tip = w(APP_TITLE);
    nid.szTip[..tip.len().min(128)].copy_from_slice(&tip[..tip.len().min(128)]);
    Shell_NotifyIconW(NIM_ADD, &nid);
}

unsafe fn tray_remove(hwnd: HWND) {
    let nid = tray_data(hwnd);
    Shell_NotifyIconW(NIM_DELETE, &nid);
}

unsafe fn toggle_main_window() {
    let a = app();
    if IsWindowVisible(a.hwnd) != 0 {
        ShowWindow(a.hwnd, SW_HIDE);
    } else {
        ShowWindow(a.hwnd, SW_SHOW);
        SetForegroundWindow(a.hwnd);
    }
}

unsafe fn show_tray_menu() {
    let a = app();
    let menu = CreatePopupMenu();
    AppendMenuW(menu, MF_STRING, IDM_TRAY_OPEN, w("Open RDP Launcher").as_ptr());
    AppendMenuW(menu, MF_STRING, IDM_TRAY_ABOUT, w("About...").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, IDM_TRAY_EXIT, w("Exit").as_ptr());
    SetMenuDefaultItem(menu, IDM_TRAY_OPEN as u32, 0);

    let mut pt: POINT = zeroed();
    GetCursorPos(&mut pt);
    SetForegroundWindow(a.hwnd);
    let cmd = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
        pt.x,
        pt.y,
        0,
        a.hwnd,
        null(),
    ) as usize;
    PostMessageW(a.hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);

    match cmd {
        IDM_TRAY_OPEN => {
            ShowWindow(a.hwnd, SW_SHOW);
            SetForegroundWindow(a.hwnd);
        }
        IDM_TRAY_ABOUT => show_about(a.hwnd),
        IDM_TRAY_EXIT => {
            DestroyWindow(a.hwnd);
        }
        _ => {}
    }
}

unsafe fn show_about(hwnd: HWND) {
    MessageBoxW(
        hwnd,
        w("RDP Launcher\nby c1t1zen\n\nVersion 1.1.0\n\nOne-click RDP connection launcher.\nLives in the system tray (near the clock) - click the tray icon to show/hide.\nConnections are stored in %APPDATA%\\RdpLauncher\nPasswords are encrypted with Windows DPAPI.")
            .as_ptr(),
        w("About RDP Launcher").as_ptr(),
        MB_OK | MB_ICONINFORMATION,
    );
}

// ---------- context menu ----------
fn show_context_menu(idx: usize, x: i32, y: i32) {
    let a = app();
    unsafe {
        let menu = CreatePopupMenu();
        AppendMenuW(menu, MF_STRING, IDM_CONNECT, w("\u{1F5A5}\u{FE0F}  Connect").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING, IDM_EDIT, w("\u{270F}\u{FE0F}  Edit / Rename...").as_ptr());
        AppendMenuW(menu, MF_STRING, IDM_DUP, w("\u{1F4CB}  Duplicate").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING, IDM_DELETE, w("\u{1F5D1}\u{FE0F}  Delete").as_ptr());

        SetForegroundWindow(a.hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            x,
            y,
            0,
            a.hwnd,
            null(),
        ) as usize;
        DestroyMenu(menu);

        match cmd {
            IDM_CONNECT => launch_connection(idx),
            IDM_EDIT => do_action(idx, ACT_EDIT),
            IDM_DUP => do_action(idx, ACT_DUP),
            IDM_DELETE => do_action(idx, ACT_DELETE),
            _ => {}
        }
    }
}

fn do_action(idx: usize, action: usize) {
    let a = app();
    if idx >= a.conns.len() {
        return;
    }
    match action {
        ACT_EDIT => {
            let initial = a.conns[idx].clone();
            if let Some(updated) = show_edit_dialog(initial, "Edit Connection") {
                a.conns[idx] = updated;
                save_config();
                rebuild_buttons();
            }
        }
        ACT_DUP => {
            let mut cp = a.conns[idx].clone();
            cp.name = format!("{} (copy)", cp.name);
            a.conns.insert(idx + 1, cp);
            a.procs.insert(idx + 1, 0);
            save_config();
            rebuild_buttons();
        }
        ACT_DELETE => {
            let prompt = format!("Delete \"{}\"?", a.conns[idx].name);
            unsafe {
                if MessageBoxW(
                    a.hwnd,
                    w(&prompt).as_ptr(),
                    w(APP_TITLE).as_ptr(),
                    MB_YESNO | MB_ICONQUESTION,
                ) == IDYES
                {
                    if a.procs[idx] != 0 {
                        CloseHandle(a.procs[idx]);
                    }
                    a.conns.remove(idx);
                    a.procs.remove(idx);
                    save_config();
                    rebuild_buttons();
                }
            }
        }
        _ => {}
    }
}

// ---------- main window proc ----------
unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            if hiword(wparam) as u32 == BN_CLICKED {
                let src = lparam as HWND;
                if src == app().plus_btn {
                    if let Some(c) = show_edit_dialog(Connection::default(), "New Connection") {
                        app().conns.push(c);
                        app().procs.push(0);
                        save_config();
                        rebuild_buttons();
                    }
                } else if let Some(idx) = app().buttons.iter().position(|&b| b == src) {
                    launch_connection(idx);
                }
            }
            0
        }
        WM_APP_ACTION => {
            do_action(lparam as usize, wparam);
            0
        }
        WM_APP_TRAY => {
            match (lparam & 0xFFFF) as u32 {
                WM_LBUTTONUP => toggle_main_window(),
                WM_RBUTTONUP => show_tray_menu(),
                _ => {}
            }
            0
        }
        WM_DRAWITEM => {
            let dis = &*(lparam as *const DRAWITEMSTRUCT);
            draw_button(dis);
            1
        }
        WM_CONTEXTMENU => {
            let src = wparam as HWND;
            if let Some(idx) = app().buttons.iter().position(|&b| b == src) {
                let x = (lparam & 0xFFFF) as i16 as i32;
                let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
                show_context_menu(idx, x, y);
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_TIMER => {
            if wparam == TIMER_POLL {
                let a = app();
                for i in 0..a.procs.len() {
                    if a.procs[i] != 0 && WaitForSingleObject(a.procs[i], 0) == WAIT_OBJECT_0 {
                        CloseHandle(a.procs[i]);
                        a.procs[i] = 0;
                        if i < a.buttons.len() {
                            InvalidateRect(a.buttons[i], null(), 1);
                        }
                    }
                }
            } else if wparam == TIMER_ANIM {
                anim_tick();
            }
            0
        }
        WM_MOUSEWHEEL => {
            let delta = (wparam >> 16) as i16 as i32;
            do_scroll(-delta / 120 * (BTN_H + GAP));
            0
        }
        WM_SIZE => {
            relayout();
            0
        }
        WM_SYSCOMMAND => {
            if wparam == IDM_ABOUT {
                show_about(hwnd);
                return 0;
            }
            if wparam & 0xFFF0 == SC_MINIMIZE as usize {
                // minimize hides to tray instead of the taskbar
                ShowWindow(hwnd, SW_HIDE);
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CLOSE => {
            // closing hides to tray; Exit is available from the tray menu
            ShowWindow(hwnd, SW_HIDE);
            0
        }
        WM_ERASEBKGND => {
            let hdc = wparam as HDC;
            let mut rc: RECT = zeroed();
            GetClientRect(hwnd, &mut rc);
            let brush = CreateSolidBrush(rgb(0xEF, 0xF2, 0xF7));
            FillRect(hdc, &rc, brush);
            DeleteObject(brush);
            // separator line above the bottom strip
            let pen = CreatePen(PS_SOLID, 1, rgb(0xC5, 0xCE, 0xDC));
            let old = SelectObject(hdc, pen);
            MoveToEx(hdc, 0, rc.bottom - STRIP_H, null_mut());
            LineTo(hdc, rc.right, rc.bottom - STRIP_H);
            SelectObject(hdc, old);
            DeleteObject(pen);
            1
        }
        WM_DESTROY => {
            tray_remove(hwnd);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ---------- entry ----------
fn main() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE);
        let hinst = GetModuleHandleW(null());

        // register classes
        let mut wc: WNDCLASSW = zeroed();
        wc.lpfnWndProc = Some(wnd_proc);
        wc.hInstance = hinst;
        wc.hCursor = LoadCursorW(0, IDC_ARROW);
        wc.hbrBackground = 0;
        wc.hIcon = LoadIconW(hinst, 1 as *const u16); // embedded resource icon
        let cls = w(CLASS_MAIN);
        wc.lpszClassName = cls.as_ptr();
        RegisterClassW(&wc);

        let mut wcd: WNDCLASSW = zeroed();
        wcd.lpfnWndProc = Some(dlg_proc);
        wcd.hInstance = hinst;
        wcd.hCursor = LoadCursorW(0, IDC_ARROW);
        wcd.hbrBackground = (COLOR_BTNFACE + 1) as HBRUSH;
        let clsd = w(CLASS_DLG);
        wcd.lpszClassName = clsd.as_ptr();
        RegisterClassW(&wcd);

        // fonts
        let mk_font = |h: i32, weight: i32| {
            CreateFontW(
                h,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                (DEFAULT_PITCH | FF_DONTCARE) as u32,
                w("Segoe UI").as_ptr(),
            )
        };
        let font_name = mk_font(-16, FW_SEMIBOLD as i32);
        let font_host = mk_font(-13, FW_NORMAL as i32);
        let font_plus = mk_font(-20, FW_BOLD as i32);

        // main window (always on top, no scrollbar - resizes with content)
        // WS_CLIPCHILDREN prevents the background erase from painting over the
        // buttons (which caused visual corruption during drag-reorder).
        // WS_EX_TOOLWINDOW keeps the app off the taskbar; it lives in the
        // system tray (near the clock) instead.
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            cls.as_ptr(),
            w(APP_TITLE).as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            400,
            300,
            0,
            0 as HMENU,
            hinst,
            null(),
        );

        let plus_btn = CreateWindowExW(
            0,
            w("BUTTON").as_ptr(),
            null(),
            WS_CHILD | WS_VISIBLE | (BS_OWNERDRAW as u32),
            0,
            0,
            PLUS_SIZE,
            PLUS_SIZE,
            hwnd,
            ID_PLUS as HMENU,
            hinst,
            null(),
        );

        (*addr_of_mut!(APP)) = Some(App {
            hwnd,
            hinst,
            conns: load_config(),
            buttons: Vec::new(),
            procs: Vec::new(),
            plus_btn,
            font_name,
            font_host,
            font_plus,
            scroll: 0,
            content_h: 0,
            ys: Vec::new(),
            vys: Vec::new(),
            drag: None,
            anim: false,
        });

        // About entry in the system (title-bar) menu
        let sysmenu = GetSystemMenu(hwnd, 0);
        AppendMenuW(sysmenu, MF_SEPARATOR, 0, null());
        AppendMenuW(
            sysmenu,
            MF_STRING,
            IDM_ABOUT,
            w("\u{2139}\u{FE0F}  About RDP Launcher...").as_ptr(),
        );

        rebuild_buttons();
        tray_add(hwnd, hinst);
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        SetTimer(hwnd, TIMER_POLL, 1000, None);

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

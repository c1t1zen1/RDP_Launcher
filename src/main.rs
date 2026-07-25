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
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, DRAWITEMSTRUCT, ICC_BAR_CLASSES, ICC_STANDARD_CLASSES,
    INITCOMMONCONTROLSEX, ODS_SELECTED, TTF_IDISHWND, TTF_SUBCLASS, TTM_ADDTOOLW, TTM_DELTOOLW,
    TTM_NEWTOOLRECTW, TTM_SETMAXTIPWIDTH, TTM_UPDATETIPTEXTW, TTS_ALWAYSTIP, TTS_NOPREFIX,
    TTTOOLINFOW, WM_MOUSELEAVE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetCapture, ReleaseCapture, SetActiveWindow, SetCapture, SetFocus,
    TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

// ---------- constants ----------
const APP_TITLE: &str = "RDP Launcher";
const CLASS_MAIN: &str = "RdpLauncherMain";
const CLASS_DLG: &str = "RdpLauncherDlg";
const CLASS_ABOUT: &str = "RdpLauncherAbout";
const APP_VERSION: &str = "1.2.0";

const BTN_W: i32 = 300;
const BTN_H: i32 = 56;
const MARGIN: i32 = 12;
const GAP: i32 = 8;
const HEADER_H: i32 = 32; // top strip holding minimize / pin / about
const HDR_BTN: i32 = 24;
const HDR_PAD: i32 = 6;
const STRIP_H: i32 = 44; // bottom strip holding the + button
const PLUS_SIZE: i32 = 30;

const HANDLE_W: i32 = 34; // width of the ≡ drag-handle zone on each button

const GLYPH_MINIMIZE: &str = "\u{E921}";
const GLYPH_PIN: &str = "\u{E718}";
const GLYPH_UNPIN: &str = "\u{E77A}";
const GLYPH_INFO: &str = "\u{E946}";
const GLYPH_ADD: &str = "\u{E710}";
const GLYPH_EDIT: &str = "\u{E70F}";
const GLYPH_COPY: &str = "\u{E8C8}";
const GLYPH_DELETE: &str = "\u{E74D}";

const TIP_BODY: usize = 1;
const TIP_EDIT: usize = 2;
const TIP_DUP: usize = 3;
const TIP_DEL: usize = 4;
const TIP_GRIP: usize = 5;

const ID_MIN: i32 = 996;
const ID_TOP: i32 = 997;
const ID_INFO: i32 = 998;
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
const IDC_ABOUT_TEXT: i32 = 3020;
const IDC_ABOUT_OK: i32 = 3021;

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
    min_btn: HWND,
    top_btn: HWND,
    info_btn: HWND,
    tooltip: HWND,
    tips: Vec<Vec<u16>>, // keeps tooltip strings alive
    hot: HWND,           // hovered flat button (0 = none)
    topmost: bool,
    font_name: HFONT,
    font_host: HFONT,
    font_icon: HFONT,
    font_icon_sm: HFONT,
    scroll: i32,
    content_h: i32,
    ys: Vec<f32>,  // animated current y per button
    vys: Vec<f32>, // velocity per button
    drag: Option<Drag>,
    anim: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct Settings {
    #[serde(default = "default_true")]
    always_on_top: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            always_on_top: true,
        }
    }
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
fn config_dir() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    let dir = PathBuf::from(base).join("RdpLauncher");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn config_path() -> PathBuf {
    config_dir().join("connections.json")
}

fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

fn load_settings() -> Settings {
    fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_settings() {
    let s = Settings {
        always_on_top: app().topmost,
    };
    if let Ok(json) = serde_json::to_string_pretty(&s) {
        let _ = fs::write(settings_path(), json);
    }
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

// ---------- RDP session focus helpers ----------
struct FindRdpContext {
    tokens: Vec<String>,
    hwnd: HWND,
}

unsafe extern "system" fn find_rdp_enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam as *mut FindRdpContext);
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    let mut class = [0u16; 256];
    let n = GetClassNameW(hwnd, class.as_mut_ptr(), 256);
    if n == 0 {
        return 1;
    }
    let class_str = String::from_utf16_lossy(&class[..n as usize]);
    if class_str.to_lowercase() != "tscshellcontainerclass" {
        return 1;
    }
    let len = GetWindowTextLengthW(hwnd);
    if len == 0 {
        return 1;
    }
    let mut text = vec![0u16; (len + 1) as usize];
    let m = GetWindowTextW(hwnd, text.as_mut_ptr(), len + 1);
    if m == 0 {
        return 1;
    }
    let title = String::from_utf16_lossy(&text[..m as usize]).to_lowercase();
    for token in &ctx.tokens {
        if !token.is_empty() && title.contains(token) {
            ctx.hwnd = hwnd;
            return 0;
        }
    }
    1
}

unsafe fn find_rdp_window(c: &Connection) -> Option<HWND> {
    let name = c.name.trim().to_lowercase();
    let host = c.host.trim().to_lowercase();
    let bare = c.host.split(':').next().unwrap_or(&c.host).trim().to_lowercase();
    let mut tokens = Vec::new();
    for t in [&name, &host, &bare] {
        if !t.is_empty() && !tokens.contains(&t.to_string()) {
            tokens.push(t.to_string());
        }
    }
    let mut ctx = FindRdpContext { tokens, hwnd: 0 };
    EnumWindows(Some(find_rdp_enum_proc), &mut ctx as *mut _ as LPARAM);
    if ctx.hwnd != 0 {
        Some(ctx.hwnd)
    } else {
        None
    }
}

unsafe fn focus_existing_rdp(idx: usize) -> bool {
    let c = app().conns[idx].clone();
    if let Some(hwnd) = find_rdp_window(&c) {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        } else if IsWindowVisible(hwnd) == 0 {
            ShowWindow(hwnd, SW_SHOW);
        }
        SetForegroundWindow(hwnd);
        BringWindowToTop(hwnd);
        SetActiveWindow(hwnd);
        true
    } else {
        false
    }
}

unsafe fn is_process_active(idx: usize) -> bool {
    let a = app();
    if idx >= a.procs.len() || a.procs[idx] == 0 {
        return false;
    }
    let h = a.procs[idx];
    if WaitForSingleObject(h, 0) == WAIT_OBJECT_0 {
        CloseHandle(h);
        a.procs[idx] = 0;
        if idx < a.buttons.len() {
            InvalidateRect(a.buttons[idx], null(), 1);
        }
        false
    } else {
        true
    }
}

fn launch_connection(idx: usize) {
    unsafe {
        // If this session is already open, bring the existing window to the foreground.
        if focus_existing_rdp(idx) {
            return;
        }
        // If the mstsc we launched is still starting, ignore repeated clicks.
        if is_process_active(idx) {
            return;
        }
    }

    let c = app().conns[idx].clone();
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

        let a = app();
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
        unsafe {
            tip_remove_all(b);
            DestroyWindow(b)
        };
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
    for i in 0..a.buttons.len() {
        unsafe { add_item_tips(a.buttons[i], i) };
    }
}

fn target_y(i: usize) -> i32 {
    HEADER_H + MARGIN + (i as i32) * (BTN_H + GAP) - app().scroll
}

fn layout_header() {
    let a = app();
    let mut rc: RECT = unsafe { zeroed() };
    unsafe { GetClientRect(a.hwnd, &mut rc) };
    let y = (HEADER_H - HDR_BTN) / 2;
    let mut x = rc.right - HDR_PAD - HDR_BTN;
    for &h in &[a.min_btn, a.top_btn, a.info_btn] {
        unsafe { MoveWindow(h, x, y, HDR_BTN, HDR_BTN, 1) };
        x -= HDR_BTN + 2;
    }
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
    layout_header();
    for &b in &a.buttons {
        unsafe { update_item_tips(b) };
    }
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
    let desired_client_h = HEADER_H + MARGIN + n * (BTN_H + GAP) + STRIP_H;
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
    layout_header();
}

fn clamp_scroll() {
    let a = app();
    let mut rc: RECT = unsafe { zeroed() };
    unsafe { GetClientRect(a.hwnd, &mut rc) };
    let view_h = rc.bottom - STRIP_H - HEADER_H;
    let max = (a.content_h - view_h).max(0);
    a.scroll = a.scroll.clamp(0, max);
}

fn do_scroll(delta: i32) {
    let a = app();
    let mut rc: RECT = unsafe { zeroed() };
    unsafe { GetClientRect(a.hwnd, &mut rc) };
    let view_h = rc.bottom - STRIP_H - HEADER_H;
    let max = (a.content_h - view_h).max(0);
    let new = (a.scroll + delta).clamp(0, max);
    if new != a.scroll {
        a.scroll = new;
        relayout();
        unsafe { InvalidateRect(a.hwnd, null(), 1) };
    }
}

// ---------- tooltips ----------
fn tip_struct_size() -> u32 {
    (size_of::<TTTOOLINFOW>() - size_of::<*mut c_void>()) as u32
}

unsafe fn tip_for_hwnd(target: HWND, text: &str) {
    let a = app();
    if a.tooltip == 0 || target == 0 {
        return;
    }
    let mut buf = w(text);
    let mut ti: TTTOOLINFOW = zeroed();
    ti.cbSize = tip_struct_size();
    ti.uFlags = TTF_IDISHWND | TTF_SUBCLASS;
    ti.hwnd = a.hwnd;
    ti.uId = target as usize;
    ti.lpszText = buf.as_mut_ptr();
    SendMessageW(
        a.tooltip,
        TTM_ADDTOOLW,
        0,
        &ti as *const TTTOOLINFOW as isize,
    );
    a.tips.push(buf);
}

unsafe fn tip_for_rect(target: HWND, id: usize, rect: RECT, text: &str) {
    let a = app();
    if a.tooltip == 0 || target == 0 {
        return;
    }
    let mut buf = w(text);
    let mut ti: TTTOOLINFOW = zeroed();
    ti.cbSize = tip_struct_size();
    ti.uFlags = TTF_SUBCLASS;
    ti.hwnd = target;
    ti.uId = id;
    ti.rect = rect;
    ti.lpszText = buf.as_mut_ptr();
    SendMessageW(
        a.tooltip,
        TTM_ADDTOOLW,
        0,
        &ti as *const TTTOOLINFOW as isize,
    );
    a.tips.push(buf);
}

unsafe fn tip_set_rect(target: HWND, id: usize, rect: RECT) {
    let a = app();
    if a.tooltip == 0 || target == 0 {
        return;
    }
    let mut ti: TTTOOLINFOW = zeroed();
    ti.cbSize = tip_struct_size();
    ti.hwnd = target;
    ti.uId = id;
    ti.rect = rect;
    SendMessageW(
        a.tooltip,
        TTM_NEWTOOLRECTW,
        0,
        &ti as *const TTTOOLINFOW as isize,
    );
}

unsafe fn tip_set_text(target: HWND, text: &str) {
    let a = app();
    if a.tooltip == 0 || target == 0 {
        return;
    }
    let mut buf = w(text);
    let mut ti: TTTOOLINFOW = zeroed();
    ti.cbSize = tip_struct_size();
    ti.uFlags = TTF_IDISHWND | TTF_SUBCLASS;
    ti.hwnd = a.hwnd;
    ti.uId = target as usize;
    ti.lpszText = buf.as_mut_ptr();
    SendMessageW(
        a.tooltip,
        TTM_UPDATETIPTEXTW,
        0,
        &ti as *const TTTOOLINFOW as isize,
    );
    a.tips.push(buf);
}

unsafe fn tip_remove_all(target: HWND) {
    let a = app();
    if a.tooltip == 0 || target == 0 {
        return;
    }
    for id in [TIP_BODY, TIP_EDIT, TIP_DUP, TIP_DEL, TIP_GRIP] {
        let mut ti: TTTOOLINFOW = zeroed();
        ti.cbSize = tip_struct_size();
        ti.hwnd = target;
        ti.uId = id;
        SendMessageW(
            a.tooltip,
            TTM_DELTOOLW,
            0,
            &ti as *const TTTOOLINFOW as isize,
        );
    }
}

fn item_tip_rects(rc: &RECT) -> [(usize, RECT); 5] {
    let icons = icon_rects(rc);
    [
        (
            TIP_BODY,
            RECT {
                left: 0,
                top: 0,
                right: (icons[0].left - 2).max(1),
                bottom: rc.bottom,
            },
        ),
        (TIP_EDIT, icons[0]),
        (TIP_DUP, icons[1]),
        (TIP_DEL, icons[2]),
        (
            TIP_GRIP,
            RECT {
                left: rc.right - HANDLE_W,
                top: 0,
                right: rc.right,
                bottom: rc.bottom,
            },
        ),
    ]
}

unsafe fn add_item_tips(btn: HWND, idx: usize) {
    let a = app();
    let c = &a.conns[idx];
    let host = if c.host.is_empty() {
        "this computer".to_string()
    } else {
        c.host.clone()
    };
    let body = format!(
        "Click to connect to {}\nRight-click for more options",
        host
    );
    let mut rc: RECT = zeroed();
    GetClientRect(btn, &mut rc);
    for (id, r) in item_tip_rects(&rc) {
        let text = match id {
            TIP_EDIT => "Edit this connection (name, host, username, password)",
            TIP_DUP => "Duplicate this connection",
            TIP_DEL => "Delete this connection",
            TIP_GRIP => "Drag up or down to reorder the list",
            _ => body.as_str(),
        };
        tip_for_rect(btn, id, r, text);
    }
}

unsafe fn update_item_tips(btn: HWND) {
    let mut rc: RECT = zeroed();
    GetClientRect(btn, &mut rc);
    for (id, r) in item_tip_rects(&rc) {
        tip_set_rect(btn, id, r);
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

    let ctl = dis.CtlID as i32;
    if ctl == ID_MIN || ctl == ID_TOP || ctl == ID_INFO {
        draw_header_button(hdc, &rc, ctl, pressed, dis.hwndItem == a.hot);
        return;
    }

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
    } else if dis.hwndItem == a.hot {
        (0xFFFFFF, 0xE3E9F3, 0x6C7E9B, rgb(0x10, 0x1A, 0x2B))
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
        draw_glyph(hdc, &rc, GLYPH_ADD, a.font_icon, -1);
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
    let glyphs = [GLYPH_EDIT, GLYPH_COPY, GLYPH_DELETE];
    SetTextColor(
        hdc,
        if active {
            rgb(0x3E, 0x7A, 0x3E)
        } else {
            rgb(0x64, 0x71, 0x89)
        },
    );
    for (k, r) in icons.iter().enumerate() {
        draw_glyph(hdc, r, glyphs[k], a.font_icon_sm, -1);
    }
}

unsafe fn draw_glyph(hdc: HDC, rc: &RECT, glyph: &str, font: HFONT, dy: i32) {
    let old_font = SelectObject(hdc, font);
    let mut trc = RECT {
        left: rc.left,
        top: rc.top + dy,
        right: rc.right,
        bottom: rc.bottom + dy,
    };
    let txt = w(glyph);
    DrawTextW(
        hdc,
        txt.as_ptr(),
        -1,
        &mut trc,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOCLIP,
    );
    SelectObject(hdc, old_font);
}

unsafe fn draw_header_button(hdc: HDC, rc: &RECT, ctl: i32, pressed: bool, hot: bool) {
    let a = app();
    let bg = if pressed {
        rgb(0xBF, 0xCB, 0xDE)
    } else if hot {
        rgb(0xDD, 0xE5, 0xF1)
    } else {
        rgb(0xE7, 0xEC, 0xF4)
    };
    let brush = CreateSolidBrush(bg);
    FillRect(hdc, rc, brush);
    DeleteObject(brush);

    SetBkMode(hdc, TRANSPARENT as i32);
    let (glyph, col) = match ctl {
        ID_MIN => (GLYPH_MINIMIZE, rgb(0x2B, 0x3A, 0x55)),
        ID_TOP => {
            if a.topmost {
                (GLYPH_PIN, rgb(0x0B, 0x63, 0xCE))
            } else {
                (GLYPH_UNPIN, rgb(0x5A, 0x66, 0x7A))
            }
        }
        _ => (GLYPH_INFO, rgb(0x2B, 0x3A, 0x55)),
    };
    SetTextColor(hdc, col);
    draw_glyph(hdc, rc, glyph, a.font_icon_sm, -1);
}

fn topmost_tip(on: bool) -> &'static str {
    if on {
        "Always on top: ON\nThe window stays above other windows.\nClick to let other windows cover it."
    } else {
        "Always on top: OFF\nOther windows can cover this one, like a normal app.\nClick to keep it above everything."
    }
}

fn set_topmost(on: bool) {
    let a = app();
    a.topmost = on;
    unsafe {
        SetWindowPos(
            a.hwnd,
            if on { HWND_TOPMOST } else { HWND_NOTOPMOST },
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        InvalidateRect(a.top_btn, null(), 1);
        tip_set_text(a.top_btn, topmost_tip(on));
    }
    save_settings();
}

// ---------- flat button hover tracking ----------
static mut OLD_FLAT_PROC: isize = 0;

unsafe extern "system" fn flat_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            let a = app();
            if a.hot != hwnd {
                let prev = a.hot;
                a.hot = hwnd;
                if prev != 0 {
                    InvalidateRect(prev, null(), 1);
                }
                InvalidateRect(hwnd, null(), 1);
            }
            let mut tme: TRACKMOUSEEVENT = zeroed();
            tme.cbSize = size_of::<TRACKMOUSEEVENT>() as u32;
            tme.dwFlags = TME_LEAVE;
            tme.hwndTrack = hwnd;
            tme.dwHoverTime = 0;
            TrackMouseEvent(&mut tme);
        }
        WM_MOUSELEAVE => {
            let a = app();
            if a.hot == hwnd {
                a.hot = 0;
                InvalidateRect(hwnd, null(), 1);
            }
        }
        _ => {}
    }
    CallWindowProcW(
        std::mem::transmute::<isize, WNDPROC>(OLD_FLAT_PROC),
        hwnd,
        msg,
        wparam,
        lparam,
    )
}

unsafe fn make_flat_button(parent: HWND, hinst: HINSTANCE, id: i32, size: i32) -> HWND {
    let h = CreateWindowExW(
        0,
        w("BUTTON").as_ptr(),
        null(),
        WS_CHILD | WS_VISIBLE | (BS_OWNERDRAW as u32),
        0,
        0,
        size,
        size,
        parent,
        id as HMENU,
        hinst,
        null(),
    );
    let old = SetWindowLongPtrW(h, GWLP_WNDPROC, flat_subclass_proc as *const () as isize);
    if OLD_FLAT_PROC == 0 {
        OLD_FLAT_PROC = old;
    }
    h
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

// ---------- about window / user manual ----------
static mut ABOUT_HWND: HWND = 0;

fn manual_text() -> String {
    let lines: Vec<String> = vec![
        format!("RDP Launcher {}   -   by c1t1zen", APP_VERSION),
        "A one-click launcher for your Remote Desktop connections.".into(),
        "".into(),
        "===============================================".into(),
        "1. NEW TO REMOTE DESKTOP?".into(),
        "===============================================".into(),
        "Remote Desktop (RDP) lets you sign in to another Windows computer over the".into(),
        "network and use its desktop as if you were sitting in front of it. Windows".into(),
        "already ships the client program, mstsc.exe. RDP Launcher simply keeps your".into(),
        "computers in a tidy list of buttons and starts mstsc.exe for you with the".into(),
        "right settings and saved credentials.".into(),
        "".into(),
        "Before your first connection you need:".into(),
        "  * The remote computer's name or IP address (for example PC-OFFICE or".into(),
        "    192.168.1.50). If it uses a non-standard port, write host:port.".into(),
        "  * Remote Desktop enabled on that computer:".into(),
        "    Settings > System > Remote Desktop > On.".into(),
        "  * A user account on that computer that is allowed to connect".into(),
        "    (an administrator, or a member of Remote Desktop Users).".into(),
        "  * Network access to it - same LAN, or a VPN, or a forwarded port.".into(),
        "".into(),
        "===============================================".into(),
        "2. QUICK START".into(),
        "===============================================".into(),
        "  1) Click the + button in the bottom-right corner.".into(),
        "  2) Fill in the form:".into(),
        "       Name      - any label you like, e.g. Office PC".into(),
        "       Host / IP  - the computer's name, IP, or host:port".into(),
        "       Username  - user, DOMAIN\\user, or user@domain (optional)".into(),
        "       Password  - optional; leave blank to be asked every time".into(),
        "  3) Click Save. A button appears in the list.".into(),
        "  4) Click that button to connect. The remote desktop opens full screen.".into(),
        "     Press Ctrl+Alt+Break to leave full screen, or close the session window".into(),
        "     to disconnect.".into(),
        "".into(),
        "A connection button turns green while its session is running. Clicking a".into(),
        "green button brings the already-open session back to the front instead of".into(),
        "starting a second one.".into(),
        "".into(),
        "===============================================".into(),
        "3. THE BUTTONS AT THE TOP".into(),
        "===============================================".into(),
        "  Minimize (-)   Hides the window to the system tray next to the clock.".into(),
        "                 Click the tray icon to bring it back.".into(),
        "  Pin            Toggles 'always on top'. When it is ON (blue pin) the".into(),
        "                 window floats above every other window. When it is OFF".into(),
        "                 other programs can cover it, like a normal Windows app.".into(),
        "                 The choice is remembered between runs.".into(),
        "  i              Opens this About / manual window.".into(),
        "".into(),
        "Hover over anything in the app to see a short description.".into(),
        "".into(),
        "===============================================".into(),
        "4. MANAGING CONNECTIONS".into(),
        "===============================================".into(),
        "Each connection button carries three small icons in its bottom-right corner:".into(),
        "  Pencil   Edit the connection (name, host, username, password).".into(),
        "  Copy     Duplicate it - handy for several accounts on one server.".into(),
        "  Bin      Delete it (you will be asked to confirm).".into(),
        "".into(),
        "Right-click a button for the same actions plus Connect.".into(),
        "".into(),
        "To reorder the list, press and hold the three-line grip on the right edge of".into(),
        "a button and drag it up or down. The other buttons glide out of the way and".into(),
        "the new order is saved automatically.".into(),
        "".into(),
        "If the list is taller than the window, scroll it with the mouse wheel.".into(),
        "".into(),
        "===============================================".into(),
        "5. TRAY ICON".into(),
        "===============================================".into(),
        "The app lives in the system tray instead of the taskbar.".into(),
        "  Left-click the tray icon  - show or hide the window.".into(),
        "  Right-click               - Open, About, or Exit.".into(),
        "Closing the window with the X only hides it; use Exit in the tray menu to".into(),
        "quit completely.".into(),
        "".into(),
        "===============================================".into(),
        "6. WHERE YOUR DATA IS STORED".into(),
        "===============================================".into(),
        "  %APPDATA%\\RdpLauncher\\connections.json   your connection list".into(),
        "  %APPDATA%\\RdpLauncher\\settings.json      always-on-top preference".into(),
        "".into(),
        "Passwords are never stored as plain text. They are encrypted with the".into(),
        "Windows Data Protection API (DPAPI), which ties them to your Windows user".into(),
        "account on this computer - nobody else can read them. When you connect, the".into(),
        "credential is handed to Windows with cmdkey so mstsc can sign in for you.".into(),
        "".into(),
        "===============================================".into(),
        "7. TROUBLESHOOTING".into(),
        "===============================================".into(),
        "  'The remote computer could not be found'".into(),
        "      Check spelling of the host name, or use the IP address instead.".into(),
        "  Connection times out".into(),
        "      Remote Desktop may be off, a firewall may block TCP port 3389, or you".into(),
        "      are not on the same network / VPN.".into(),
        "  It keeps asking for a password".into(),
        "      Edit the connection and re-enter username and password. Use the full".into(),
        "      DOMAIN\\user form for domain accounts.".into(),
        "  'Your credentials did not work'".into(),
        "      The account may lack Remote Desktop permission on the remote machine.".into(),
        "  Session opens but the screen is black for a moment".into(),
        "      Normal - the remote desktop is still loading.".into(),
        "".into(),
        "===============================================".into(),
        "8. GOOD PRACTICE".into(),
        "===============================================".into(),
        "Never expose port 3389 directly to the internet. Use a VPN, an RD Gateway,".into(),
        "or SSH tunnelling, and always use strong passwords.".into(),
        "".into(),
        "-----------------------------------------------".into(),
        "Copyright (c) 2026 c1t1zen. Released under the MIT licence.".into(),
    ];
    lines.join("\r\n")
}

unsafe fn layout_about(hwnd: HWND) {
    let mut rc: RECT = zeroed();
    GetClientRect(hwnd, &mut rc);
    let edit = GetDlgItem(hwnd, IDC_ABOUT_TEXT);
    let ok = GetDlgItem(hwnd, IDC_ABOUT_OK);
    MoveWindow(edit, 12, 12, rc.right - 24, rc.bottom - 58, 1);
    MoveWindow(ok, rc.right - 102, rc.bottom - 38, 90, 26, 1);
}

unsafe extern "system" fn about_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let hinst = app().hinst;
            let edit = CreateWindowExW(
                WS_EX_CLIENTEDGE,
                w("EDIT").as_ptr(),
                w(&manual_text()).as_ptr(),
                WS_CHILD
                    | WS_VISIBLE
                    | WS_VSCROLL
                    | WS_TABSTOP
                    | ((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32),
                12,
                12,
                100,
                100,
                hwnd,
                IDC_ABOUT_TEXT as HMENU,
                hinst,
                null(),
            );
            SendMessageW(edit, WM_SETFONT, app().font_host as usize, 1);
            let ok = CreateWindowExW(
                0,
                w("BUTTON").as_ptr(),
                w("Close").as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | (BS_DEFPUSHBUTTON as u32),
                0,
                0,
                90,
                26,
                hwnd,
                IDC_ABOUT_OK as HMENU,
                hinst,
                null(),
            );
            SendMessageW(ok, WM_SETFONT, app().font_host as usize, 1);
            layout_about(hwnd);
            SetFocus(ok);
            0
        }
        WM_SIZE => {
            layout_about(hwnd);
            0
        }
        WM_COMMAND => {
            if loword(wparam) as i32 == IDC_ABOUT_OK {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            ABOUT_HWND = 0;
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn show_about(_hwnd: HWND) {
    let a = app();
    if ABOUT_HWND != 0 {
        ShowWindow(ABOUT_HWND, SW_SHOW);
        SetForegroundWindow(ABOUT_HWND);
        return;
    }
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_CLIPCHILDREN;
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: 560,
        bottom: 540,
    };
    AdjustWindowRectEx(&mut rc, style, 0, 0);

    let mut prc: RECT = zeroed();
    GetWindowRect(a.hwnd, &mut prc);
    let x = prc.left + 30;
    let y = (prc.top + 30).max(0);

    let exstyle = if a.topmost { WS_EX_TOPMOST } else { 0 };
    ABOUT_HWND = CreateWindowExW(
        exstyle,
        w(CLASS_ABOUT).as_ptr(),
        w("About RDP Launcher").as_ptr(),
        style,
        x,
        y,
        rc.right - rc.left,
        rc.bottom - rc.top,
        0,
        0 as HMENU,
        a.hinst,
        null(),
    );
    ShowWindow(ABOUT_HWND, SW_SHOW);
    SetForegroundWindow(ABOUT_HWND);
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
                } else if src == app().min_btn {
                    ShowWindow(hwnd, SW_HIDE);
                } else if src == app().top_btn {
                    set_topmost(!app().topmost);
                } else if src == app().info_btn {
                    show_about(hwnd);
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
            // top header strip
            let hdr_rc = RECT {
                left: 0,
                top: 0,
                right: rc.right,
                bottom: HEADER_H,
            };
            let hbr = CreateSolidBrush(rgb(0xE7, 0xEC, 0xF4));
            FillRect(hdc, &hdr_rc, hbr);
            DeleteObject(hbr);
            // separator lines below the header and above the bottom strip
            let pen = CreatePen(PS_SOLID, 1, rgb(0xC5, 0xCE, 0xDC));
            let old = SelectObject(hdc, pen);
            MoveToEx(hdc, 0, HEADER_H - 1, null_mut());
            LineTo(hdc, rc.right, HEADER_H - 1);
            MoveToEx(hdc, 0, rc.bottom - STRIP_H, null_mut());
            LineTo(hdc, rc.right, rc.bottom - STRIP_H);
            SelectObject(hdc, old);
            DeleteObject(pen);

            SetBkMode(hdc, TRANSPARENT as i32);
            let a = app();
            let oldf = SelectObject(hdc, a.font_host);
            SetTextColor(hdc, rgb(0x44, 0x50, 0x66));
            let mut trc = RECT {
                left: 10,
                top: 0,
                right: rc.right - HDR_BTN * 3 - HDR_PAD * 2,
                bottom: HEADER_H - 1,
            };
            let n = a.conns.len();
            let label = match n {
                0 => "No connections yet - click + to add one".to_string(),
                1 => "1 connection".to_string(),
                _ => format!("{} connections", n),
            };
            let title = w(&label);
            DrawTextW(
                hdc,
                title.as_ptr(),
                -1,
                &mut trc,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
            );
            SelectObject(hdc, oldf);
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

        let icce = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_BAR_CLASSES,
        };
        InitCommonControlsEx(&icce);

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

        let mut wca: WNDCLASSW = zeroed();
        wca.lpfnWndProc = Some(about_proc);
        wca.hInstance = hinst;
        wca.hCursor = LoadCursorW(0, IDC_ARROW);
        wca.hbrBackground = (COLOR_BTNFACE + 1) as HBRUSH;
        wca.hIcon = LoadIconW(hinst, 1 as *const u16);
        let clsa = w(CLASS_ABOUT);
        wca.lpszClassName = clsa.as_ptr();
        RegisterClassW(&wca);

        // fonts
        let mk_font = |h: i32, weight: i32, face: &str| {
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
                w(face).as_ptr(),
            )
        };
        let font_name = mk_font(-16, FW_SEMIBOLD as i32, "Segoe UI");
        let font_host = mk_font(-13, FW_NORMAL as i32, "Segoe UI");
        let font_icon = mk_font(-15, FW_NORMAL as i32, "Segoe MDL2 Assets");
        let font_icon_sm = mk_font(-12, FW_NORMAL as i32, "Segoe MDL2 Assets");

        let settings = load_settings();

        // main window (always on top, no scrollbar - resizes with content)
        // WS_CLIPCHILDREN prevents the background erase from painting over the
        // buttons (which caused visual corruption during drag-reorder).
        // WS_EX_TOOLWINDOW keeps the app off the taskbar; it lives in the
        // system tray (near the clock) instead.
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
        let exstyle = if settings.always_on_top {
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW
        } else {
            WS_EX_TOOLWINDOW
        };
        let hwnd = CreateWindowExW(
            exstyle,
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

        let plus_btn = make_flat_button(hwnd, hinst, ID_PLUS, PLUS_SIZE);
        let min_btn = make_flat_button(hwnd, hinst, ID_MIN, HDR_BTN);
        let top_btn = make_flat_button(hwnd, hinst, ID_TOP, HDR_BTN);
        let info_btn = make_flat_button(hwnd, hinst, ID_INFO, HDR_BTN);

        let tooltip = CreateWindowExW(
            WS_EX_TOPMOST,
            w("tooltips_class32").as_ptr(),
            null(),
            WS_POPUP | TTS_NOPREFIX | TTS_ALWAYSTIP,
            0,
            0,
            0,
            0,
            hwnd,
            0 as HMENU,
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
            min_btn,
            top_btn,
            info_btn,
            tooltip,
            tips: Vec::new(),
            hot: 0,
            topmost: settings.always_on_top,
            font_name,
            font_host,
            font_icon,
            font_icon_sm,
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

        SendMessageW(tooltip, TTM_SETMAXTIPWIDTH, 0, 320);
        tip_for_hwnd(plus_btn, "Add a new RDP connection");
        tip_for_hwnd(min_btn, "Minimize to the system tray\nClick the tray icon to bring it back");
        tip_for_hwnd(top_btn, topmost_tip(settings.always_on_top));
        tip_for_hwnd(info_btn, "About RDP Launcher and user manual");

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

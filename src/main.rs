// GUI 子系统：双击 exe 不会再弹出黑屏 cmd 窗口(真正的前台窗口程序)。
// CLI 命令与 --console 模式通过 AllocConsole / AttachConsole 仍可输出到终端。
#![windows_subsystem = "windows"]
#![allow(dead_code)]

mod config;
mod geo;
mod gui;
mod icon;
mod sun;
mod theme;

use anyhow::anyhow;
use chrono::{Local, NaiveDate, NaiveTime};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use windows_sys::Win32::Foundation::{HWND, LRESULT, LPARAM, POINT, RECT, SIZE, WPARAM};
use windows_sys::core::PCSTR;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    ShellExecuteW, Shell_NotifyIconW,
};
use windows_sys::Win32::Graphics::Gdi::{
    CreateSolidBrush, DEFAULT_GUI_FONT, DeleteObject, DrawTextW, FillRect, GetDC,
    GetStockObject, GetTextExtentPoint32W, HDC, HGDIOBJ, InvalidateRect, ReleaseDC, SetBkColor,
    SetBkMode, SetTextColor, SelectObject,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, CW_USEDEFAULT, DefWindowProcW,
    DestroyMenu, DestroyWindow, EnableWindow, EnumChildWindows, GetClientRect, GetCursorPos, GetMessageW,
    GetWindowTextLengthW, GetWindowTextW, HMENU, IDC_ARROW, InsertMenuItemW, LoadCursorW,
    MENUITEMINFOW, MFS_CHECKED, MFS_DISABLED, MFS_ENABLED, MFT_OWNERDRAW, MFT_SEPARATOR,
    MIIM_DATA, MIIM_FTYPE, MIIM_ID, MIIM_STATE, MF_SEPARATOR, MF_STRING, MessageBoxW, MSG,
    PostMessageW, PostQuitMessage, RegisterClassW,
    SetForegroundWindow, SetTimer, ShowWindow, SW_HIDE, SW_SHOWNORMAL, TrackPopupMenu,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TranslateMessage, DispatchMessageW, WM_CLOSE, WM_COMMAND,
    WM_CREATE, WM_DESTROY, WM_APP, WM_LBUTTONUP, WM_RBUTTONUP, WM_QUIT, WM_SETFONT, WM_TIMER, WNDCLASSW, HWND_MESSAGE,
};
use windows_sys::Win32::UI::Controls::{
    DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_DISABLED,
    ODS_FOCUS, ODS_HOTLIGHT, ODS_NOFOCUSRECT, ODS_SELECTED, ODT_BUTTON, ODT_MENU,
};

use crate::config::Config;
use crate::sun::SunTimes;
use crate::theme::Theme;

const ID_LIGHT: u32 = 1001;
const ID_DARK: u32 = 1002;
const ID_TOGGLE: u32 = 1003;
const ID_MODE_SUN: u32 = 1004;
const ID_MODE_SCHED: u32 = 1005;
const ID_OFF: u32 = 1006;
const ID_CHECK: u32 = 1007;
const ID_CONFIG: u32 = 1008;
const ID_EXIT: u32 = 1009;

const TRAY_ICON_ID: u32 = 1;
// windows-sys 0.52 没有 UINT 类型，消息类型直接用 u32(WNDPROC 签名即 u32)
const WM_TRAY: u32 = WM_APP + 1;
/// 后台定位完成后通知主线程立即刷新
const MSG_REFRESH: u32 = WM_APP + 2;

static APP_STATE: OnceLock<Arc<Mutex<AppState>>> = OnceLock::new();

struct AppState {
    cfg: Config,
    sun_cache_date: Option<NaiveDate>,
    sun_cache: Option<SunTimes>,
    coords: Option<(f64, f64)>,
    /// coords 来自哪里：用户手动配置 / 上次缓存 / 系统位置(用于 UI 显示来源)
    coords_source: CoordsSource,
    /// 是否正在后台获取地理位置(防止重复发起请求)
    fetching_coords: bool,
    /// 系统时区 UTC offset(小时；负值 = 西半球；由 fetch_system_timezone 填充)
    tz_offset_hours: Option<f64>,
    /// 最近一次系统位置 API 返回的细分原因("Disabled" / "NotAvailable" 等)，
    /// 由 ensure_coords 后台线程在 Err 时填充，用于 UI 给出针对性提示。
    location_status: Option<String>,
    /// 托盘窗口句柄，用于后台线程发消息通知刷新
    tray_hwnd: Option<HWND>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CoordsSource {
    None,
    CachedFile,
    Config,
    System,
}

fn main() -> anyhow::Result<()> {
    // 声明 PerMonitorV2 DPI 感知：高分屏(如 200%)下保持清晰，避免系统位图拉伸导致模糊。
    // 必须在创建任何窗口之前调用。
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        enable_dark_mode();
        flush_menu_themes();
    }

    let args: Vec<String> = std::env::args().collect();

    // 单实例：互斥体已存在(错误码 183 = ERROR_ALREADY_EXISTS)即说明已有实例在运行。
    // 注意：CreateMutexW 在已存在时会返回一个有效句柄并置 last_error=183，不能用"返回 0"判断。
    unsafe {
        let mutex_name = widestring("Local\\WinThemeAuto-SingleInstance");
        let held = CreateMutexW(ptr::null(), 1, mutex_name.as_ptr());
        if std::io::Error::last_os_error().raw_os_error() == Some(183) {
            // 已有实例在运行：弹窗提示后退出
            let msg = widestring(
                "WinTheme Auto 已在运行，请勿重复打开。\n如需重启，请先从系统托盘退出旧实例。",
            );
            let cap = widestring("WinTheme Auto");
            const MB_ICONINFORMATION: u32 = 0x40;
            const MB_OK: u32 = 0x0;
            MessageBoxW(0, msg.as_ptr(), cap.as_ptr(), MB_OK | MB_ICONINFORMATION);
            std::process::exit(0);
        }
        // 保持 held 句柄在进程生命周期内不释放，作为单实例锁
        std::mem::forget(held);
    }

    // 仅在自启动场景下由注册表 Run 项带入 --silent，意图是"不要弹主窗口、只在托盘里跑"。
    // 用户双击 exe 时不会带这个 flag，所以默认总是显示主窗口。
    let silent = args.iter().any(|a| a == "--silent");

    let cfg = config::load()?;
    if cfg.auto_start {
        // 写注册表时带上 --silent 标志，登录后程序在托盘里静默常驻，不弹主窗口。
        // (用户想"开机就看到主窗口"的话，可以在主窗口的"开机自启"下方关掉 start_minimized。)
        let _ = config::install_startup(&cfg_exe()?, cfg.start_minimized);
    }

    // 启动时同步读取一次系统时区(毫秒级，无 IO)，用于 sun 计算
    let tz_offset_hours = fetch_system_timezone().ok();

    let state = AppState {
        cfg,
        sun_cache_date: None,
        sun_cache: None,
        coords: None,
        coords_source: CoordsSource::None,
        fetching_coords: false,
        tz_offset_hours,
        location_status: None,
        tray_hwnd: None,
    };
    APP_STATE
        .set(Arc::new(Mutex::new(state)))
        .map_err(|_| anyhow!("APP_STATE 已初始化"))?;

    if silent {
        // 静默启动：托盘常驻，不弹主窗口。
        run_tray_only()?;
    } else {
        // 默认：标准 GUI 主窗口 + 系统托盘
        run_gui()?;
    }
    Ok(())
}

/// 获取当前 exe 路径(用于写注册表 Run 项)。
fn cfg_exe() -> anyhow::Result<String> {
    Ok(std::env::current_exe()?.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------
// 主循环：无托盘后台模式
// ---------------------------------------------------------------------------

fn run_headless() -> anyhow::Result<()> {
    loop {
        if let Some(s) = APP_STATE.get() {
            if let Ok(mut st) = s.lock() {
                if let Err(e) = tick(&mut st) {
                    log(&format!("tick 错误: {e}"));
                }
            }
        }
        let secs = APP_STATE
            .get()
            .map(|s| s.lock().unwrap().cfg.check_interval_secs)
            .unwrap_or(60);
        std::thread::sleep(Duration::from_secs(secs.max(1)));
    }
}

// ---------------------------------------------------------------------------
// 主循环：标准 GUI 模式(默认)-- 打开就有主窗口 + 系统托盘
// ---------------------------------------------------------------------------

fn run_gui() -> anyhow::Result<()> {
    IS_MAIN_WINDOW.store(true, Ordering::Relaxed);
    run_event_loop()
}

// ---------------------------------------------------------------------------
// 主循环：仅托盘模式(旧行为)-- HWND_MESSAGE 隐藏窗口 + 系统托盘
// ---------------------------------------------------------------------------

fn run_tray_only() -> anyhow::Result<()> {
    IS_MAIN_WINDOW.store(false, Ordering::Relaxed);
    run_event_loop()
}

fn run_event_loop() -> anyhow::Result<()> {
    unsafe {
        let hinst = GetModuleHandleW(ptr::null());
        let class_name = widestring("WinthemeAutoClass");
        let title = widestring("WinTheme Auto");
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wnd_proc);
        wc.hInstance = hinst;
        wc.lpszClassName = class_name.as_ptr();
        wc.hCursor = LoadCursorW(0, IDC_ARROW);
        wc.hbrBackground = win_brush() as _;
        if RegisterClassW(&wc) == 0 {
            log("RegisterClassW 失败");
        }
        let is_main = IS_MAIN_WINDOW.load(Ordering::Relaxed);
        // 主窗口：OVERLAPPEDWINDOW 风格(标题栏/系统菜单/最小化按钮)+ 可见
        // 仅托盘：HWND_MESSAGE 隐藏窗口
        let (style, ex_style, parent, w, h) = if is_main {
            let style_bits: u32 = 0x00C00000 // WS_OVERLAPPED | WS_CAPTION
                | 0x00080000  // WS_SYSMENU
                | 0x00020000  // WS_MINIMIZEBOX
                | 0x02000000  // WS_CLIPCHILDREN
                | 0x04000000  // WS_CLIPSIBLINGS
                | 0x10000000; // WS_VISIBLE
            let k = gui::dpi_scale_for_system();
            let win_w = (gui::BASE_W as f64 * k).round() as i32;
            let win_h = (gui::BASE_H as f64 * k).round() as i32;
            (
                style_bits,
                0x00040000, // WS_EX_APPWINDOW
                0,          // 无父窗口(HWND 是 isize，空句柄传 0)
                win_w,
                win_h,
            )
        } else {
            (
                0,
                0,
                HWND_MESSAGE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
            )
        };
        let hwnd = CreateWindowExW(
            ex_style,
            class_name.as_ptr(),
            title.as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            w,
            h,
            parent,
            0,
            hinst,
            ptr::null(),
        );
        if hwnd == 0 {
            log("创建窗口失败，回退到无托盘模式");
            return run_headless();
        }
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        drop(class_name);
        drop(title);
    }
    Ok(())
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    _wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let mut nid = nid_base(hwnd);
            Shell_NotifyIconW(NIM_ADD, &mut nid);
            if let Some(s) = APP_STATE.get() {
                if let Ok(mut st) = s.lock() {
                    st.tray_hwnd = Some(hwnd);
                }
            }
            let secs = APP_STATE
                .get()
                .map(|s| s.lock().unwrap().cfg.check_interval_secs)
                .unwrap_or(60);
            SetTimer(hwnd, 1, ((secs * 1000).max(1000)) as u32, None);
            // 主窗口模式：在已创建的主窗口上填充 GUI 控件
            if IS_MAIN_WINDOW.load(Ordering::Relaxed) {
                let hinst = GetModuleHandleW(ptr::null());
                allow_dark_window(hwnd);
                gui::populate_main_window(hwnd, hinst);
                // 设置标题栏 / 任务栏图标(用我们自定义的 .ico，而非默认图标)
                apply_app_icon(hwnd);
            }
            if let Some(s) = APP_STATE.get() {
                if let Ok(mut st) = s.lock() {
                    if let Err(e) = tick(&mut st) {
                        log(&format!("tick 错误: {e}"));
                    }
                }
            }
            update_tooltip(hwnd);
            refresh_ui(hwnd);
            0
        }
        // 静态文本：与应用主题一致(去掉灰底矩形，文字同主题)
        0x0138 => { // WM_CTLCOLORSTATIC
            let hdc = _wparam as isize;
            SetBkMode(hdc, 2); // OPAQUE
            SetBkColor(hdc, bg_color());
            SetTextColor(hdc, text_color());
            win_brush()
        }
        // 输入框：与应用主题一致
        0x0133 => { // WM_CTLCOLOREDIT
            let hdc = _wparam as isize;
            SetBkMode(hdc, 2); // OPAQUE
            SetBkColor(hdc, edit_color());
            SetTextColor(hdc, edit_text_color());
            edit_brush()
        }
        // 窗口背景擦除：与应用主题一致的背景
        0x0014 => { // WM_ERASEBKGND
            let hdc = _wparam as isize;
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);
            FillRect(hdc, &rc, win_brush());
            1
        }
        // 自绘按钮：深色背景 + 白字 + 状态反馈(hover/press/focus/disable)
        0x002B => { // WM_DRAWITEM
            let dis = &*(lparam as *const DRAWITEMSTRUCT);
            if dis.CtlType == ODT_MENU {
                draw_menu_item(lparam)
            } else {
                draw_owner_button(lparam)
            }
        }
        // 自绘菜单：每条菜单项的尺寸(ODT_MENU)
        0x002C => { // WM_MEASUREITEM
            measure_menu_item(lparam)
        }
        // 后台定位完成后触发：立即重新评估并刷新托盘提示
        MSG_REFRESH => {
            if let Some(s) = APP_STATE.get() {
                if let Ok(mut st) = s.lock() {
                    if let Err(e) = tick(&mut st) {
                        log(&format!("tick 错误: {e}"));
                    }
                }
            }
            update_tooltip(hwnd);
            refresh_ui(hwnd);
            0
        }
        WM_TIMER => {
            if let Some(s) = APP_STATE.get() {
                if let Ok(mut st) = s.lock() {
                    if let Err(e) = tick(&mut st) {
                        log(&format!("tick 错误: {e}"));
                    }
                }
            }
            refresh_ui(hwnd);
            0
        }
        WM_TRAY => {
            let event = lparam as u32;
            match event {
                WM_LBUTTONUP => {
                    if IS_MAIN_WINDOW.load(Ordering::Relaxed) {
                        // 主窗口模式：左键 = 显示/隐藏主窗口
                        show_or_toggle_main_window(hwnd);
                    } else {
                        handle_menu_cmd(hwnd, ID_TOGGLE);
                    }
                }
                WM_RBUTTONUP => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_COMMAND => {
            // 主窗口按钮命令分发
            let id = (_wparam as u32) & 0xFFFF;
            match id {
                gui::ID_BTN_TOGGLE => {
                    handle_menu_cmd(hwnd, ID_TOGGLE);
                    refresh_ui(hwnd);
                }
                gui::ID_BTN_CHECK => {
                    // 立即刷新：丢弃内存 + 磁盘上的位置缓存，重新读系统时区，
                    // 并强制重新获取位置(同时也作为一次 manual tick 触发器)。
                    refresh_all(hwnd);
                    refresh_ui(hwnd);
                }
                gui::ID_BTN_ABOUT => {
                    // 关于对话框：作者 / 版本 / 简短说明。
                    show_about_dialog(hwnd);
                }
                gui::ID_BTN_OPEN_LOCATION_SETTINGS => {
                    // 直接跳到 Windows 设置的位置页(ms-settings:privacy-location)
                    // 这要求 Win10 1709+；旧版 fallback 到 ms-settings 顶层
                    open_windows_location_settings();
                }
                gui::ID_BTN_OPEN_CFG => handle_menu_cmd(hwnd, ID_CONFIG),
                gui::ID_BTN_HIDE => {
                    ShowWindow(hwnd, SW_HIDE);
                    log("主窗口已隐藏到托盘");
                }
                gui::ID_BTN_EXIT => {
                    PostQuitMessage(0);
                }
                gui::ID_BTN_CYCLE_MODE => {
                    // 循环切换模式：sun -> schedule -> off -> sun
                    if let Some(s) = APP_STATE.get() {
                        let mut st = s.lock().unwrap();
                        st.cfg.mode = match st.cfg.mode.as_str() {
                            "sun" => "schedule".into(),
                            "schedule" => "off".into(),
                            _ => "sun".into(),
                        };
                        let new_mode = st.cfg.mode.clone();
                        let _ = config::save(&st.cfg);
                        log(&format!("模式 -> {}", new_mode));
                        drop(st);
                        refresh_ui(hwnd);
                    }
                }
                gui::ID_BTN_SAVE_TIME => {
                    save_schedule_time_from_edits(hwnd);
                    refresh_ui(hwnd);
                }
                gui::ID_CHK_AUTOSTART => {
                    if ((_wparam >> 16) as u32) == 0 {
                        // BN_CLICKED：切换开机自启
                        on_autostart_clicked(hwnd);
                        refresh_ui(hwnd);
                    }
                }
                gui::ID_CHK_START_MINIMIZED => {
                    if ((_wparam >> 16) as u32) == 0 {
                        // BN_CLICKED：切换"开机时只在托盘后台运行"
                        on_start_minimized_clicked(hwnd);
                        refresh_ui(hwnd);
                    }
                }
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            // 主窗口：关闭按钮 = 隐藏到托盘(不退出)
            ShowWindow(hwnd, SW_HIDE);
            log("主窗口已隐藏到托盘(关闭窗口 = 隐藏，并未退出)");
            0
        }
        WM_DESTROY => {
            // 清理：托盘 +字体
            let mut nid = nid_base(hwnd);
            Shell_NotifyIconW(NIM_DELETE, &mut nid);
            if IS_MAIN_WINDOW.load(Ordering::Relaxed) {
                use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW;
                let font = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as isize;
                if font != 0 {
                    DeleteObject(font);
                }
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, _wparam, lparam),
    }
}

// ---- 主窗口辅助 ----

static IS_MAIN_WINDOW: AtomicBool = AtomicBool::new(false);
const GWLP_USERDATA: i32 = -21;

// ---- 界面配色：由应用自己的主题判断(is_dark)决定，深浅色各一套，保证可读且一致 ----
const BG_LIGHT: u32 = 0x00F2F2F2; // 浅色窗口背景
const TEXT_LIGHT: u32 = 0x001E1E1E; // 浅色正文
const EDIT_LIGHT: u32 = 0x00FFFFFF; // 浅色输入框
const BG_DARK: u32 = 0x001F1F1F; // 深色窗口背景
const TEXT_DARK: u32 = 0x00E8E8E8; // 深色正文
const EDIT_DARK: u32 = 0x002D2D2D; // 深色输入框(略亮于背景，便于辨认)

// 自绘按钮颜色(深色主题下"凸"于 #1F1F1F 背景；浅色主题下深度更深的灰，比 #F2F2F2 深)
const BTN_NORMAL_LIGHT: u32 = 0x00E1E1E1; // 浅色按钮常态
const BTN_HOVER_LIGHT: u32 = 0x00D4D4D4; // 浅色按钮悬停
const BTN_PRESSED_LIGHT: u32 = 0x00C0C0C0; // 浅色按钮按下
const BTN_NORMAL_DARK: u32 = 0x002D2D2D; // 深色按钮常态
const BTN_HOVER_DARK: u32 = 0x003F3F3F; // 深色按钮悬停
const BTN_PRESSED_DARK: u32 = 0x001A1A1A; // 深色按钮按下
const BTN_TEXT_DISABLED: u32 = 0x00808080; // 禁用文字
const BTN_BORDER: u32 = 0x00808080; // 焦点边框

/// 当前是否为深色主题(跟应用切换的主题一致)。
fn is_dark() -> bool {
    theme::get_theme().map(|t| t == Theme::Dark).unwrap_or(false)
}

fn bg_color() -> u32 {
    if is_dark() { BG_DARK } else { BG_LIGHT }
}
fn text_color() -> u32 {
    if is_dark() { TEXT_DARK } else { TEXT_LIGHT }
}
fn edit_color() -> u32 {
    if is_dark() { EDIT_DARK } else { EDIT_LIGHT }
}
fn edit_text_color() -> u32 {
    if is_dark() { TEXT_DARK } else { TEXT_LIGHT }
}

/// 按主题缓存的窗口画刷(主题切换时由 invalidate_theme_brushes 释放并重建)。
fn win_brush() -> isize {
    ensure_brush(&BG_BRUSH, bg_color())
}

/// 按主题缓存的输入框画刷。
fn edit_brush() -> isize {
    ensure_brush(&EDIT_BRUSH, edit_color())
}

// 用 Mutex<Option<isize>> 替代 OnceLock，让主题切换时能清空缓存并 DeleteObject 旧句柄。
static BG_BRUSH: std::sync::Mutex<Option<isize>> = std::sync::Mutex::new(None);
static EDIT_BRUSH: std::sync::Mutex<Option<isize>> = std::sync::Mutex::new(None);

fn ensure_brush(slot: &'static std::sync::Mutex<Option<isize>>, color: u32) -> isize {
    let mut g = slot.lock().unwrap();
    if let Some(h) = *g {
        if h != 0 {
            return h;
        }
    }
    let h = unsafe { CreateSolidBrush(color) };
    *g = Some(h);
    h
}

/// 主题切换时调用：删除旧 brush 让下一次 WM_CTLCOLOR* 重建，避免深浅色穿越。
unsafe fn invalidate_theme_brushes() {
    if let Ok(mut g) = BG_BRUSH.lock() {
        if let Some(h) = *g {
            if h != 0 {
                DeleteObject(h);
            }
        }
        *g = None;
    }
    if let Ok(mut g) = EDIT_BRUSH.lock() {
        if let Some(h) = *g {
            if h != 0 {
                DeleteObject(h);
            }
        }
        *g = None;
    }
}

/// 自绘按钮绘制(处理 WM_DRAWITEM)。
/// 形参是 WM_DRAWITEM 的 lParam，返回 nonzero = 已处理该消息。
unsafe fn draw_owner_button(lparam: LPARAM) -> LRESULT {
    if lparam == 0 {
        return 0;
    }
    let dis: &DRAWITEMSTRUCT = &*(lparam as *const DRAWITEMSTRUCT);
    // 我们只为按钮绘制；菜单/组合框等保持默认
    if dis.CtlType != ODT_BUTTON {
        return 0;
    }
    let hdc = dis.hDC;
    let mut rc = dis.rcItem;
    let state = dis.itemState;

    // 选颜色(按当前主题 + 状态)
    let (bg, _) = if (state & ODS_DISABLED) != 0 {
        (
            if is_dark() { BTN_NORMAL_DARK } else { BTN_NORMAL_LIGHT },
            true,
        )
    } else if (state & ODS_SELECTED) != 0 {
        (
            if is_dark() { BTN_PRESSED_DARK } else { BTN_PRESSED_LIGHT },
            false,
        )
    } else if (state & ODS_HOTLIGHT) != 0 {
        (
            if is_dark() { BTN_HOVER_DARK } else { BTN_HOVER_LIGHT },
            false,
        )
    } else {
        (
            if is_dark() { BTN_NORMAL_DARK } else { BTN_NORMAL_LIGHT },
            false,
        )
    };
    let brush = CreateSolidBrush(bg);
    FillRect(hdc, &rc, brush);
    // 删除临时刷子；FillRect 用完即可
    DeleteObject(brush);

    // 画按钮文字(居中)
    let len = GetWindowTextLengthW(dis.hwndItem);
    if len > 0 {
        let mut buf: Vec<u16> = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(dis.hwndItem, buf.as_mut_ptr(), buf.len() as i32);
        SetBkMode(hdc, 1); // TRANSPARENT
        let text_color = if (state & ODS_DISABLED) != 0 {
            BTN_TEXT_DISABLED
        } else {
            text_color()
        };
        SetTextColor(hdc, text_color);
        // DT_CENTER | DT_VCENTER | DT_SINGLELINE = 0x0025
        DrawTextW(hdc, buf.as_ptr(), n, &mut rc as *mut RECT, 0x0025);
    }

    // 焦点边框(无障碍要求)
    if (state & ODS_FOCUS) != 0 && (state & ODS_NOFOCUSRECT) == 0 {
        use windows_sys::Win32::Graphics::Gdi::DrawFocusRect;
        SetTextColor(hdc, BTN_BORDER);
        DrawFocusRect(hdc, &rc);
    }

    1
}

/// 给主窗口设置自定义标题栏 / 任务栏图标(大小随 DPI 缩放，避免高分屏下模糊)。
unsafe fn apply_app_icon(hwnd: HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;
    const WM_SETICON: u32 = 0x0080;
    const ICON_SMALL: usize = 0;
    const ICON_BIG: usize = 1;
    let k = gui::dpi_scale_for_system();
    let small = icon::load_icon((16.0 * k).round() as i32);
    let big = icon::load_icon((32.0 * k).round() as i32);
    // wParam = 图标类型，lParam = HICON
    SendMessageW(hwnd, WM_SETICON, ICON_SMALL, small);
    SendMessageW(hwnd, WM_SETICON, ICON_BIG, big);
}

/// 让标题栏跟随当前主题(深色标题栏 / 浅色标题栏)。
unsafe fn apply_titlebar_theme(hwnd: HWND) {
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    let dark: i32 = if is_dark() { 1 } else { 0 };
    DwmSetWindowAttribute(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        &dark as *const i32 as *const std::ffi::c_void,
        std::mem::size_of::<i32>() as u32,
    );
}

// ---- 让系统通用控件跟随深色模式(未公开 API，失败则静默保持现状)----

/// 进程启动时调用一次：告诉系统该应用“允许深色模式”(通用控件/菜单才会跟随主题)。
unsafe fn enable_dark_mode() {
    type SetPreferredAppMode = unsafe extern "system" fn(u32) -> u32;
    let dll = LoadLibraryW(widestring("uxtheme.dll").as_ptr());
    if dll == 0 {
        return;
    }
    let proc = GetProcAddress(dll, b"SetPreferredAppMode\0".as_ptr() as PCSTR);
    if let Some(base) = proc {
        let f: SetPreferredAppMode = std::mem::transmute(base);
        f(1); // PreferredAppMode::AllowDark
    }
}

/// 让指定窗口的深色模式生效(AllowDarkModeForWindow)。
unsafe fn allow_dark_window(hwnd: HWND) {
    type AllowDark = unsafe extern "system" fn(isize, u32) -> u32;
    let dll = LoadLibraryW(widestring("uxtheme.dll").as_ptr());
    if dll == 0 {
        return;
    }
    let proc = GetProcAddress(dll, b"AllowDarkModeForWindow\0".as_ptr() as PCSTR);
    if let Some(base) = proc {
        let f: AllowDark = std::mem::transmute(base);
        f(hwnd, 1);
    }
}

/// 强制让系统丢弃本进程已缓存的菜单主题，强制下次 menu 弹出时重新读取深色偏好。
///
/// uxtheme 的非公开 API。对我们 app 自己创建的菜单(如果有)有效，能让 popup menu
/// 立即切到深色样式。但**系统托盘(Explorer 进程)的右键菜单**不在本进程，所以
/// 严格来说这个调用对系统托盘菜单无效--它是 Explorer 自己的 theme setting。
/// 保留调用是因为：未来如果加 app 级 popup menu，能自动跟随主题。
unsafe fn flush_menu_themes() {
    type FlushMenuThemes = unsafe extern "system" fn() -> i32;
    let dll = LoadLibraryW(widestring("uxtheme.dll").as_ptr());
    if dll == 0 {
        return;
    }
    let proc = GetProcAddress(dll, b"FlushMenuThemes\0".as_ptr() as PCSTR);
    if let Some(base) = proc {
        let f: FlushMenuThemes = std::mem::transmute(base);
        let _ = f();
    }
}

/// 刷新 UI(托盘提示 + 主窗口)
unsafe fn refresh_ui(hwnd: HWND) {
    update_tooltip(hwnd);
    if IS_MAIN_WINDOW.load(Ordering::Relaxed) {
        apply_titlebar_theme(hwnd);
        // 主题可能已切换：让旧的窗口/编辑刷子失效，下一次 WM_CTLCOLOR* 重建为当前主题颜色
        invalidate_theme_brushes();
        gui::refresh_main_window(hwnd);
        // 连同子控件一起重绘，让深浅色即时生效(自绘按钮重发 WM_DRAWITEM)
        invalidate_with_children(hwnd);
    }
}

/// 重绘窗口及其全部子控件(切换深浅色时让文字/输入框颜色跟着变)。
unsafe fn invalidate_with_children(hwnd: HWND) {
    unsafe extern "system" fn invalidate_child(h: HWND, _: isize) -> i32 {
        InvalidateRect(h, std::ptr::null(), 1);
        1
    }
    EnumChildWindows(hwnd, Some(invalidate_child), 0);
    InvalidateRect(hwnd, std::ptr::null(), 1);
}

/// 左键托盘：显示/隐藏主窗口
unsafe fn show_or_toggle_main_window(hwnd: HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible;
    if IsWindowVisible(hwnd) != 0 {
        ShowWindow(hwnd, SW_HIDE);
    } else {
        ShowWindow(hwnd, SW_SHOWNORMAL);
        SetForegroundWindow(hwnd);
        refresh_ui(hwnd);
    }
}

/// 把主窗口编辑框里的 light_time/dark_time 写回配置
unsafe fn save_schedule_time_from_edits(hwnd: HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetDlgItem;
    let light_h = GetDlgItem(hwnd, gui::ID_EDIT_LIGHT as i32);
    let dark_h = GetDlgItem(hwnd, gui::ID_EDIT_DARK as i32);
    if light_h == 0 || dark_h == 0 {
        return;
    }
    let mut buf1 = [0u16; 16];
    let mut buf2 = [0u16; 16];
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowTextW;
    let n1 = GetWindowTextW(light_h, buf1.as_mut_ptr(), 16);
    let n2 = GetWindowTextW(dark_h, buf2.as_mut_ptr(), 16);
    if n1 == 0 || n2 == 0 {
        log("时间保存失败：读取编辑框内容为空");
        return;
    }
    let s1 = String::from_utf16_lossy(&buf1[..n1 as usize]);
    let s2 = String::from_utf16_lossy(&buf2[..n2 as usize]);
    if let Some(s) = APP_STATE.get() {
        let mut st = s.lock().unwrap();
        st.cfg.light_time = s1.trim().to_string();
        st.cfg.dark_time = s2.trim().to_string();
        let _ = config::save(&st.cfg);
        log(&format!(
            "时间已保存：浅色 {} 深色 {}",
            st.cfg.light_time, st.cfg.dark_time
        ));
    }
}

/// 开机自启复选框被点击：按当前勾选状态写入/移除开机启动，并保存配置。
unsafe fn on_autostart_clicked(hwnd: HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetDlgItem, SendMessageW};
    const BM_GETCHECK: u32 = 0x00F0;
    let chk = GetDlgItem(hwnd, gui::ID_CHK_AUTOSTART as i32);
    if chk == 0 {
        return;
    }
    let on = SendMessageW(chk, BM_GETCHECK, 0, 0) == 1;
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(s) = APP_STATE.get() {
        let mut st = s.lock().unwrap();
        st.cfg.auto_start = on;
        let _ = config::save(&st.cfg);
        if on {
            // 写注册表时按 cfg.start_minimized 决定是否带 --silent
            let _ = config::install_startup(&exe, st.cfg.start_minimized);
            log(&format!(
                "开机自启已开启(写注册表 Run 项，start_minimized={})",
                st.cfg.start_minimized
            ));
        } else {
            let _ = config::uninstall_startup();
            log("开机自启已关闭(移除注册表 Run 项)");
        }
    }
}

/// 弹"关于"对话框：自绘窗口(不用 MessageBoxW，因为 MessageBox 在 app dark
/// preference 下文字颜色仍走系统主题，会出现"深色背景下深色字"的不可读情况)。
///
/// 注册一个临时窗口类 "WinthemeAboutClass"，创建模态样式窗口，背景/文字
/// 按当前主题(is_dark())画，底部一个"关闭"按钮 + Enter/Esc 键退出。
unsafe fn show_about_dialog(parent: HWND) {

    let hinst = GetModuleHandleW(ptr::null());
    let class_name = widestring("WinthemeAboutClass");
    // CLASS_ALREADY_EXISTS 不算错
    let _ = register_about_class();
    let title = widestring("关于 WinTheme Auto");
    // 居中显示在 parent 客户区中心
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    let mut parent_rc: RECT = std::mem::zeroed();
    GetClientRect(parent, &mut parent_rc);
    let mut pp: POINT = std::mem::zeroed();
    ClientToScreen(parent, &mut pp);
    let ww: i32 = 420;
    let wh: i32 = 320;
    let x = pp.x + (parent_rc.right - parent_rc.left - ww) / 2;
    let y = pp.y + (parent_rc.bottom - parent_rc.top - wh) / 2;
    let hwnd = CreateWindowExW(
        0,
        class_name.as_ptr(),
        title.as_ptr(),
        0x80C80000 | 0x10000000, // WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_VISIBLE
        x, y, ww, wh,
        parent,
        0, hinst, ptr::null(),
    );
    if hwnd == 0 {
        log("show_about_dialog: CreateWindowExW 失败");
        return;
    }

    // 模态对话框：禁用父窗口，只为 about 窗口跑局部消息循环。
    // 用 GetMessageW(hwnd) 过滤，窗口销毁后自然返回 0 退出循环。
    EnableWindow(parent, 0);
    ShowWindow(hwnd, SW_SHOWNORMAL);
    SetForegroundWindow(hwnd);
    let mut msg: MSG = std::mem::zeroed();
    while GetMessageW(&mut msg, hwnd, 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    EnableWindow(parent, 1);
    SetForegroundWindow(parent);
    let _ = DestroyWindow(hwnd);
}
/// 注册 "WinthemeAboutClass" 窗口类（独立于主窗口，避免 class 复用导致 wnd_proc 误判）。
unsafe fn register_about_class() -> u16 {
    let hinst = GetModuleHandleW(ptr::null());
    let class_name = widestring("WinthemeAboutClass");
    let mut wc: WNDCLASSW = std::mem::zeroed();
    wc.lpfnWndProc = Some(about_wnd_proc);
    wc.hInstance = hinst;
    wc.lpszClassName = class_name.as_ptr();
    wc.hCursor = LoadCursorW(0, IDC_ARROW);
    wc.hbrBackground = win_brush() as _;
    RegisterClassW(&wc)
}


/// "关于"对话框的窗口回调：自绘背景与文字，添加"关闭"按钮，
/// 处理 Enter/Esc 键、关闭按钮、WM_CLOSE。
unsafe extern "system" fn about_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    const ID_OK: u32 = 100;
    match msg {
        0x0001 /* WM_CREATE */ => {
            // "关闭"按钮(自绘，跟主窗口按钮风格一致)
            let hinst = GetModuleHandleW(ptr::null());
            let btn_w = 100;
            let btn_h = 32;
            // 居中放底部
            let r = get_window_rect_content(hwnd);
            let x = (r.right - r.left - btn_w) / 2;
            let y = (r.bottom - r.top) - btn_h - 16;
            let btn = CreateWindowExW(
                0,
                widestring("BUTTON").as_ptr(),
                widestring("关闭").as_ptr(),
                0x40000000 | 0x10000000 | 0x00010000 | 0x0000000B, // WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW
                x, y, btn_w, btn_h,
                hwnd,
                ID_OK as isize,
                hinst,
                ptr::null(),
            );
            // 复用主窗口 UI 字体
            let f = crate::gui::ui_font(crate::gui::dpi_scale_for_window(hwnd));
            if f != 0 {
                use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;
                use windows_sys::Win32::Foundation::WPARAM;
                SendMessageW(btn, WM_SETFONT, f as WPARAM, 1);
            }
            0
        }
        0x0002 /* WM_DESTROY */ => {
            // 关键：不要 PostQuitMessage。about 是非模态的副窗口，
            // 关闭它只关自己，**不能**让主程序退出
            0
        }
        0x0010 /* WM_CLOSE */ => {
            DestroyWindow(hwnd);
            0
        }
        0x002B /* WM_DRAWITEM (按钮) */ => {
            // 跟主窗口按钮同款自绘逻辑
            draw_owner_button(lparam)
        }
        0x0009 /* WM_PAINT */ => {
            paint_about(hwnd);
            0
        }
        0x0111 /* WM_COMMAND */ => {
            let id = (wparam as u32) & 0xFFFF;
            if id == ID_OK {
                // 把主窗口 enable 回来（非模态的副效应补偿）
                use windows_sys::Win32::UI::WindowsAndMessaging::GetParent;
                let parent = GetParent(hwnd);
                if parent != 0 {
                    PostMessageW(parent, 0x000A, 1, 0);  // WM_ENABLE = 1
                }
                DestroyWindow(hwnd);
                return 0;
            }
            0
        }
        0x0100 /* WM_KEYDOWN */ => {
            if wparam as u32 == 0x1B /* VK_ESCAPE */ {
                DestroyWindow(hwnd);
                return 0;
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 拿窗口的"客户区坐标下的整个窗口"rect(用于在窗口内放按钮)
unsafe fn get_window_rect_content(hwnd: HWND) -> RECT {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;
    let mut r: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut r);
    r
}

/// "关于"对话框自绘文字
unsafe fn paint_about(hwnd: HWND) {
    use windows_sys::Win32::Graphics::Gdi::{
        BeginPaint, EndPaint, DrawTextW, FillRect, SetBkMode, SetTextColor, PAINTSTRUCT,
    };
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    if hdc == 0 {
        return;
    }
    let rc = get_window_rect_content(hwnd);
    // 显式按主题填充背景，否则 BEGINPAINT 可能不清屏，导致黑底 + 深色字不可见
    FillRect(hdc, &rc, win_brush());
    let version = env!("CARGO_PKG_VERSION");
    // 文字：先居中标题("WinTheme Auto")，再列各字段
    let title_w = widestring(&format!("WinTheme Auto v{version}"));
    let mut title_rc = rc;
    title_rc.left += 16;
    title_rc.right -= 16;
    title_rc.top += 24;
    title_rc.bottom = title_rc.top + 36;
    SetBkMode(hdc, 1); // TRANSPARENT
    SetTextColor(hdc, if is_dark() { 0xFFFFFF } else { 0x1E1E1E });
    // 0x0024 = DT_CENTER(0x0001) | DT_SINGLELINE(0x0020) | DT_VCENTER(0x0004) 是错的
    // 0x0025 = 0x0001 | 0x0004 | 0x0020 = CENTER | VCENTER | SINGLELINE  对
    DrawTextW(hdc, title_w.as_ptr(), title_w.len() as i32, &mut title_rc, 0x0025);

    // body 文本：按行写
    let lines: Vec<String> = vec![
        "Windows 11 浅色/深色主题自动切换器".to_string(),
        "".to_string(),
        "支持：".to_string(),
        "  • 跟随当地日出日落(基于系统位置 API)".to_string(),
        "  • 定时切换(浅色 / 深色 时刻可设)".to_string(),
        "  • 开机可静默在托盘后台运行".to_string(),
        "".to_string(),
        "作者：stephen-cusi".to_string(),
        "仓库：github.com/stephen-cusi/wintheme-auto".to_string(),
        "协议：MIT License".to_string(),
        "".to_string(),
        "原生 Win32 + Rust 编写，零额外运行时依赖。".to_string(),
    ];
    SetTextColor(hdc, if is_dark() { 0xE8E8E8 } else { 0x1E1E1E });
    let mut body_rc = rc;
    body_rc.left += 20;
    body_rc.right -= 20;
    body_rc.top += 70;
    body_rc.bottom = rc.bottom - 60; // 留空间给底部按钮
    let joined = lines.join("\n");
    let body_w = widestring(&joined);
    // 0x0008 = DT_LEFT
    DrawTextW(hdc, body_w.as_ptr(), body_w.len() as i32, &mut body_rc, 0x0008);
    let _ = EndPaint(hwnd, &mut ps);
}

/// 开机静默启动复选框被点击：翻转 cfg.start_minimized 持久化。
/// 如果当前已写注册表自启，会同步更新注册表项的 --silent 标志。
unsafe fn on_start_minimized_clicked(hwnd: HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetDlgItem, SendMessageW};
    const BM_GETCHECK: u32 = 0x00F0;
    let chk = GetDlgItem(hwnd, gui::ID_CHK_START_MINIMIZED as i32);
    if chk == 0 {
        return;
    }
    let on = SendMessageW(chk, BM_GETCHECK, 0, 0) == 1;
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(s) = APP_STATE.get() {
        let mut st = s.lock().unwrap();
        st.cfg.start_minimized = on;
        let _ = config::save(&st.cfg);
        // 如果自启已开，把注册表项的 --silent 同步一下(避免选项改了但下次登录仍按旧值启动)
        if st.cfg.auto_start {
            let _ = config::install_startup(&exe, on);
        }
        log(&format!("开机静默启动 = {}(已持久化)", on));
    }
}

/// 主窗口的"刷新"按钮：丢弃已缓存的位置(内存 + 磁盘)+ 重新读系统时区 +
/// 强制走一次系统位置 API + 立即按新坐标 tick。
///
/// 典型用途：
/// - 改了系统时区(控制面板 → 日期和时间)后想立即按新时区重算日出日落
/// - 关闭系统位置后又开启 → 立即重新触发权限对话框和获取流程
/// - 改完 cfg.latitude/longitude 手动配置后想立即生效(清掉旧 coords)
fn refresh_all(hwnd: HWND) {
    // 1) 重新读系统时区(毫秒级，IO 只读注册表)
    let new_tz = fetch_system_timezone().ok();
    // 2) 清内存坐标 + sun 缓存
    if let Some(s) = APP_STATE.get() {
        if let Ok(mut st) = s.lock() {
            if let Some(tz) = new_tz {
                if st.tz_offset_hours != Some(tz) {
                    log(&format!("系统时区已刷新: UTC{:+.1}", tz));
                    st.tz_offset_hours = Some(tz);
                }
            }
            st.coords = None;
            st.coords_source = CoordsSource::None;
            st.sun_cache = None;
            st.sun_cache_date = None;
        }
    }
    // 3) 删磁盘 cache
    let cache_path = config::config_dir().join("coords.cache");
    if cache_path.exists() {
        if let Err(e) = std::fs::remove_file(&cache_path) {
            log(&format!("删除位置缓存失败({e})- 继续执行"));
        } else {
            log("已清除位置缓存，强制重新获取");
        }
    } else {
        log("磁盘无位置缓存，立即重新获取");
    }
    // 4) 立即 tick(不等待定时器)：根据新坐标重新评估主题
    if let Some(s) = APP_STATE.get() {
        if let Ok(mut st) = s.lock() {
            if let Err(e) = tick(&mut st) {
                log(&format!("tick 错误: {e}"));
            }
        }
    }
    // 5) 立即强制刷新一次 UI
    unsafe { refresh_ui(hwnd) };
    log("已触发时区 + 位置刷新(后台完成会自动再刷一次)");
}

/// 读取 Windows 系统时区 → UTC offset(小时，东半球为正)。
///
/// 用 Win32 `GetTimeZoneInformation` 直接拿 Bias + DaylightBias(**不**依赖 chrono)，
/// 原因：
/// 1. 跨进程不缓存--用户改了系统时区后这个函数会立刻返回新值
/// 2. 显式支持夏令时(chrono 在 Windows 上对 DST 的处理是它自己实现的，
///    跟 Win32 API 不一定一致)
///
/// Bias 是"本地分钟数 minus UTC 分钟数"，所以 UTC offset = -Bias / 60。
/// 若当前处于夏令时，再加 DaylightBias。
pub fn fetch_system_timezone() -> anyhow::Result<f64> {
    unsafe {
        let mut tzi: TIME_ZONE_INFORMATION = std::mem::zeroed();
        let ret = GetTimeZoneInformation(&mut tzi);
        // ret: 0 = STANDARD, 1 = DAYLIGHT, 0xFFFFFFFF = INVALID
        if ret == 0xFFFF_FFFF {
            return Err(anyhow!("GetTimeZoneInformation 返回 INVALID"));
        }
        let bias_min = tzi.Bias as i32;
        let dst_min = if ret == 1 { tzi.DaylightBias as i32 } else { 0 };
        let total_min = bias_min + dst_min;
        Ok(-(total_min as f64) / 60.0)
    }
}

// ---------------------------------------------------------------------------
// 托盘菜单
// ---------------------------------------------------------------------------

/// 单条菜单项的数据。owner-draw 模式下 wnd_proc 需要这些信息来画背景/文字/勾选标记。
struct MenuItem {
    id: u32,
    text: String,
    separator: bool,
    checked: bool,
    enabled: bool,
}

/// 托盘菜单的当前快照(每次 show_menu 时整体替换)。
/// MENUITEMINFOW.dwItemData 存 index 进去(usize)，WM_DRAWITEM 时用 itemData 反查。
static MENU_ITEMS: std::sync::Mutex<Vec<MenuItem>> = std::sync::Mutex::new(Vec::new());

unsafe fn show_menu(hwnd: HWND) {
    let (mode, cur) = {
        let lock = APP_STATE.get().unwrap().lock().unwrap();
        (lock.cfg.mode.clone(), theme::get_theme().ok())
    };
    let check = |cond: bool| if cond { "    ✓" } else { "" };

    let mut items: Vec<MenuItem> = Vec::new();
    items.push(MenuItem { id: ID_LIGHT,    text: format!("切换到浅色{}",   check(cur == Some(Theme::Light))),  separator: false, checked: cur == Some(Theme::Light),  enabled: true });
    items.push(MenuItem { id: ID_DARK,     text: format!("切换到深色{}",   check(cur == Some(Theme::Dark))),   separator: false, checked: cur == Some(Theme::Dark),   enabled: true });
    items.push(MenuItem { id: 0,           text: String::new(), separator: true,  checked: false, enabled: true });
    items.push(MenuItem { id: ID_MODE_SUN, text: format!("模式：跟随日出日落{}", check(mode == "sun")),         separator: false, checked: mode == "sun",        enabled: true });
    items.push(MenuItem { id: ID_MODE_SCHED, text: format!("模式：定时切换{}",     check(mode == "schedule")),     separator: false, checked: mode == "schedule",   enabled: true });
    items.push(MenuItem { id: ID_OFF,      text: format!("模式：暂停(手动){}", check(mode == "off")),          separator: false, checked: mode == "off",        enabled: true });
    items.push(MenuItem { id: 0,           text: String::new(), separator: true,  checked: false, enabled: true });
    items.push(MenuItem { id: ID_CHECK,    text: "刷新位置与时区".to_string(), separator: false, checked: false, enabled: true });
    items.push(MenuItem { id: ID_CONFIG,   text: "打开配置文件".to_string(),  separator: false, checked: false, enabled: true });
    items.push(MenuItem { id: 0,           text: String::new(), separator: true,  checked: false, enabled: true });
    items.push(MenuItem { id: ID_EXIT,     text: "退出".to_string(),         separator: false, checked: false, enabled: true });

    // 替换全局菜单快照(在创建 HMENU 之前，这样 InsertMenuItemW 时不会被其他线程读到旧数据)
    {
        let mut g = MENU_ITEMS.lock().unwrap();
        *g = items;
    }

    let menu = CreatePopupMenu();
    {
        let items_ref = MENU_ITEMS.lock().unwrap();
        for (idx, item) in items_ref.iter().enumerate() {
            let mut mii: MENUITEMINFOW = std::mem::zeroed();
            mii.cbSize = std::mem::size_of::<MENUITEMINFOW>() as u32;
            mii.fMask = MIIM_FTYPE | MIIM_ID | MIIM_DATA | MIIM_STATE;
            // 关键：MFT_SEPARATOR 走系统绘制，无法用我们自定义的高度/线宽。
// 改用 MFT_OWNERDRAW 让我们自己画 separator（draw_menu_item 已处理）。
mii.fType = MFT_OWNERDRAW;
            mii.wID = item.id;
            mii.fState = (if item.checked { MFS_CHECKED } else { 0u32 })
                      | (if item.enabled { MFS_ENABLED } else { MFS_DISABLED });
            mii.dwItemData = idx;
            InsertMenuItemW(menu, idx as u32, 1, &mii);
        }
    }

    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_RETURNCMD,
        pt.x, pt.y, 0, hwnd, std::ptr::null(),
    );
    DestroyMenu(menu);
    // 清理快照，避免潜在的下一次访问读到过期数据
    MENU_ITEMS.lock().unwrap().clear();

    if cmd != 0 {
        handle_menu_cmd(hwnd, cmd as u32);
    }
}

/// 处理 WM_MEASUREITEM：返回每条菜单项的尺寸。
unsafe fn measure_menu_item(lparam: LPARAM) -> LRESULT {
    let mis = &mut *(lparam as *mut MEASUREITEMSTRUCT);
    if mis.CtlType != ODT_MENU {
        return 0;
    }
    let idx = mis.itemData;
    let items = MENU_ITEMS.lock().unwrap();
    if idx >= items.len() {
        return 0;
    }
    if items[idx].separator {
        mis.itemHeight = 14;
        mis.itemWidth = 100;
        return 1;
    }
    mis.itemHeight = 30;
    // 文字宽度：用 default GUI 字体算
    let hdc = GetDC(0);
    if hdc == 0 {
        mis.itemWidth = 200;
        return 1;
    }
    let prev_font = select_font_for_hdc(hdc);
    let text_w: Vec<u16> = items[idx].text.encode_utf16().collect();
    let mut size = SIZE { cx: 0, cy: 0 };
    GetTextExtentPoint32W(hdc, text_w.as_ptr(), text_w.len() as i32, &mut size);
    select_font_for_hdc_revert(hdc, prev_font);
    ReleaseDC(0, hdc);
    // 36 (check 区域) + text + 24 (右侧 padding)
    mis.itemWidth = (size.cx + 60) as u32;
    1
}

/// 切换 HDC 当前字体为系统默认 GUI 字体，返回原字体句柄。owner-draw 菜单算文字宽度用。
unsafe fn select_default_gui_font(hdc: HDC) -> HGDIOBJ {
    SelectObject(hdc, GetStockObject(DEFAULT_GUI_FONT))
}
unsafe fn select_font_for_hdc(hdc: HDC) -> HGDIOBJ { select_default_gui_font(hdc) }
unsafe fn select_font_for_hdc_revert(hdc: HDC, prev: HGDIOBJ) {
    SelectObject(hdc, prev);
}

/// 处理 WM_DRAWITEM：自绘菜单项。
unsafe fn draw_menu_item(lparam: LPARAM) -> LRESULT {
    let dis = &*(lparam as *const DRAWITEMSTRUCT);
    if dis.CtlType != ODT_MENU {
        return 0;
    }
    let idx = dis.itemData;
    let items = MENU_ITEMS.lock().unwrap();
    if idx >= items.len() {
        return 0;
    }
    let item = &items[idx];
    let hdc = dis.hDC;
    let mut rc = dis.rcItem;
    let state = dis.itemState;
    let selected = (state & ODS_SELECTED) != 0;
    let grayed   = (state & ODS_DISABLED) != 0;

    // 配色：深浅色两套；选中态背景略亮；disabled 文字更暗
    let (bg, fg_text) = if is_dark() {
        if selected { (0x333333u32, 0xFFFFFFu32) } else { (0x1F1F1Fu32, 0xE8E8E8u32) }
    } else {
        if selected { (0xCFE3F8u32, 0x000000u32) } else { (0xF2F2F2u32, 0x1E1E1Eu32) }
    };
    let fg = if grayed { if is_dark() { 0x808080 } else { 0xA0A0A0 } } else { fg_text };

    // 背景
    let brush = CreateSolidBrush(bg);
    FillRect(hdc, &rc, brush);
    DeleteObject(brush);

    if item.separator {
        // 画一条 1px 横线(深色模式浅灰，浅色模式中灰)
        let line_color = if is_dark() { 0x4A4A4A } else { 0xC8C8C8 };
        let mid_y = (rc.top + rc.bottom) / 2;
        // 2px 粗线，居中在 separator 中点；左右各留 14 像素边距
        let line_rc = RECT { left: rc.left + 14, top: mid_y - 1, right: rc.right - 14, bottom: mid_y + 1 };
        let line_brush = CreateSolidBrush(line_color);
        FillRect(hdc, &line_rc, line_brush);
        DeleteObject(line_brush);
        return 1;
    }

    // 左侧 check 区域
    if item.checked {
        // 画一个 ✓(用 Segoe UI Symbol 字符 ✓，U+2713)
        let check_w: Vec<u16> = "\u{2713}".encode_utf16().collect();
        let mut crc = rc;
        crc.left += 6;
        crc.right = crc.left + 22;
        SetBkMode(hdc, 1); // TRANSPARENT
        SetTextColor(hdc, fg);
        DrawTextW(hdc, check_w.as_ptr(), check_w.len() as i32, &mut crc, 0x0025);
        // 0x0025 = DT_CENTER(0x0001) | DT_VCENTER(0x0004) | DT_SINGLELINE(0x0020)
    }

    // 文字
    let text_w: Vec<u16> = item.text.encode_utf16().collect();
    let mut trc = rc;
    trc.left += 36; // 给 check 区域让位
    trc.right -= 16;
    SetBkMode(hdc, 1);
    SetTextColor(hdc, fg);
    DrawTextW(hdc, text_w.as_ptr(), text_w.len() as i32, &mut trc, 0x0025);
    1
}

unsafe fn handle_menu_cmd(hwnd: HWND, id: u32) {
    let mut s = APP_STATE.get().unwrap().lock().unwrap();
    match id {
        ID_LIGHT => {
            let _ = theme::set_theme(Theme::Light);
            log("手动切换到浅色");
        }
        ID_DARK => {
            let _ = theme::set_theme(Theme::Dark);
            log("手动切换到深色");
        }
        ID_TOGGLE => {
            if let Ok(cur) = theme::get_theme() {
                let _ = theme::set_theme(if cur == Theme::Light {
                    Theme::Dark
                } else {
                    Theme::Light
                });
            }
        }
        ID_MODE_SUN => {
            s.cfg.mode = "sun".into();
            let _ = config::save(&s.cfg);
            log("模式 -> 跟随日出日落");
        }
        ID_MODE_SCHED => {
            s.cfg.mode = "schedule".into();
            let _ = config::save(&s.cfg);
            log("模式 -> 定时切换");
        }
        ID_OFF => {
            s.cfg.mode = "off".into();
            let _ = config::save(&s.cfg);
            log("模式 -> 暂停(手动)");
        }
        ID_CHECK => {
            if let Err(e) = tick(&mut s) {
                log(&format!("tick 错误: {e}"));
            }
        }
        ID_CONFIG => {
            drop(s);
            open_config();
            return;
        }
        ID_EXIT => {
            drop(s);
            DestroyWindow(hwnd);
            return;
        }
        _ => {}
    }
    drop(s);
    update_tooltip(hwnd);
}

unsafe fn open_config() {
    let path = config::config_path();
    let path_str = path.to_string_lossy().into_owned();
    let path_w: Vec<u16> = path_str.encode_utf16().chain(std::iter::once(0)).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    ShellExecuteW(
        0, // hwnd 是 isize 句柄，空句柄传 0
        verb.as_ptr(),
        path_w.as_ptr(),
        ptr::null(),
        ptr::null(),
        SW_SHOWNORMAL,
    );
}

/// 用 ShellExecuteW 打开 Windows 设置 → 隐私和安全性 → 位置。
/// 走 ms-settings:privacy-location URI (Win10 1709+)。旧版本系统会自动 fallback。
unsafe fn open_windows_location_settings() {
    let uri = concat!("ms-settings", ":", "privacy-location");
    let uri_w: Vec<u16> = uri.encode_utf16().chain(std::iter::once(0)).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    let result = ShellExecuteW(
        0,
        verb.as_ptr(),
        uri_w.as_ptr(),
        ptr::null(),
        ptr::null(),
        SW_SHOWNORMAL,
    );
    // ShellExecuteW 返回 >32 表示成功，<=32 表示失败
    let code = result as isize;
    if code <= 32 {
        log(&format!("打开系统位置设置失败(ShellExecuteW 返回 {code}) - 请手动到 设置 -> 隐私和安全性 -> 位置 打开"));
    } else {
        log("已尝试打开系统位置设置");
    }
}

// ---------------------------------------------------------------------------
// 托盘图标
// ---------------------------------------------------------------------------

unsafe fn nid_base(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_ICON_ID;
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = tray_hicon();
    nid
}

/// 首次调用时加载并缓存托盘 HICON(16x16)。
/// 注意：进程退出时不主动 DestroyIcon--Windows 会自动清理；显式 DestroyIcon
/// 需要 HICON 是可变，且要保证不与之后 nid_base 的缓存句柄冲突。
fn tray_hicon() -> isize {
    use std::sync::OnceLock;
    static ICON: OnceLock<isize> = OnceLock::new();
    *ICON.get_or_init(|| {
        let k = gui::dpi_scale_for_system();
        let size = (16.0_f64 * k).round() as i32;
        unsafe { icon::load_icon(size.max(16)) }
    })
}

unsafe fn update_tooltip(hwnd: HWND) {
    let (mode, cur) = if let Some(s) = APP_STATE.get() {
        let st = s.lock().unwrap();
        let mode = match st.cfg.mode.as_str() {
            "sun" => "跟随日出日落",
            "schedule" => "定时切换",
            "off" => "已暂停",
            _ => "未知",
        };
        (
            mode.to_string(),
            theme::get_theme()
                .map(|t| if t == Theme::Light { "浅色" } else { "深色" })
                .unwrap_or("?"),
        )
    } else {
        ("?".into(), "?")
    };
    let tip = format!("WinThemeAuto｜模式:{}｜当前:{}", mode, cur);
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_ICON_ID;
    nid.uFlags = NIF_TIP;
    let tip_w: Vec<u16> = tip.encode_utf16().collect();
    let len = tip_w.len().min(127);
    nid.szTip[..len].copy_from_slice(&tip_w[..len]);
    nid.szTip[len] = 0;
    Shell_NotifyIconW(NIM_MODIFY, &mut nid);
}

// ---------------------------------------------------------------------------
// 核心逻辑
// ---------------------------------------------------------------------------

fn tick(state: &mut AppState) -> anyhow::Result<()> {
    let desired = evaluate(state)?;
    let current = theme::get_theme().unwrap_or(desired);
    if current != desired {
        theme::set_theme(desired)?;
        log(&format!("主题已切换为 {:?}", desired));
    }
    Ok(())
}

fn evaluate(state: &mut AppState) -> anyhow::Result<Theme> {
    let mode = state.cfg.mode.clone();
    match mode.as_str() {
        "off" => theme::get_theme().map_err(|e| anyhow!(e.to_string())),
        "schedule" => {
            let now = Local::now().naive_local().time();
            let light = parse_hm(&state.cfg.light_time)?;
            let dark = parse_hm(&state.cfg.dark_time)?;
            Ok(if light <= dark {
                if now >= light && now < dark {
                    Theme::Light
                } else {
                    Theme::Dark
                }
            } else {
                // 跨午夜：例如 19:00 浅色 -> 07:00 深色
                if now >= light || now < dark {
                    Theme::Light
                } else {
                    Theme::Dark
                }
            })
        }
        "sun" => {
            let st = sun_for_today(state)?;
            let now = Local::now().naive_local().time();
            Ok(sun::desired_theme_for_sun(now, &st))
        }
        _ => {
            log(&format!("未知模式 '{}'，按暂停处理", mode));
            theme::get_theme().map_err(|e| anyhow!(e.to_string()))
        }
    }
}

fn sun_for_today(state: &mut AppState) -> anyhow::Result<SunTimes> {
    let today = Local::now().date_naive();
    if state.sun_cache_date == Some(today) {
        if let Some(st) = state.sun_cache {
            return Ok(st);
        }
    }
    let (lat, lon) = ensure_coords(state)?;
    // 优先使用启动时(或"刷新"时)通过 Win32 GetTimeZoneInformation 拿到的
    // 系统时区 offset。拿不到再退回到 chrono 的 Local offset。
    let tz = state.tz_offset_hours.unwrap_or_else(|| {
        Local::now().offset().local_minus_utc() as f64 / 3600.0
    });
    let st = sun::sun_times(today, lat, lon, tz);
    state.sun_cache_date = Some(today);
    state.sun_cache = Some(st);
    Ok(st)
}

fn ensure_coords(state: &mut AppState) -> anyhow::Result<(f64, f64)> {
    if let Some(c) = state.coords {
        return Ok(c);
    }
    if let Some(c) = read_coords_cache()? {
        state.coords = Some(c);
        state.coords_source = CoordsSource::CachedFile;
        return Ok(c);
    }
    if let (Some(lat), Some(lon)) = (state.cfg.latitude, state.cfg.longitude) {
        let c = (lat, lon);
        state.coords = Some(c);
        state.coords_source = CoordsSource::Config;
        let _ = write_coords_cache(c);
        return Ok(c);
    }
    // 没有可用坐标：放到后台线程去获取，绝不阻塞 UI / 主循环。
    if state.fetching_coords {
        return Err(anyhow!("正在后台获取地理位置，本次跳过"));
    }
    state.fetching_coords = true;
    log("正在后台获取地理位置...");
    let app = APP_STATE.get().unwrap().clone();
    std::thread::spawn(move || {
        let result = geo::fetch_location();
        let mut st = app.lock().unwrap();
        match result {
            Ok(c) => {
                st.coords = Some(c);
                st.coords_source = CoordsSource::System;
                let _ = write_coords_cache(c);
                log(&format!("地理位置获取成功: {:.4},{:.4}", c.0, c.1));
                // 通知主线程立即刷新(不等待下一个定时 tick)
                if let Some(hwnd) = st.tray_hwnd {
                    unsafe {
                        PostMessageW(hwnd, MSG_REFRESH, 0, 0);
                    }
                }
            }
            Err(e) => {
                // 解析出"细分原因"用于 UI 针对性提示
                let msg = e.to_string();
                let status = msg
                    .strip_prefix("location_disabled:")
                    .or_else(|| msg.strip_prefix("location_denied:"))
                    .map(|s| s.to_string());
                st.location_status = status.clone();
                if let Some(s) = &status {
                    let kind = if msg.starts_with("location_denied:") { "权限被拒" } else { "LocationStatus 不可用" };
                    log(&format!("地理位置获取失败({kind}={s}) - UI 将按此状态给出提示"));
                } else {
                    log(&format!("地理位置获取失败: {e}(将按当前配置继续，稍后自动重试)"));
                }
            }
        }
        st.fetching_coords = false;
    });
    Err(anyhow!("正在后台获取地理位置..."))
}

fn read_coords_cache() -> anyhow::Result<Option<(f64, f64)>> {
    let p = config::config_dir().join("coords.cache");
    if !p.exists() {
        return Ok(None);
    }
    let s = std::fs::read_to_string(&p)?;
    let mut parts = s.split(',');
    if let (Some(a), Some(b)) = (parts.next(), parts.next()) {
        if let (Ok(lat), Ok(lon)) = (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
            return Ok(Some((lat, lon)));
        }
    }
    Ok(None)
}

fn write_coords_cache(c: (f64, f64)) -> anyhow::Result<()> {
    let p = config::config_dir().join("coords.cache");
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, format!("{},{}", c.0, c.1))?;
    Ok(())
}

fn parse_hm(s: &str) -> anyhow::Result<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").map_err(|e| anyhow!(e.to_string()))
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn widestring(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    if v.last() != Some(&0u16) {
        v.push(0);
    }
    v
}

fn log(msg: &str) {
    let dir = config::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("wintheme-auto.log");
    let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("[{}] {}\n", ts, msg);
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

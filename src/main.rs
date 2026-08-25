// GUI 子系统：双击 exe 不会再弹出黑屏 cmd 窗口（真正的前台窗口程序）。
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
use windows_sys::Win32::Foundation::{HWND, LRESULT, LPARAM, POINT, RECT, WPARAM};
use windows_sys::core::PCSTR;
use windows_sys::Win32::System::Console::{
    AllocConsole, AttachConsole, ATTACH_PARENT_PROCESS,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetModuleHandleW, GetProcAddress, LoadLibraryW,
};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    ShellExecuteW, Shell_NotifyIconW,
};
use windows_sys::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, DrawTextW, FillRect, InvalidateRect, SetBkColor, SetBkMode,
    SetTextColor,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, CW_USEDEFAULT, DefWindowProcW,
    DestroyMenu, DestroyWindow, EnumChildWindows, GetClientRect, GetCursorPos, GetMessageW,
    GetWindowTextLengthW, GetWindowTextW, HMENU, IDC_ARROW, LoadCursorW, MessageBoxW,
    MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    SetForegroundWindow, SetTimer, ShowWindow, SW_HIDE, SW_SHOWNORMAL, TrackPopupMenu,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TranslateMessage, DispatchMessageW, WM_CLOSE, WM_COMMAND,
    WM_CREATE, WM_DESTROY, WM_APP, WM_LBUTTONUP, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, HWND_MESSAGE,
};
use windows_sys::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_HOTLIGHT, ODS_NOFOCUSRECT, ODS_SELECTED, ODT_BUTTON};

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
// windows-sys 0.52 没有 UINT 类型，消息类型直接用 u32（WNDPROC 签名即 u32）
const WM_TRAY: u32 = WM_APP + 1;
/// 后台定位完成后通知主线程立即刷新
const MSG_REFRESH: u32 = WM_APP + 2;

static APP_STATE: OnceLock<Arc<Mutex<AppState>>> = OnceLock::new();
/// --console 模式：不隐藏控制台，log() 同时输出到 stdout
static CONSOLE_MODE: AtomicBool = AtomicBool::new(false);

struct AppState {
    cfg: Config,
    sun_cache_date: Option<NaiveDate>,
    sun_cache: Option<SunTimes>,
    coords: Option<(f64, f64)>,
    /// 是否正在后台获取地理位置（防止重复发起请求）
    fetching_coords: bool,
    /// 托盘窗口句柄，用于后台线程发消息通知刷新
    tray_hwnd: Option<HWND>,
}

fn main() -> anyhow::Result<()> {
    // 声明 PerMonitorV2 DPI 感知：高分屏（如 200%）下保持清晰，避免系统位图拉伸导致模糊。
    // 必须在创建任何窗口之前调用。
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        enable_dark_mode();
    }

    let args: Vec<String> = std::env::args().collect();
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();

    // GUI 子系统下默认不弹出控制台；--console 自建控制台，其他命令尝试挂到父控制台。
    let console_args = args.iter().any(|a| a == "--console");
    if console_args {
        unsafe { AllocConsole() };
        CONSOLE_MODE.store(true, Ordering::Relaxed);
    } else {
        // 使 CLI 命令在终端里的 println! 输出可见（双击无父控制台则失败并忽略）
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }

    // 一次性命令（带 -- 前缀的都视为命令行调用）
    if args.iter().any(|a| a == "--light") {
        theme::set_theme(Theme::Light)?;
        return Ok(());
    }
    if args.iter().any(|a| a == "--dark") {
        theme::set_theme(Theme::Dark)?;
        return Ok(());
    }
    if args.iter().any(|a| a == "--uninstall") {
        config::uninstall_startup()?;
        emit("已移除开机启动注册表项。");
        return Ok(());
    }
    if args.iter().any(|a| a == "--status") {
        let t = theme::get_theme()
            .map(|t| if t == Theme::Light { "浅色(light)" } else { "深色(dark)" })
            .unwrap_or("未知");
        emit(&format!("当前主题: {}", t));
        return Ok(());
    }

    if args.iter().any(|a| a == "--install") {
        config::install_startup(&exe)?;
        emit("已写入开机启动注册表项，程序将在登录时自动运行。");
    }

    // 单实例：互斥体已存在（错误码 183 = ERROR_ALREADY_EXISTS）即说明已有实例在运行。
    // 注意：CreateMutexW 在已存在时会返回一个有效句柄并置 last_error=183，不能用“返回 0”判断。
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

    let cfg = config::load()?;
    if cfg.auto_start && !args.iter().any(|a| a == "--no-autostart") {
        let _ = config::install_startup(&exe);
    }

    let state = AppState {
        cfg,
        sun_cache_date: None,
        sun_cache: None,
        coords: None,
        fetching_coords: false,
        tray_hwnd: None,
    };
    APP_STATE
        .set(Arc::new(Mutex::new(state)))
        .map_err(|_| anyhow!("APP_STATE 已初始化"))?;

    if args.iter().any(|a| a == "--tray-only") {
        // 仅托盘模式（保留旧行为）
        run_tray_only()?;
    } else {
        // 默认：标准 GUI 主窗口 + 系统托盘
        run_gui()?;
    }
    Ok(())
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
// 主循环：标准 GUI 模式（默认）—— 打开就有主窗口 + 系统托盘
// ---------------------------------------------------------------------------

fn run_gui() -> anyhow::Result<()> {
    IS_MAIN_WINDOW.store(true, Ordering::Relaxed);
    run_event_loop()
}

// ---------------------------------------------------------------------------
// 主循环：仅托盘模式（旧行为）—— HWND_MESSAGE 隐藏窗口 + 系统托盘
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
        // 主窗口：OVERLAPPEDWINDOW 风格（标题栏/系统菜单/最小化按钮）+ 可见
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
                0,          // 无父窗口（HWND 是 isize，空句柄传 0）
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
                // 设置标题栏 / 任务栏图标（用我们自定义的 .ico，而非默认图标）
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
        // 静态文本：与应用主题一致（去掉灰底矩形，文字同主题）
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
        // 自绘按钮：深色背景 + 白字 + 状态反馈（hover/press/focus/disable）
        0x002B => { // WM_DRAWITEM
            draw_owner_button(lparam)
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
                    // 立即检查：重新评估主题；刷新信息行让结果可见。
                    // 若坐标未知会触发后台定位，标签会显示“正在后台获取...”。
                    handle_menu_cmd(hwnd, ID_CHECK);
                    refresh_ui(hwnd);
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
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            // 主窗口：关闭按钮 = 隐藏到托盘（不退出）
            ShowWindow(hwnd, SW_HIDE);
            log("主窗口已隐藏到托盘（关闭窗口 = 隐藏，并未退出）");
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

// ---- 界面配色：由应用自己的主题判断（is_dark）决定，深浅色各一套，保证可读且一致 ----
const BG_LIGHT: u32 = 0x00F2F2F2; // 浅色窗口背景
const TEXT_LIGHT: u32 = 0x001E1E1E; // 浅色正文
const EDIT_LIGHT: u32 = 0x00FFFFFF; // 浅色输入框
const BG_DARK: u32 = 0x001F1F1F; // 深色窗口背景
const TEXT_DARK: u32 = 0x00E8E8E8; // 深色正文
const EDIT_DARK: u32 = 0x002D2D2D; // 深色输入框（略亮于背景，便于辨认）

// 自绘按钮颜色（深色主题下"凸"于 #1F1F1F 背景；浅色主题下深度更深的灰，比 #F2F2F2 深）
const BTN_NORMAL_LIGHT: u32 = 0x00E1E1E1; // 浅色按钮常态
const BTN_HOVER_LIGHT: u32 = 0x00D4D4D4; // 浅色按钮悬停
const BTN_PRESSED_LIGHT: u32 = 0x00C0C0C0; // 浅色按钮按下
const BTN_NORMAL_DARK: u32 = 0x002D2D2D; // 深色按钮常态
const BTN_HOVER_DARK: u32 = 0x003F3F3F; // 深色按钮悬停
const BTN_PRESSED_DARK: u32 = 0x001A1A1A; // 深色按钮按下
const BTN_TEXT_DISABLED: u32 = 0x00808080; // 禁用文字
const BTN_BORDER: u32 = 0x00808080; // 焦点边框

/// 当前是否为深色主题（跟应用切换的主题一致）。
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

/// 按主题缓存的窗口画刷（主题切换时由 invalidate_theme_brushes 释放并重建）。
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

/// 自绘按钮绘制（处理 WM_DRAWITEM）。
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

    // 选颜色（按当前主题 + 状态）
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

    // 画按钮文字（居中）
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

    // 焦点边框（无障碍要求）
    if (state & ODS_FOCUS) != 0 && (state & ODS_NOFOCUSRECT) == 0 {
        use windows_sys::Win32::Graphics::Gdi::DrawFocusRect;
        SetTextColor(hdc, BTN_BORDER);
        DrawFocusRect(hdc, &rc);
    }

    1
}

/// 给主窗口设置自定义标题栏 / 任务栏图标（大小随 DPI 缩放，避免高分屏下模糊）。
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

/// 让标题栏跟随当前主题（深色标题栏 / 浅色标题栏）。
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

// ---- 让系统通用控件跟随深色模式（未公开 API，失败则静默保持现状）----

/// 进程启动时调用一次：告诉系统该应用“允许深色模式”（通用控件/菜单才会跟随主题）。
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

/// 让指定窗口的深色模式生效（AllowDarkModeForWindow）。
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

// ---- 控制台输出 ----
// GUI 子系统下 Rust 自带的 println! 不保证能写到控制台，这里统一走一个直接向
// 标准输出句柄写入的通道（结合 AllocConsole / AttachConsole 使用）。

fn open_console_stdout() -> Option<std::fs::File> {
    unsafe {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        if h == 0 || h == -1 {
            return None;
        }
        Some(std::fs::File::from_raw_handle(h as *mut std::ffi::c_void))
    }
}

/// 往控制台写一行；未附着控制台时静默忽略。
fn emit(msg: &str) {    use std::io::Write;
    use std::sync::Mutex;
    static CONSOLE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();
    let m = CONSOLE.get_or_init(|| Mutex::new(None));
    let mut guard = m.lock().unwrap();
    if guard.is_none() {
        *guard = open_console_stdout();
    }
    if let Some(f) = guard.as_mut() {
        let _ = writeln!(f, "{msg}");
    }
}

/// 刷新 UI（托盘提示 + 主窗口）
unsafe fn refresh_ui(hwnd: HWND) {
    update_tooltip(hwnd);
    if IS_MAIN_WINDOW.load(Ordering::Relaxed) {
        apply_titlebar_theme(hwnd);
        // 主题可能已切换：让旧的窗口/编辑刷子失效，下一次 WM_CTLCOLOR* 重建为当前主题颜色
        invalidate_theme_brushes();
        gui::refresh_main_window(hwnd);
        // 连同子控件一起重绘，让深浅色即时生效（自绘按钮重发 WM_DRAWITEM）
        invalidate_with_children(hwnd);
    }
}

/// 重绘窗口及其全部子控件（切换深浅色时让文字/输入框颜色跟着变）。
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
            let _ = config::install_startup(&exe);
            log("开机自启已开启（写注册表 Run 项）");
        } else {
            let _ = config::uninstall_startup();
            log("开机自启已关闭（移除注册表 Run 项）");
        }
    }
}

// ---------------------------------------------------------------------------
// 托盘菜单
// ---------------------------------------------------------------------------

unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    let (mode, cur) = {
        let lock = APP_STATE.get().unwrap().lock().unwrap();
        (
            lock.cfg.mode.clone(),
            theme::get_theme().ok(),
        )
    };
    let checked = " ✓";
    append_str(
        menu,
        ID_LIGHT,
        &format!("切换到浅色{}", if cur == Some(Theme::Light) { checked } else { "" }),
    );
    append_str(
        menu,
        ID_DARK,
        &format!("切换到深色{}", if cur == Some(Theme::Dark) { checked } else { "" }),
    );
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    append_str(
        menu,
        ID_MODE_SUN,
        &format!("模式：跟随日出日落{}", if mode == "sun" { checked } else { "" }),
    );
    append_str(
        menu,
        ID_MODE_SCHED,
        &format!("模式：定时切换{}", if mode == "schedule" { checked } else { "" }),
    );
    append_str(
        menu,
        ID_OFF,
        &format!("模式：暂停(手动){}", if mode == "off" { checked } else { "" }),
    );
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    append_str(menu, ID_CHECK, "立即检查");
    append_str(menu, ID_CONFIG, "打开配置文件");
    append_str(menu, ID_EXIT, "退出");

    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_RETURNCMD,
        pt.x,
        pt.y,
        0,
        hwnd,
        std::ptr::null(),
    );
    DestroyMenu(menu);
    if cmd != 0 {
        handle_menu_cmd(hwnd, cmd as u32);
    }
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

unsafe fn append_str(menu: HMENU, id: u32, text: &str) {
    let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    AppendMenuW(menu, MF_STRING, id as usize, w.as_ptr());
}

unsafe fn open_config() {
    let path = config::config_path();
    let path_str = path.to_string_lossy().into_owned();
    let path_w: Vec<u16> = path_str.encode_utf16().chain(std::iter::once(0)).collect();
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    ShellExecuteW(
        0, // hwnd 是 isize 句柄，空句柄传 0
        verb.as_ptr(),
        path_w.as_ptr(),
        ptr::null(),
        ptr::null(),
        SW_SHOWNORMAL,
    );
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

/// 首次调用时加载并缓存托盘 HICON（16x16）。
/// 注意：进程退出时不主动 DestroyIcon——Windows 会自动清理；显式 DestroyIcon
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
    let tz = Local::now().offset().local_minus_utc() as f64 / 3600.0;
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
        return Ok(c);
    }
    if let (Some(lat), Some(lon)) = (state.cfg.latitude, state.cfg.longitude) {
        let c = (lat, lon);
        state.coords = Some(c);
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
                let _ = write_coords_cache(c);
                log(&format!("地理位置获取成功: {:.4},{:.4}", c.0, c.1));
                // 通知主线程立即刷新（不等待下一个定时 tick）
                if let Some(hwnd) = st.tray_hwnd {
                    unsafe {
                        PostMessageW(hwnd, MSG_REFRESH, 0, 0);
                    }
                }
            }
            Err(e) => {
                log(&format!("地理位置获取失败: {e}（将按当前配置继续，稍后自动重试）"));
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
    if CONSOLE_MODE.load(Ordering::Relaxed) {
        emit(&format!("[wintheme-auto] {msg}"));
    }
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

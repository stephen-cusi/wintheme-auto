// GUI 子系统：双击 exe 不会再弹出黑屏 cmd 窗口(真正的前台窗口程序)。
// CLI 命令与 --console 模式通过 AllocConsole / AttachConsole 仍可输出到终端。
#![windows_subsystem = "windows"]
#![allow(dead_code)]

mod config;
mod geo;
mod gui;
mod icon;
mod nightlight;
mod notify;
mod sun;
mod theme;

use anyhow::anyhow;
use chrono::{Local, NaiveDate, NaiveTime};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
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
    CreateRoundRectRgn, CreateSolidBrush, DeleteObject, DrawTextW, FillRect,
    FillRgn, GetDC, GetTextExtentPoint32W, InvalidateRect, ReleaseDC, ScreenToClient, SetBkColor,
    SetBkMode, SetTextColor, SelectObject, UpdateWindow,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, CW_USEDEFAULT, DefWindowProcW, DestroyMenu, DestroyWindow,
    AppendMenuW, CreatePopupMenu, EnumChildWindows,
    GetClientRect, GetClassNameW, GetCursorPos, GetDlgItem, GetMessageW, GetWindowTextLengthW, GetWindowTextW,
    IDC_ARROW, IsWindowVisible, KillTimer, LoadCursorW, MF_CHECKED, MF_OWNERDRAW,
    MessageBoxW, MSG, MENUINFO, MIM_APPLYTOSUBMENUS, MIM_BACKGROUND,
    PostMessageW, PostQuitMessage, RegisterClassW, SetCursor, SetForegroundWindow,
    SetMenuInfo, SetWindowsHookExW, UnhookWindowsHookEx, CallNextHookEx,
    HCBT_CREATEWND, WH_CBT,
    SetTimer, ShowWindow, SW_HIDE, SW_SHOWNORMAL, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
    TranslateMessage, DispatchMessageW,
    WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_APP, WM_LBUTTONUP, WM_RBUTTONUP, WM_SETFONT,
    WM_TIMER, WNDCLASSW, IDC_HAND,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::Controls::{
    DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_DISABLED,
    ODS_FOCUS, ODS_HOTLIGHT, ODS_NOFOCUSRECT, ODS_SELECTED, ODT_BUTTON, ODT_MENU,
};

use windows_sys::Win32::System::Threading::GetCurrentThreadId;

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
    /// 手动覆盖：用户在 sun/schedule 自动模式下手动切过主题时，记录"自动化当时想要的主题"。
    /// 只要自动化想要的主题未变，就尊重手动选择、不强制切回；到下一个自动切换边界再恢复。
    manual_desired: Option<Theme>,
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
        enable_per_monitor_v2();
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
        // （HANDLE 是 isize/Copy，无需也绝不能 CloseHandle；let _ 仅消除未使用告警）
        let _ = held;
    }

    // 仅在自启动场景下由注册表 Run 项带入 --silent，意图是"不要弹主窗口、只在托盘里跑"。
    // 用户双击 exe 时不会带这个 flag，所以默认总是显示主窗口。
    let silent = args.iter().any(|a| a == "--silent");

    let cfg = config::load()?;
    // 初始化通知器（静态存储，避免锁冲突）
    let _ = NOTIFIER.set(notify::init());
    // 启动时把配置同步到自绘复选框状态
    CHK_AUTOSTART.store(cfg.auto_start, Ordering::Relaxed);
    CHK_START_MINIMIZED.store(cfg.start_minimized, Ordering::Relaxed);
    CHK_NIGHT_LIGHT.store(cfg.night_light, Ordering::Relaxed);
    CHK_NOTIFICATIONS.store(cfg.notifications, Ordering::Relaxed);
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
        manual_desired: None,
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
// 主循环：仅托盘模式-- 隐藏主窗口 + 系统托盘(左键点击托盘可随时打开主窗口)
// ---------------------------------------------------------------------------

fn run_tray_only() -> anyhow::Result<()> {
    IS_MAIN_WINDOW.store(false, Ordering::Relaxed);
    run_event_loop()
}

fn run_event_loop() -> anyhow::Result<()> {
    unsafe {
        let hinst = GetModuleHandleW(ptr::null());
        let class_name = widestring("WinthemeAutoClass");
        let title = widestring(&format!("WinTheme Auto v{}-{}", env!("CARGO_PKG_VERSION"), build_commit()));
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wnd_proc);
        wc.hInstance = hinst;
        wc.lpszClassName = class_name.as_ptr();
        wc.hCursor = LoadCursorW(0, IDC_ARROW);
        wc.hbrBackground = win_brush() as _;
        if RegisterClassW(&wc) == 0 {
            log("RegisterClassW 失败");
        }
        // 始终创建真正的窗口：托盘模式只是不加 WS_VISIBLE（隐藏），
        // 这样左键点击托盘时可以直接 ShowWindow 显示主窗口。
        // （旧实现用 HWND_MESSAGE 导致托盘模式永远无法打开主界面）
        let k = gui::dpi_scale_for_system();
        let win_w = (gui::BASE_W as f64 * k).round() as i32;
        let win_h = (gui::BASE_H as f64 * k).round() as i32;
        let style: u32 = 0x00C00000 // WS_OVERLAPPED | WS_CAPTION
            | 0x00080000  // WS_SYSMENU
            | 0x00020000  // WS_MINIMIZEBOX
            | 0x02000000  // WS_CLIPCHILDREN
            | 0x04000000  // WS_CLIPSIBLINGS
            | if IS_MAIN_WINDOW.load(Ordering::Relaxed) { 0x10000000 } else { 0 }; // WS_VISIBLE 仅 GUI 模式
        let ex_style: u32 = if IS_MAIN_WINDOW.load(Ordering::Relaxed) { 0x00040000 } else { 0 }; // WS_EX_APPWINDOW 仅 GUI 模式
        let (style, ex_style, parent, w, h) = (style, ex_style, 0isize, win_w, win_h);
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
            SetTimer(hwnd, TIMER_MAIN, ((secs * 1000).max(1000)) as u32, None);
            // 始终填充 GUI 控件（托盘模式也创建了真正的窗口，只是隐藏的）
            {
                let hinst = GetModuleHandleW(ptr::null());
                allow_dark_window(hwnd, true);
                gui::populate_main_window(hwnd, hinst);
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
        // 复选框/单选标签：文字跟随主题(否则深色模式下仍是黑字)
        0x0135 => { // WM_CTLCOLORBTN
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
            } else if dis.CtlID == gui::ID_CHK_AUTOSTART
                || dis.CtlID == gui::ID_CHK_START_MINIMIZED
                || dis.CtlID == gui::ID_CHK_NIGHT_LIGHT
                || dis.CtlID == gui::ID_CHK_NOTIFICATIONS
            {
                draw_owner_checkbox(dis)
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
        WM_TIMER if _wparam == TIMER_SUBMENU => {
            // 子复选框展开/收起动画：每帧推进进度并只重绘该控件
            let h = GetDlgItem(hwnd, gui::ID_CHK_START_MINIMIZED as i32);
            if h == 0 {
                KillTimer(hwnd, TIMER_SUBMENU);
                SUB_ANIM_ACTIVE.store(false, Ordering::Relaxed);
                return 0;
            }
            let showing = SUB_ANIM_SHOWING.load(Ordering::Relaxed);
            let mut f = SUB_ANIM_FRAME.load(Ordering::Relaxed);
            if showing && f < SUB_ANIM_FRAMES {
                f += 1;
                SUB_ANIM_FRAME.store(f, Ordering::Relaxed);
            } else if !showing && f > 0 {
                f -= 1;
                SUB_ANIM_FRAME.store(f, Ordering::Relaxed);
            }
            InvalidateRect(h, ptr::null(), 1);
            UpdateWindow(h);
            // && 优先级高于 ||，无需括号
            let done = showing && f == SUB_ANIM_FRAMES || !showing && f == 0;
            if done {
                KillTimer(hwnd, TIMER_SUBMENU);
                SUB_ANIM_ACTIVE.store(false, Ordering::Relaxed);
                if !showing {
                    ShowWindow(h, SW_HIDE);
                }
            }
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
                    // 左键点击托盘：始终显示/隐藏主窗口
                    show_or_toggle_main_window(hwnd);
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
                    // 与托盘退出(ID_EXIT)保持一致：先隐藏让 DWM 重新合成底层桌面，
                    // 再强制 DWM 完成合成，最后销毁窗口，避免退出时的白闪/残影。
                    ShowWindow(hwnd, SW_HIDE);
                    use windows_sys::Win32::Graphics::Dwm::DwmFlush;
                    unsafe { DwmFlush() };
                    DestroyWindow(hwnd);
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
                        // BN_CLICKED：翻转勾选并切换开机自启
                        toggle_checkbox(hwnd, gui::ID_CHK_AUTOSTART);
                        on_autostart_clicked(hwnd);
                        // 只同步"静默"子选项显隐，不整窗刷新(避免图标/文字闪烁)
                        sync_sub_checkbox_visibility(hwnd);
                    }
                }
                gui::ID_CHK_START_MINIMIZED => {
                    if ((_wparam >> 16) as u32) == 0 {
                        // BN_CLICKED：翻转"开机时只在托盘后台运行"
                        toggle_checkbox(hwnd, gui::ID_CHK_START_MINIMIZED);
                        on_start_minimized_clicked(hwnd);
                    }
                }
                gui::ID_CHK_NIGHT_LIGHT => {
                    if ((_wparam >> 16) as u32) == 0 {
                        toggle_checkbox(hwnd, gui::ID_CHK_NIGHT_LIGHT);
                        on_night_light_clicked();
                    }
                }
                gui::ID_CHK_NOTIFICATIONS => {
                    if ((_wparam >> 16) as u32) == 0 {
                        toggle_checkbox(hwnd, gui::ID_CHK_NOTIFICATIONS);
                        on_notifications_clicked();
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
            {
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
static NOTIFIER: OnceLock<Option<notify::ToastsNotifier>> = OnceLock::new();
const GWLP_USERDATA: i32 = -21;

/// 定时检查(主题/日出日落)的计时器 id
const TIMER_MAIN: usize = 1;
/// 子复选框展开/收起动画的计时器 id
const TIMER_SUBMENU: usize = 2;
/// 子复选框动画总帧数(16ms/帧 ≈ 144ms)
const SUB_ANIM_FRAMES: usize = 9;
/// 子复选框动画状态：ACTIVE=动画进行中；SHOWING=方向(true=展开)；FRAME=当前帧(0..=SUB_ANIM_FRAMES)
static SUB_ANIM_ACTIVE: AtomicBool = AtomicBool::new(false);
static SUB_ANIM_SHOWING: AtomicBool = AtomicBool::new(true);
static SUB_ANIM_FRAME: AtomicUsize = AtomicUsize::new(SUB_ANIM_FRAMES);

// 自绘复选框的勾选状态(自己维护，不依赖系统 BM_GETCHECK，避免 owner-draw 语义不一致)。
// 启动时从 cfg 同步；点击时由 toggle_checkbox 翻转，同时写回 cfg(start_auto / start_minimized)。
static CHK_AUTOSTART: AtomicBool = AtomicBool::new(true);
static CHK_START_MINIMIZED: AtomicBool = AtomicBool::new(true);
static CHK_NIGHT_LIGHT: AtomicBool = AtomicBool::new(false);
static CHK_NOTIFICATIONS: AtomicBool = AtomicBool::new(true);

// ---- 界面配色：由应用自己的主题判断(is_dark)决定，深浅色各一套，保证可读且一致 ----
const BG_LIGHT: u32 = 0x00F2F2F2; // 浅色窗口背景
const TEXT_LIGHT: u32 = 0x001E1E1E; // 浅色正文
const EDIT_LIGHT: u32 = 0x00FFFFFF; // 浅色输入框
const BG_DARK: u32 = 0x001F1F1F; // 深色窗口背景
const TEXT_DARK: u32 = 0x00F2F2F2; // 深色正文
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
const CHK_ACCENT: u32 = 0x00D77800; // 复选框勾选的强调蓝 RGB(0,120,215)，COLORREF 字节序 BBGGRR
const CHK_BOX_BORDER_DARK: u32 = 0x009A9A9A; // 深色下未勾选框边框
const CHK_BOX_BORDER_LIGHT: u32 = 0x00808080; // 浅色下未勾选框边框

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

/// COLORREF(0x00BBGGRR) 各通道线性插值：t=0 → a，t=1 → b。
fn lerp_color(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |sh: u32| -> u32 {
        let x = ((a >> sh) & 0xFF) as f32;
        let y = ((b >> sh) & 0xFF) as f32;
        (((x + (y - x) * t).round() as u32) & 0xFF) << sh
    };
    mix(16) | mix(8) | mix(0)
}

/// 自绘复选框绘制(处理主窗口 WM_DRAWITEM，勾选框 + 文字都跟随主题)。
/// 支持 hover/按压明暗反馈(系统经 itemState 传入)与子复选框的展开动画(整行向背景色
/// 淡入淡出 + 从上方 ~10px 滑入)。返回 nonzero = 已处理该消息。
unsafe fn draw_owner_checkbox(dis: &DRAWITEMSTRUCT) -> LRESULT {
    use windows_sys::Win32::Graphics::Gdi::{
        CreatePen, LineTo, MoveToEx, RoundRect, PS_SOLID,
    };
    let hdc = dis.hDC;
    let rc = dis.rcItem;

    // hover/按压状态：BS_OWNERDRAW 按钮控件自己跟踪鼠标，经 itemState 告诉我们
    let disabled = (dis.itemState & ODS_DISABLED) != 0;
    let pressed = !disabled && (dis.itemState & ODS_SELECTED) != 0;
    let hot = !disabled && !pressed && (dis.itemState & ODS_HOTLIGHT) != 0;

    // 展开动画进度(仅子复选框参与)：p∈[0,1]，1 = 完全显示
    let frame = if dis.CtlID == gui::ID_CHK_START_MINIMIZED {
        SUB_ANIM_FRAME.load(Ordering::Relaxed)
    } else {
        SUB_ANIM_FRAMES
    };
    let p = frame as f32 / SUB_ANIM_FRAMES as f32;
    let fade = 1.0 - p; // 1 = 完全融入背景，0 = 完全显示
    // 滑入：内容从上方 ~10px 处滑到位(在控件自身 DC 内平移，不会盖住别的控件)
    let dy = (-(1.0 - p) * 10.0).round() as i32;

    // 背景：跟窗口一致，避免露出默认按钮底色(不参与淡入，始终不透明遮底)
    let bg = bg_color();
    let bgbr = CreateSolidBrush(bg);
    FillRect(hdc, &rc, bgbr);
    DeleteObject(bgbr);

    // 勾选状态：读自维护状态(owner-draw 的 BM_GETCHECK 语义不可靠)
    let checked = chk_state(dis.CtlID);

    // 取文本
    let len = GetWindowTextLengthW(dis.hwndItem);
    let mut buf: Vec<u16> = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(dis.hwndItem, buf.as_mut_ptr(), buf.len() as i32);

    let cy = (rc.bottom - rc.top) as f32;
    let box_sz = ((cy - 6.0).max(14.0).min(22.0)) as i32;
    let box_x = rc.left + 2;
    let box_y = rc.top + (rc.bottom - rc.top - box_sz) / 2 + dy;
    let r = (box_sz as f32 * 0.25) as i32; // 圆角半径

    // 颜色：基色 → hover/按压明暗 → 动画淡入(向背景色融合)
    let txt = text_color();
    let (mut fill, mut border) = if checked {
        (CHK_ACCENT, CHK_ACCENT)
    } else {
        (
            edit_color(),
            if is_dark() { CHK_BOX_BORDER_DARK } else { CHK_BOX_BORDER_LIGHT },
        )
    };
    if hot {
        if checked {
            fill = lerp_color(fill, 0x00FFFFFF, 0.12); // 悬停：略微提亮
        } else {
            border = lerp_color(border, txt, 0.40); // 悬停：边框加深/加亮
        }
    }
    if pressed {
        if checked {
            fill = lerp_color(fill, 0x00000000, 0.20); // 按下：明显加深
        } else {
            fill = lerp_color(fill, txt, 0.10);
            border = lerp_color(border, txt, 0.55);
        }
    }
    if fade > 0.0 {
        fill = lerp_color(fill, bg, fade);
        border = lerp_color(border, bg, fade);
    }

    if checked {
        // 蓝底圆角 + 白勾
        let br = CreateSolidBrush(fill);
        let pen = CreatePen(PS_SOLID, 1, border);
        let oldbr = SelectObject(hdc, br);
        let oldpen = SelectObject(hdc, pen);
        RoundRect(hdc, box_x, box_y, box_x + box_sz, box_y + box_sz, r, r);
        SelectObject(hdc, oldbr);
        SelectObject(hdc, oldpen);
        DeleteObject(br);
        DeleteObject(pen);

        let check_c = lerp_color(0x00FFFFFF, bg, fade);
        let cpen = CreatePen(PS_SOLID, 2, check_c);
        let oldcpen = SelectObject(hdc, cpen);
        let mut prev_pt: POINT = std::mem::zeroed();
        let (x0, y0, s) = (box_x as f32, box_y as f32, box_sz as f32);
        // 画一个勾：起点 -> 折角 -> 终点
        MoveToEx(hdc, (x0 + s * 0.22) as i32, (y0 + s * 0.53) as i32, &mut prev_pt);
        LineTo(hdc, (x0 + s * 0.42) as i32, (y0 + s * 0.73) as i32);
        LineTo(hdc, (x0 + s * 0.80) as i32, (y0 + s * 0.27) as i32);
        SelectObject(hdc, oldcpen);
        DeleteObject(cpen);
    } else {
        // 空框：填充 + 边框
        let br = CreateSolidBrush(fill);
        let pen = CreatePen(PS_SOLID, 1, border);
        let oldbr = SelectObject(hdc, br);
        let oldpen = SelectObject(hdc, pen);
        RoundRect(hdc, box_x, box_y, box_x + box_sz, box_y + box_sz, r, r);
        SelectObject(hdc, oldbr);
        SelectObject(hdc, oldpen);
        DeleteObject(br);
        DeleteObject(pen);
    }

    // 文字(theme 色，单行垂直居中，随动画淡入 + 位移)
    if n > 0 {
        SetBkMode(hdc, 1); // TRANSPARENT
        SetTextColor(hdc, lerp_color(txt, bg, fade));
        let mut tr = rc;
        tr.top += dy;
        tr.bottom += dy;
        tr.left = box_x + box_sz + 6;
        tr.right = rc.right - 2;
        // DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX
        DrawTextW(hdc, buf.as_ptr(), n, &mut tr, 0x0860);
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
/// 兼容性：值 20 仅 Win11 22000+；Win10 1809+ 用的是早期未公开的同一属性 = 19。
/// 先试 20，失败再回退 19，避免 Win10 上标题栏不变深色。
unsafe fn apply_titlebar_theme(hwnd: HWND) {
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    const DWMWA_USE_IMMERSIVE_DARK_MODE_11: u32 = 20; // Win11
    const DWMWA_USE_IMMERSIVE_DARK_MODE_10: u32 = 19; // Win10 1809+，未公开
    let dark: i32 = if is_dark() { 1 } else { 0 };
    let attr = &dark as *const i32 as *const std::ffi::c_void;
    let sz = std::mem::size_of::<i32>() as u32;
    let hr = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE_11, attr, sz);
    if hr != 0 {
        // Win10：用旧值 19 再试一次
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE_10, attr, sz);
    }
}

// ---- 让系统通用控件跟随深色模式(未公开 API，失败则静默保持现状)----

/// PerMonitorV2 DPI 感知。Win10 1703+ 的 user32 才有 SetProcessDpiAwarenessContext，
/// 直接 import 会让旧版 Win10 因"找不到入口点"而**加载失败**。这里动态获取，缺失则交给 manifest 兜底。
unsafe fn enable_per_monitor_v2() {
    type FnSetProcessDpiAwarenessContext = unsafe extern "system" fn(isize) -> i32;
    let dll = LoadLibraryW(widestring("user32.dll").as_ptr());
    if dll == 0 {
        return;
    }
    let proc = GetProcAddress(dll, b"SetProcessDpiAwarenessContext\0".as_ptr() as PCSTR);
    if let Some(base) = proc {
        let f: FnSetProcessDpiAwarenessContext = std::mem::transmute(base);
        let _ = f(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// 进程启动时调用一次：告诉系统该应用"允许深色模式"(通用控件/菜单才会跟随主题)。
unsafe fn enable_dark_mode() {
    set_preferred_app_mode(1); // PreferredAppMode::AllowDark
}

/// uxtheme 非公开 API：SetPreferredAppMode。
/// 0=Default 1=AllowDark(跟随系统) 2=ForceDark(强制深色) 3=ForceLight(强制浅色)
unsafe fn set_preferred_app_mode(mode: u32) {
    type SetPreferredAppMode = unsafe extern "system" fn(u32) -> u32;
    let dll = LoadLibraryW(widestring("uxtheme.dll").as_ptr());
    if dll == 0 {
        return;
    }
    let proc = GetProcAddress(dll, b"SetPreferredAppMode\0".as_ptr() as PCSTR);
    if let Some(base) = proc {
        let f: SetPreferredAppMode = std::mem::transmute(base);
        f(mode);
    }
}

/// 让指定窗口的深色模式生效(AllowDarkModeForWindow)。`on` 跟随应用当前主题。
unsafe fn allow_dark_window(hwnd: HWND, on: bool) {
    type AllowDark = unsafe extern "system" fn(isize, u32) -> u32;
    let dll = LoadLibraryW(widestring("uxtheme.dll").as_ptr());
    if dll == 0 {
        return;
    }
    let proc = GetProcAddress(dll, b"AllowDarkModeForWindow\0".as_ptr() as PCSTR);
    if let Some(base) = proc {
        let f: AllowDark = std::mem::transmute(base);
        f(hwnd, on as u32);
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
    use windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible;
    if IsWindowVisible(hwnd) != 0 {
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
    // 规范化：全角标点/数字 → 半角，"2000" → "20:00"
    let norm = |s: &str| -> String {
        let s: String = s
            .trim()
            .chars()
            .map(|c| match c {
                // 全角冒号 → 半角
                '：' | '︓' => ':',
                // 全角数字 → 半角
                '０'..='９' => (c as u32 - '０' as u32 + '0' as u32) as u8 as char,
                _ => c,
            })
            .collect();
        if s.len() == 4 && s.chars().all(|c| c.is_ascii_digit()) {
            format!("{}:{}", &s[0..2], &s[2..4])
        } else {
            s
        }
    };
    if let Some(s) = APP_STATE.get() {
        let mut st = s.lock().unwrap();
        st.cfg.light_time = norm(&s1);
        st.cfg.dark_time = norm(&s2);
        let _ = config::save(&st.cfg);
        log(&format!(
            "时间已保存：浅色 {} 深色 {}",
            st.cfg.light_time, st.cfg.dark_time
        ));
    }
}

/// 开机自启复选框被点击：按当前勾选状态写入/移除开机启动，并保存配置。
unsafe fn on_autostart_clicked(_hwnd: HWND) {
    let on = chk_state(gui::ID_CHK_AUTOSTART);
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
    // 按 DPI 缩放窗口尺寸，避免高分屏下文字溢出
    let scale = gui::dpi_scale_for_window(parent);
    let ww: i32 = (450.0 * scale).round() as i32;
    let wh: i32 = (540.0 * scale).round() as i32;
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

    // 非模态：不禁用父窗口，也不跑局部消息循环。about 有独立 wnd_proc，
    // 主窗口的消息循环会照常把它的消息派发过去，因此主窗口可正常交互，不会“点了卡死”。
    ShowWindow(hwnd, SW_SHOWNORMAL);
    SetForegroundWindow(hwnd);
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
            // "关闭"按钮(自绘，跟主窗口按钮风格一致)，随 DPI 缩放并加大
            let hinst = GetModuleHandleW(ptr::null());
            let scale = crate::gui::dpi_scale_for_window(hwnd);
            let btn_w = (140.0 * scale).round() as i32;
            let btn_h = (44.0 * scale).round() as i32;
            // 居中放底部
            let r = get_window_rect_content(hwnd);
            let x = (r.right - r.left - btn_w) / 2;
            let y = (r.bottom - r.top) - btn_h - (16.0 * scale) as i32;
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
            let f = crate::gui::ui_font(scale);
            if f != 0 {
                use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;
                use windows_sys::Win32::Foundation::WPARAM;
                SendMessageW(btn, WM_SETFONT, f as WPARAM, 1);
            }
            // 标题栏跟随主题(深/浅色)，避免深色模式下标题栏还是白色
            apply_titlebar_theme(hwnd);
            // 应用窗口图标(标题栏/任务栏)
            apply_app_icon(hwnd);
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
        0x000F /* WM_PAINT */ => {
            paint_about(hwnd);
            0
        }
        0x0111 /* WM_COMMAND */ => {
            let id = (wparam as u32) & 0xFFFF;
            if id == ID_OK {
                // 非模态：直接关掉自己即可，主窗口本就可用
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
         0x0200 /* WM_MOUSEMOVE */ => {
            // 悬停在"仓库"链接行上：变色 + 手型光标；离开时还原
            let over = about_link_hit(hwnd);
            if over != ABOUT_LINK_HOVER.swap(over, Ordering::Relaxed) {
                InvalidateRect(hwnd, ptr::null(), 0);
            }
            // 注册鼠标离开通知
            let mut tme: TRACKMOUSEEVENT = std::mem::zeroed();
            tme.cbSize = std::mem::size_of::<TRACKMOUSEEVENT>() as u32;
            tme.dwFlags = 0x0002; // TME_LEAVE
            tme.hwndTrack = hwnd;
            TrackMouseEvent(&mut tme);
            return 0;
        }
         0x02A3 /* WM_MOUSELEAVE */ => {
            if ABOUT_LINK_HOVER.swap(false, Ordering::Relaxed) {
                InvalidateRect(hwnd, ptr::null(), 0);
            }
            return 0;
        }
         0x0020 /* WM_SETCURSOR */ => {
            if about_link_hit(hwnd) {
                SetCursor(LoadCursorW(0, IDC_HAND));
                return 1;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
         0x0201 /* WM_LBUTTONDOWN */ => {
            // 点击"仓库"链接行 → 用默认浏览器打开
            let x = (lparam as u32 & 0xFFFF) as i16 as i32;
            let y = ((lparam as u32 >> 16) & 0xFFFF) as i16 as i32;
            let pt = POINT { x, y };
            if let Some(r) = *about_link_rect().lock().unwrap() {
                if pt.x >= r.left && pt.x <= r.right && pt.y >= r.top && pt.y <= r.bottom {
                    open_url("https://github.com/stephen-cusi/wintheme-auto");
                }
            }
            return 0;
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
        BeginPaint, EndPaint, DrawTextW, FillRect, GetTextMetricsW, SelectObject, SetBkMode,
        SetTextColor, TEXTMETRICW, PAINTSTRUCT,
    };
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    if hdc == 0 {
        return;
    }
    let rc = get_window_rect_content(hwnd);
    // 显式按主题填充背景，否则 BEGINPAINT 可能不清屏，导致黑底 + 深色字不可见
    FillRect(hdc, &rc, win_brush());
    // 按 DPI 缩放文字(选统一 UI 字体)与布局，避免高分屏下溢出
    let scale = gui::dpi_scale_for_window(hwnd);
    let old_font = SelectObject(hdc, gui::ui_font(scale));
    let version = env!("CARGO_PKG_VERSION");
    let commit = build_commit();
    // 文字：先居中标题("WinTheme Auto")，再列各字段
    let title_w = widestring(&format!("WinTheme Auto v{version}-{commit}"));
    let mut title_rc = rc;
    title_rc.left += (16.0 * scale) as i32;
    title_rc.right -= (16.0 * scale) as i32;
    title_rc.top += (24.0 * scale) as i32;
    title_rc.bottom = title_rc.top + (36.0 * scale) as i32;
    SetBkMode(hdc, 1); // TRANSPARENT
    SetTextColor(hdc, if is_dark() { 0xFFFFFF } else { 0x1E1E1E });
    // 0x0025 = CENTER | VCENTER | SINGLELINE
    DrawTextW(hdc, title_w.as_ptr(), title_w.len() as i32, &mut title_rc, 0x0025);

    // body 文本：逐行绘制，方便把「仓库」那一行做成可点击链接
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
        format!("架构：{} · 构建 {}", arch_label(), build_commit()),
        "".to_string(),
        "原生 Win32 + Rust 编写，零额外运行时依赖。".to_string(),
        "采用 vibe coding 方式开发。".to_string(),
    ];
    let body_color = if is_dark() { 0xFFFFFFu32 } else { 0x1E1E1Eu32 };
    let link_color = if is_dark() { 0x6FB3FFu32 } else { 0x0059C8u32 };
    let link_color_hover = if is_dark() { 0x8FC7FFu32 } else { 0x0A7AF0u32 };
    let link_hover = ABOUT_LINK_HOVER.load(Ordering::Relaxed);
    let body_left = rc.left + (20.0 * scale) as i32;
    let body_right = rc.right - (20.0 * scale) as i32;
    let mut ty = rc.top + (70.0 * scale) as i32;
    let mut tm: TEXTMETRICW = std::mem::zeroed();
    GetTextMetricsW(hdc, &mut tm);
    let lh = (tm.tmHeight as i32).max(20);
    for line in &lines {
        let mut lr = RECT { left: body_left, top: ty, right: body_right, bottom: ty + lh };
        let is_link = line.contains("github.com");
        if is_link {
            SetTextColor(hdc, if link_hover { link_color_hover } else { link_color });
        } else {
            SetTextColor(hdc, body_color);
        }
        let lw = widestring(line);
        // DT_SINGLELINE | DT_NOPREFIX（悬停时加 DT_UNDERLINE）
        let mut dt = 0x0020 | 0x0800;
        if is_link && link_hover {
            dt |= 0x0200;
        }
        DrawTextW(hdc, lw.as_ptr(), lw.len() as i32, &mut lr, dt);
        if is_link {
            *about_link_rect().lock().unwrap() = Some(lr);
        }
        SetTextColor(hdc, body_color);
        ty += lh;
    }
    SelectObject(hdc, old_font);
    let _ = EndPaint(hwnd, &mut ps);
}

/// 当前二进制编译时的 CPU 架构标识（x64 / ARM64 / x86 / …）。
fn arch_label() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "ARM64",
        "x86" => "x86",
        other => other,
    }
}

/// 构建时的 git 提交短 SHA（build.rs 注入 GIT_SHA；未注入回退 "dev"）。
fn build_commit() -> &'static str {
    option_env!("GIT_SHA").unwrap_or("dev")
}

/// 保存"仓库"链接行在关于窗口里的客户端矩形，供点击检测用。
fn about_link_rect() -> &'static std::sync::Mutex<Option<RECT>> {
    use std::sync::OnceLock;
    static R: OnceLock<std::sync::Mutex<Option<RECT>>> = OnceLock::new();
    R.get_or_init(|| std::sync::Mutex::new(None))
}

/// 关于窗口"仓库"链接是否处于鼠标悬停(用于变色 + 点击手型光标)。
static ABOUT_LINK_HOVER: AtomicBool = AtomicBool::new(false);

/// 鼠标当前是否落在"仓库"链接矩形上(客户端坐标)。
unsafe fn about_link_hit(hwnd: HWND) -> bool {
    let Some(r) = *about_link_rect().lock().unwrap() else {
        return false;
    };
    let mut p = POINT { x: 0, y: 0 };
    GetCursorPos(&mut p);
    ScreenToClient(hwnd, &mut p);
    p.x >= r.left && p.x <= r.right && p.y >= r.top && p.y <= r.bottom
}

/// 用默认浏览器打开网址。
unsafe fn open_url(url: &str) {
    let verb_us: Vec<u16> = widestring("open");
    let url_w: Vec<u16> = widestring(url);
    ShellExecuteW(0, verb_us.as_ptr(), url_w.as_ptr(), ptr::null(), ptr::null(), SW_SHOWNORMAL);
}

/// 取指定复选框当前是否勾选(读自维护的状态)。
fn chk_state(id: u32) -> bool {
    match id {
        gui::ID_CHK_AUTOSTART => CHK_AUTOSTART.load(Ordering::Relaxed),
        gui::ID_CHK_START_MINIMIZED => CHK_START_MINIMIZED.load(Ordering::Relaxed),
        gui::ID_CHK_NIGHT_LIGHT => CHK_NIGHT_LIGHT.load(Ordering::Relaxed),
        gui::ID_CHK_NOTIFICATIONS => CHK_NOTIFICATIONS.load(Ordering::Relaxed),
        _ => false,
    }
}

/// 翻转自绘复选框的勾选状态(自维护状态 + 同步系统 BM_SETCHECK + 重绘)。
unsafe fn toggle_checkbox(hwnd: HWND, id: u32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetDlgItem, SendMessageW};
    const BM_SETCHECK: u32 = 0x00F1;
    const BST_CHECKED: usize = 1;
    let new = !chk_state(id);
    match id {
        gui::ID_CHK_AUTOSTART => CHK_AUTOSTART.store(new, Ordering::Relaxed),
        gui::ID_CHK_START_MINIMIZED => CHK_START_MINIMIZED.store(new, Ordering::Relaxed),
        gui::ID_CHK_NIGHT_LIGHT => CHK_NIGHT_LIGHT.store(new, Ordering::Relaxed),
        gui::ID_CHK_NOTIFICATIONS => CHK_NOTIFICATIONS.store(new, Ordering::Relaxed),
        _ => {}
    }
    let h = GetDlgItem(hwnd, id as i32);
    if h != 0 {
        SendMessageW(h, BM_SETCHECK, if new { BST_CHECKED } else { 0 }, 0);
        // 局部重绘复选框本身(不整窗刷新)，并同步上屏，避免点击反馈被后续 I/O 拖慢
        InvalidateRect(h, ptr::null(), 1);
        use windows_sys::Win32::Graphics::Gdi::UpdateWindow;
        UpdateWindow(h);
    }
}

/// "开机时只在托盘后台运行"子选项：跟随 auto_start 切换显隐，带 ~144ms 滑入/淡出动画。
/// 只动它自己(局部重绘)，不触发整窗刷新；快速反复点击时从当前帧继续，不跳变。
unsafe fn sync_sub_checkbox_visibility(hwnd: HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow;
    let show = APP_STATE
        .get()
        .and_then(|s| s.lock().ok())
        .map(|st| st.cfg.auto_start)
        .unwrap_or(false);
    let h = GetDlgItem(hwnd, gui::ID_CHK_START_MINIMIZED as i32);
    if h == 0 {
        return;
    }
    let animating = SUB_ANIM_ACTIVE.load(Ordering::Relaxed);
    // 已是目标状态且没在动画中：无事可做
    if !animating && (IsWindowVisible(h) != 0) == show {
        return;
    }
    SUB_ANIM_SHOWING.store(show, Ordering::Relaxed);
    if !animating {
        // 起点帧：展开从 0 → 满；收起从满 → 0
        SUB_ANIM_FRAME.store(if show { 0 } else { SUB_ANIM_FRAMES }, Ordering::Relaxed);
    }
    SUB_ANIM_ACTIVE.store(true, Ordering::Relaxed);
    if show {
        ShowWindow(h, 5); // 5=SW_SHOW
    }
    SetTimer(hwnd, TIMER_SUBMENU, 16, None);
    // 立即画出第一帧，避免首帧前有 16ms 空白
    InvalidateRect(h, ptr::null(), 1);
    UpdateWindow(h);
}

/// 开机静默启动复选框被点击：翻转 cfg.start_minimized 持久化。
/// 如果当前已写注册表自启，会同步更新注册表项的 --silent 标志。
unsafe fn on_start_minimized_clicked(_hwnd: HWND) {
    let on = chk_state(gui::ID_CHK_START_MINIMIZED);
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

/// 夜间模式复选框被点击
fn on_night_light_clicked() {
    let on = chk_state(gui::ID_CHK_NIGHT_LIGHT);
    if let Some(s) = APP_STATE.get() {
        let mut st = s.lock().unwrap();
        st.cfg.night_light = on;
        let _ = config::save(&st.cfg);
        // 如果当前是深色主题，立即同步
        if on && theme::get_theme().ok() == Some(Theme::Dark) {
            let _ = nightlight::set_enabled(true);
        } else if !on {
            let _ = nightlight::set_enabled(false);
        }
        log(&format!("夜间模式联动 = {}", on));
    }
}

/// 通知复选框被点击
fn on_notifications_clicked() {
    let on = chk_state(gui::ID_CHK_NOTIFICATIONS);
    if let Some(s) = APP_STATE.get() {
        let mut st = s.lock().unwrap();
        st.cfg.notifications = on;
        let _ = config::save(&st.cfg);
        log(&format!("通知 = {}", on));
    }
}

/// 切换主题时的反馈：任务栏闪烁 + Toast 通知

fn notify_theme_changed(hwnd: HWND, is_dark: bool) {
    let enabled = chk_state(gui::ID_CHK_NOTIFICATIONS);
    log(&format!("notify: is_dark={} enabled={} notifier={}", is_dark, enabled, NOTIFIER.get().is_some()));
    if !enabled { return; }
    let label = if is_dark { "已切换到深色模式" } else { "已切换到浅色模式" };
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{FlashWindowEx, FLASHWINFO, FLASHW_ALL};
        let mut fwi: FLASHWINFO = std::mem::zeroed();
        fwi.cbSize = std::mem::size_of::<FLASHWINFO>() as u32;
        fwi.hwnd = hwnd;
        fwi.dwFlags = FLASHW_ALL;
        fwi.uCount = 3;
        fwi.dwTimeout = 0;
        FlashWindowEx(&fwi);
    }
    if let Some(Some(n)) = NOTIFIER.get() {
        notify::show_raw(n, "WinTheme Auto", label);
    }
    log(label);
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

/// CBT 钩子抓到的托盘菜单 popup 窗口句柄(系统类名 "#32768")
static MENU_HWND: AtomicIsize = AtomicIsize::new(0);

/// CBT 钩子：TrackPopupMenu 弹出时系统会创建类名为 "#32768" 的菜单窗口，
/// 这里只在创建瞬间(HCBT_CREATEWND)记录句柄；圆角的 DWM 设置由 show_menu 里
/// 启动的辅助线程完成(创建瞬间 DWM 尚未接管窗口，直接调用会返回 E_HANDLE)。
/// 其余钩子事件必须原样 CallNextHookEx 传递，否则菜单行为会异常。
unsafe extern "system" fn menu_cbt_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HCBT_CREATEWND as i32 {
        let hwnd = wparam as HWND;
        let mut cls = [0u16; 8];
        let n = GetClassNameW(hwnd, cls.as_mut_ptr(), 8) as usize;
        const MENU_CLASS: [u16; 6] = [0x23, 0x33, 0x32, 0x37, 0x36, 0x38]; // "#32768"
        if n >= 6 && cls[..6] == MENU_CLASS {
            MENU_HWND.store(hwnd, Ordering::SeqCst);
        }
    }
    CallNextHookEx(0, code, wparam, lparam)
}

/// 给菜单窗口设置 Win11 圆角。DWM 在菜单窗口刚创建(HCBT_CREATEWND)时还未接管它，
/// 立即设置会返回 E_HANDLE，所以由辅助线程轮询重试，直到生效或窗口销毁。
/// DwmSetWindowAttribute 可跨线程调用，与 GUI 线程的 TrackPopupMenu 模态循环互不干扰。
unsafe fn apply_menu_rounding() {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };
    std::thread::spawn(|| {
        let pref: i32 = DWMWCP_ROUND;
        let attr_ptr = &pref as *const i32 as *const std::ffi::c_void;
        for _ in 0..60 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let h = MENU_HWND.load(Ordering::SeqCst);
            if h == 0 {
                // 等钩子抓到菜单窗口
                continue;
            }
            let hr = DwmSetWindowAttribute(
                h as HWND,
                DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                attr_ptr,
                std::mem::size_of::<i32>() as u32,
            );
            if hr == ERROR_SUCCESS as i32 {
                return;
            }
            // 窗口已销毁(菜单瞬间关闭)则放弃；否则继续重试
            if hr != 0 && MENU_HWND.load(Ordering::SeqCst) != h {
                return;
            }
        }
    });
}

/// 弹出托盘菜单：原生 TrackPopupMenu 承担弹出/键盘/点击外部消散；
/// 条目仍 owner-draw(由 wnd_proc 的 WM_MEASUREITEM/WM_DRAWITEM 自绘深浅色)。
unsafe fn show_menu(hwnd: HWND) {
    let (mode, cur) = {
        let lock = APP_STATE.get().unwrap().lock().unwrap();
        (lock.cfg.mode.clone(), theme::get_theme().ok())
    };
    let mut items: Vec<MenuItem> = Vec::new();
    items.push(MenuItem { id: ID_LIGHT,    text: "切换到浅色".to_string(), separator: false, checked: cur == Some(Theme::Light),  enabled: true });
    items.push(MenuItem { id: ID_DARK,     text: "切换到深色".to_string(), separator: false, checked: cur == Some(Theme::Dark),   enabled: true });
    items.push(MenuItem { id: 0,           text: String::new(), separator: true,  checked: false, enabled: true });
    items.push(MenuItem { id: ID_MODE_SUN, text: "模式：跟随日出日落".to_string(), separator: false, checked: mode == "sun",        enabled: true });
    items.push(MenuItem { id: ID_MODE_SCHED, text: "模式：定时切换".to_string(),    separator: false, checked: mode == "schedule",   enabled: true });
    items.push(MenuItem { id: ID_OFF,      text: "模式：暂停(手动)".to_string(),    separator: false, checked: mode == "off",        enabled: true });
    items.push(MenuItem { id: 0,           text: String::new(), separator: true,  checked: false, enabled: true });
    items.push(MenuItem { id: ID_CHECK,    text: "刷新位置与时区".to_string(), separator: false, checked: false, enabled: true });
    items.push(MenuItem { id: ID_CONFIG,   text: "打开配置文件".to_string(),  separator: false, checked: false, enabled: true });
    items.push(MenuItem { id: 0,           text: String::new(), separator: true,  checked: false, enabled: true });
    items.push(MenuItem { id: ID_EXIT,     text: "退出".to_string(),         separator: false, checked: false, enabled: true });

    // 替换全局菜单快照(绘制时按 itemData 索引反查)
    {
        let mut g = MENU_ITEMS.lock().unwrap();
        *g = items;
    }

    let menu = CreatePopupMenu();
    if menu == 0 {
        return;
    }
    {
        let items = MENU_ITEMS.lock().unwrap();
        for (i, it) in items.iter().enumerate() {
            // 分隔线同样 owner-draw(id=0)：与条目同底色的无缝分隔
            let flags = MF_OWNERDRAW | if it.checked { MF_CHECKED } else { 0 };
            AppendMenuW(menu, flags, it.id as usize, i as *const u16);
        }
    }

    // 菜单窗口底色与条目同色：边缘/圆角露出的部分不再出现系统浅色底
    let menu_bg: u32 = if is_dark() { 0x1F1F1F } else { 0xF2F2F2 };
    let hbr_back = CreateSolidBrush(menu_bg);
    let mi = MENUINFO {
        cbSize: std::mem::size_of::<MENUINFO>() as u32,
        fMask: MIM_BACKGROUND | MIM_APPLYTOSUBMENUS,
        dwStyle: 0,
        cyMax: 0,
        hbrBack: hbr_back,
        dwContextHelpID: 0,
        dwMenuData: 0,
    };
    SetMenuInfo(menu, &mi);

    // 菜单边框由系统按 uxtheme 菜单主题绘制，AllowDark 只是"跟随系统"，
    // app 深色+系统浅色时会画浅色边框(丑)。这里按 app 主题强制 ForceDark/ForceLight，
    // 让边框与菜单同色；弹完恢复 AllowDark。FlushMenuThemes 立即生效。
    set_preferred_app_mode(if is_dark() { 2 } else { 3 });
    flush_menu_themes();

    // CBT 钩子抓菜单窗口(#32768)设置 DWM 圆角；菜单关闭后卸钩并恢复主题模式
    MENU_HWND.store(0, Ordering::SeqCst);
    let hook = SetWindowsHookExW(WH_CBT, Some(menu_cbt_hook), 0, GetCurrentThreadId());
    apply_menu_rounding();

    // 经典托盘菜单模式(KB135788)：先 SetForegroundWindow，TrackPopupMenu 才能在
    // 点击外部时消散；结束后补发 WM_NULL 修正焦点归属。
    SetForegroundWindow(hwnd);
    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    let cmd = TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_RETURNCMD,
        pt.x, pt.y,
        0, hwnd,
        std::ptr::null(),
    );
    if hook != 0 {
        UnhookWindowsHookEx(hook);
    }
    set_preferred_app_mode(1); // 恢复 AllowDark(跟随系统)
    flush_menu_themes();
    PostMessageW(hwnd, 0x0000, 0, 0); // WM_NULL
    DeleteObject(hbr_back);
    DestroyMenu(menu);
    if cmd != 0 {
        handle_menu_cmd(hwnd, cmd as u32);
        refresh_ui(hwnd); // 托盘菜单操作后刷新主窗口
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
    let k = gui::dpi_scale_for_system();
    if items[idx].separator {
        mis.itemHeight = (12.0 * k).round() as u32;
        mis.itemWidth = 100;
        return 1;
    }
    mis.itemHeight = menu_item_height();
    // 文字宽度：用大号菜单字体算
    let hdc = GetDC(0);
    if hdc == 0 {
        mis.itemWidth = 220;
        return 1;
    }
    let prev_font = SelectObject(hdc, menu_font());
    let text_w: Vec<u16> = items[idx].text.encode_utf16().collect();
    let mut size = SIZE { cx: 0, cy: 0 };
    GetTextExtentPoint32W(hdc, text_w.as_ptr(), text_w.len() as i32, &mut size);
    SelectObject(hdc, prev_font);
    ReleaseDC(0, hdc);
    // 左内边距 + ✓ 列 + 间隙 + 右内边距(全部随 DPI 缩放)；size.cx 是 i32，统一在 i32 里算
    mis.itemWidth =
        (size.cx + (14.0 * k).round() as i32 * 2 + (20.0 * k).round() as i32 + (6.0 * k).round() as i32) as u32;
    1
}

/// 菜单用的大号字体(比 UI 字体更大，营造 Windows 安全中心那种宽松菜单)。
fn menu_font() -> windows_sys::Win32::Graphics::Gdi::HFONT {
    use std::sync::OnceLock;
    use windows_sys::Win32::Graphics::Gdi::{CreateFontW, HFONT};
    static F: OnceLock<HFONT> = OnceLock::new();
    *F.get_or_init(|| unsafe {
        let k = gui::dpi_scale_for_system();
        let name = widestring("Microsoft YaHei UI");
        CreateFontW(
            (18.0 * k).round() as i32,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0x86, // GB2312_CHARSET
            0,
            0,
            0,
            0,
            name.as_ptr(),
        )
    })
}

/// 菜单项高度(随 DPI 缩放)。
fn menu_item_height() -> u32 {
    (36.0 * gui::dpi_scale_for_system()).round() as u32
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
    let rc = dis.rcItem;
    let state = dis.itemState;
    let selected = (state & ODS_SELECTED) != 0;
    let grayed   = (state & ODS_DISABLED) != 0;

    // 配色：深浅色两套；悬停/选中画圆角高亮(Defender 风格)；disabled 文字更暗
    let (bg, fg_text) = if is_dark() {
        (0x1F1F1Fu32, 0xE8E8E8u32)
    } else {
        (0xF2F2F2u32, 0x1E1E1Eu32)
    };
    let fg = if grayed { if is_dark() { 0x808080 } else { 0xA0A0A0 } } else { fg_text };

    // 背景：整格铺纯色(即便弹窗底色偏白，也保证条目是深色)
    let brush = CreateSolidBrush(bg);
    FillRect(hdc, &rc, brush);
    DeleteObject(brush);

    if item.separator {
        // 去掉显眼的横线/边框：只保留与菜单项一致的背景色，做成无痕迹分隔
        return 1;
    }

    // 悬停/选中：内缩圆角高亮，中性灰(对齐 Defender/Win11 菜单的规整感)
    if selected {
        let k = gui::dpi_scale_for_system();
        let pad = (3.0 * k).round() as i32;
        let hr = RECT {
            left: rc.left + pad,
            top: rc.top + pad,
            right: rc.right - pad,
            bottom: rc.bottom - pad,
        };
        let hl = if is_dark() { 0x3A3A3Au32 } else { 0xDEDEDEu32 };
        let rad = (6.0 * k).round() as i32;
        let rgn = CreateRoundRectRgn(hr.left, hr.top, hr.right + 1, hr.bottom + 1, rad, rad);
        let b = CreateSolidBrush(hl);
        FillRgn(hdc, rgn, b);
        DeleteObject(b);
        DeleteObject(rgn);
    }

    // 用大号菜单字体画 ✓ 和文字
    let old_font = SelectObject(hdc, menu_font());
    let k = gui::dpi_scale_for_system();
    // 统一内边距：✓ 和文字全部左对齐(不再居中)，✓ 在紧贴文字的固定列
    let pad_l = (14.0 * k).round() as i32;
    let gutter = (20.0 * k).round() as i32;
    let gap = (6.0 * k).round() as i32;

    // ✓ 列(固定位置，与文字左缘对齐成一条线；未勾选项同样留空，保证文字齐整)
    if item.checked {
        // 画一个 ✓(Segoe UI Symbol 字符 ✓，U+2713)
        let check_w: Vec<u16> = "\u{2713}".encode_utf16().collect();
        let mut crc = rc;
        crc.left += pad_l;
        crc.right = crc.left + gutter;
        SetBkMode(hdc, 1); // TRANSPARENT
        SetTextColor(hdc, fg);
        // DT_LEFT(0x0) | DT_VCENTER(0x4) | DT_SINGLELINE(0x20)
        DrawTextW(hdc, check_w.as_ptr(), check_w.len() as i32, &mut crc, 0x0024);
    }

    // 文字：左对齐 + 统一内边距(规整的关键；原先 DT_CENTER 居中导致参差)。
    // 未勾选项同样给 ✓ 列留空位，保证所有文字左缘对齐成一条线。
    let text_w: Vec<u16> = item.text.encode_utf16().collect();
    let mut trc = rc;
    trc.left += pad_l + gutter + gap;
    trc.right -= pad_l;
    SetBkMode(hdc, 1);
    SetTextColor(hdc, fg);
    DrawTextW(hdc, text_w.as_ptr(), text_w.len() as i32, &mut trc, 0x0024);

    SelectObject(hdc, old_font);
    1
}

/// 手动改主题时调用：记录"自动化当前想要的主题"作为手动覆盖参照，
/// 使 tick() 只在到达下一个自动切换边界时才恢复自动(避免手动切换被下一轮定时覆盖)。
fn mark_manual_override(s: &mut AppState) {
    let cur = theme::get_theme().unwrap_or(Theme::Light);
    let auto_desired = evaluate(s).unwrap_or(cur);
    s.manual_desired = Some(auto_desired);
}

unsafe fn handle_menu_cmd(hwnd: HWND, id: u32) {
    let mut s = APP_STATE.get().unwrap().lock().unwrap();    match id {
        ID_LIGHT => {
            mark_manual_override(&mut *s);
            let _ = theme::set_theme(Theme::Light);
            sync_night_light(&s.cfg, Theme::Light);
            notify_theme_changed(hwnd, false);
            log("手动切换到浅色");
        }
        ID_DARK => {
            mark_manual_override(&mut *s);
            let _ = theme::set_theme(Theme::Dark);
            sync_night_light(&s.cfg, Theme::Dark);
            notify_theme_changed(hwnd, true);
            log("手动切换到深色");
        }
        ID_TOGGLE => {
            mark_manual_override(&mut *s);
            if let Ok(cur) = theme::get_theme() {
                let new = if cur == Theme::Light { Theme::Dark } else { Theme::Light };
                let _ = theme::set_theme(new);
                sync_night_light(&s.cfg, new);
                notify_theme_changed(hwnd, new == Theme::Dark);
                log(if new == Theme::Dark { "手动切换到深色" } else { "手动切换到浅色" });
            }
        }
        ID_MODE_SUN => {
            s.cfg.mode = "sun".into();
            s.manual_desired = None; // 切换模式即清除手动覆盖，交给新模式管理
            let _ = config::save(&s.cfg);
            log("模式 -> 跟随日出日落");
        }
        ID_MODE_SCHED => {
            s.cfg.mode = "schedule".into();
            s.manual_desired = None;
            let _ = config::save(&s.cfg);
            log("模式 -> 定时切换");
        }
        ID_OFF => {
            s.cfg.mode = "off".into();
            s.manual_desired = None;
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
            // 干净退出：先隐藏窗口(让 DWM 重新合成底层桌面)，再强制 DWM 完成合成，最后销毁。
            // 这样既不会让自定义子控件在销毁瞬间擦成白闪，也不会留下半透明残影。
            ShowWindow(hwnd, SW_HIDE);
            use windows_sys::Win32::Graphics::Dwm::DwmFlush;
            unsafe { DwmFlush() };
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
    // 系统时区可能被用户改动（或开机时时钟错乱、后来才校正）。每轮 tick 重新读一次，
    // 若偏移变了就作废日出日落缓存，让本轮 evaluate 用新时区重算，避免显示/主题停留在旧数据。
    if let Ok(tz) = fetch_system_timezone() {
        if state.tz_offset_hours != Some(tz) {
            log(&format!("检测到系统时区变化 -> UTC offset {tz}，刷新日出日落缓存"));
            state.tz_offset_hours = Some(tz);
            state.sun_cache = None;
            state.sun_cache_date = None;
        }
    }
    let desired = evaluate(state)?;
    // 手动覆盖：用户在自动(sun/schedule)模式下手动改过主题，只要自动化想要的主题还没变，
    // 就尊重手动选择、不强制切回；直到进入下一个自动切换边界再恢复正常自动。
    if let Some(prev) = state.manual_desired {
        if desired == prev {
            return Ok(());
        }
        state.manual_desired = None; // 到达下一个切换边界：清除覆盖
    }
    let current = theme::get_theme().unwrap_or(desired);
    if current != desired {
        theme::set_theme(desired)?;
        if let Some(h) = state.tray_hwnd {
            notify_theme_changed(h, desired == Theme::Dark);
        }
        log(&format!("主题已切换为 {:?}", desired));
        sync_night_light(&state.cfg, desired);
    }
    Ok(())
}

fn sync_night_light(cfg: &Config, theme: Theme) {
    if !cfg.night_light { return; }
    if theme == Theme::Dark {
        let _ = nightlight::set_enabled(true);
    } else {
        let _ = nightlight::set_enabled(false);
    }
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
    // 全角标点/数字 → 半角，再兼容无冒号输入
    let s: String = s
        .trim()
        .chars()
        .map(|c| match c {
            '：' | '︓' => ':',
            '０'..='９' => (c as u32 - '０' as u32 + '0' as u32) as u8 as char,
            _ => c,
        })
        .collect();
    let formatted = if s.len() == 4 && s.chars().all(|c| c.is_ascii_digit()) {
        format!("{}:{}", &s[0..2], &s[2..4])
    } else {
        s
    };
    NaiveTime::parse_from_str(&formatted, "%H:%M").map_err(|e| anyhow!(e.to_string()))
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

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
use windows_sys::Win32::Foundation::{HWND, LRESULT, LPARAM, POINT, WPARAM};
use windows_sys::Win32::System::Console::GetConsoleWindow;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    ShellExecuteW, Shell_NotifyIconW,
};
use windows_sys::Win32::Graphics::Gdi::DeleteObject;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, CW_USEDEFAULT, DefWindowProcW,
    DestroyMenu, DestroyWindow, GetCursorPos, GetMessageW, HMENU, IDC_ARROW, LoadCursorW,
    LoadIconW, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    SetForegroundWindow, SetTimer, ShowWindow, SW_HIDE, SW_SHOWNORMAL, TrackPopupMenu,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TranslateMessage, DispatchMessageW, WM_CLOSE, WM_COMMAND,
    WM_CREATE, WM_DESTROY, WM_APP, WM_LBUTTONUP, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, HWND_MESSAGE,
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
    let args: Vec<String> = std::env::args().collect();
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();

    // 一次性命令（带 -- 前缀的都视为命令行调用，不隐藏控制台）
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
        println!("已移除开机启动注册表项。");
        return Ok(());
    }
    if args.iter().any(|a| a == "--status") {
        let t = theme::get_theme()
            .map(|t| if t == Theme::Light { "浅色(light)" } else { "深色(dark)" })
            .unwrap_or("未知");
        println!("当前主题: {}", t);
        return Ok(());
    }

    if args.iter().any(|a| a == "--install") {
        config::install_startup(&exe)?;
        println!("已写入开机启动注册表项，程序将在登录时自动运行。");
    }

    // --console：终端运行时不隐藏控制台，日志实时打到 stdout（便于观察）
    CONSOLE_MODE.store(args.iter().any(|a| a == "--console"), Ordering::Relaxed);

    // 单实例：已有常驻实例在跑时直接退出（避免双托盘/多进程困惑）。
    // CreateMutexW 返回 0 且错误码 183(ERROR_ALREADY_EXISTS) 表示锁已被占用。
    unsafe {
        let mutex_name = widestring("Local\\WinThemeAuto-SingleInstance");
        let m = CreateMutexW(ptr::null(), 1, mutex_name.as_ptr());
        if m == 0 && std::io::Error::last_os_error().raw_os_error() == Some(183) {
            eprintln!("wintheme-auto 已在运行（托盘常驻）。如需重启，请先从托盘菜单退出旧实例。");
            std::process::exit(0);
        }
    }

    let cfg = config::load()?;
    if cfg.auto_start && !args.iter().any(|a| a == "--no-autostart") {
        let _ = config::install_startup(&exe);
    }

    // 普通运行（无 -- 命令行参数）时隐藏控制台窗口
    if !args.iter().skip(1).any(|a| a.starts_with("--")) {
        unsafe { hide_console() };
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

    let use_tray = APP_STATE.get().unwrap().lock().unwrap().cfg.tray;
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
            (
                style_bits,
                0x00040000, // WS_EX_APPWINDOW
                0,          // 无父窗口（HWND 是 isize，空句柄传 0）
                gui::WIN_W,
                gui::WIN_H,
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
                gui::populate_main_window(hwnd, hinst);
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
                gui::ID_BTN_TOGGLE => handle_menu_cmd(hwnd, ID_TOGGLE),
                gui::ID_BTN_CHECK => handle_menu_cmd(hwnd, ID_CHECK),
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

/// 刷新 UI（托盘提示 + 主窗口）
unsafe fn refresh_ui(hwnd: HWND) {
    update_tooltip(hwnd);
    if IS_MAIN_WINDOW.load(Ordering::Relaxed) {
        gui::refresh_main_window(hwnd);
    }
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
    *ICON.get_or_init(|| unsafe { icon::load_icon(16) })
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

unsafe fn hide_console() {
    let hw = GetConsoleWindow();
    if hw != 0 {
        ShowWindow(hw, SW_HIDE);
    }
}

fn log(msg: &str) {
    if CONSOLE_MODE.load(Ordering::Relaxed) {
        println!("[wintheme-auto] {msg}");
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

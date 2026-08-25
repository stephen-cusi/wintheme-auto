// 主窗口 GUI：控件 ID、控件创建、状态刷新。
//
// 策略：所有 Win32 常量按原始 u32 处理，最后再包成 WINDOW_STYLE / WINDOW_EX_STYLE。
// windows-sys 0.52 的 NewType 没有实现 BitOr，所以位运算在 .0 上做。
//
// DPI：应用为 PerMonitorV2 DPI 感知，所有坐标按系统 DPI 缩放，否则在高分屏
// （如 200%）下会被系统拉伸而模糊。

#![allow(dead_code)]

use crate::config::Config;
use crate::icon;
use crate::sun;
use crate::theme::Theme;
use crate::APP_STATE;
use chrono::{Local, NaiveTime, Timelike};
use std::ptr;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{HINSTANCE, HWND};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, FW_BOLD, GB2312_CHARSET, HFONT,
};
use windows_sys::Win32::System::SystemServices::{SS_ICON, SS_LEFT};
use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetDlgItem, SetWindowTextW, BS_PUSHBUTTON, ES_AUTOHSCROLL, ES_NUMBER,
    WM_SETFONT,
};
use crate::theme;

// ---- 控件 ID ----
pub const ID_BTN_TOGGLE: u32 = 2001;
pub const ID_BTN_CYCLE_MODE: u32 = 2002;
pub const ID_BTN_CHECK: u32 = 2003;
pub const ID_BTN_OPEN_CFG: u32 = 2004;
pub const ID_BTN_HIDE: u32 = 2005;
pub const ID_BTN_EXIT: u32 = 2006;

pub const ID_LBL_ICON: u32 = 3001;
pub const ID_LBL_THEME: u32 = 3002;
pub const ID_LBL_MODE: u32 = 3003;
pub const ID_LBL_POS: u32 = 3004;
pub const ID_LBL_NEXT: u32 = 3005;
pub const ID_LBL_HINT: u32 = 3006;

pub const ID_EDIT_LIGHT: u32 = 4001;
pub const ID_EDIT_DARK: u32 = 4002;
pub const ID_BTN_SAVE_TIME: u32 = 4003;

pub const ID_CHK_AUTOSTART: u32 = 5001;

// 基础布局尺寸（按 96 DPI 设计，运行时按 DPI 缩放）
pub const BASE_W: i32 = 460;
pub const BASE_H: i32 = 420;

// 常用 raw u32 window style（避免与 windows-sys 的 NewType 混用）
mod raw {
    pub const WS_OVERLAPPED: u32 = 0x00C00000;
    pub const WS_CAPTION: u32 = 0x00C00000;
    pub const WS_SYSMENU: u32 = 0x00080000;
    pub const WS_MINIMIZEBOX: u32 = 0x00020000;
    pub const WS_VISIBLE: u32 = 0x10000000;
    pub const WS_CHILD: u32 = 0x40000000;
    pub const WS_TABSTOP: u32 = 0x00010000;
    pub const WS_CLIPCHILDREN: u32 = 0x02000000;
    pub const WS_CLIPSIBLINGS: u32 = 0x04000000;
    pub const WS_EX_APPWINDOW: u32 = 0x00040000;
    pub const WS_EX_CLIENTEDGE: u32 = 0x00000200;
    pub const STM_SETICON: u32 = 0x0170;
    pub const GWLP_USERDATA: i32 = -21;
}

fn w(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

// ---- DPI 帮助 ----

fn dpi_to_scale(dpi: u32) -> f64 {
    if dpi == 0 {
        1.0
    } else {
        dpi as f64 / 96.0
    }
}

/// 当前系统 DPI 缩放系数（用于创建主窗口前的初始尺寸、托盘图标等）。
pub fn dpi_scale_for_system() -> f64 {
    // SAFETY: 无窗口前置条件
    dpi_to_scale(unsafe { GetDpiForSystem() })
}

/// 某窗口当前所在显示器的 DPI 缩放系数。
pub fn dpi_scale_for_window(hwnd: HWND) -> f64 {
    // SAFETY: hwnd 须有效
    dpi_to_scale(unsafe { GetDpiForWindow(hwnd) })
}

// ---- 控件字体：统一用微软雅黑，大小随 DPI 缩放，避免默认小字体 ----
static UI_FONT: OnceLock<HFONT> = OnceLock::new();

fn ui_font(k: f64) -> HFONT {
    *UI_FONT.get_or_init(|| unsafe {
        CreateFontW(
            ((18.0 * k).round()) as i32, // 字高（像素，随 DPI 缩放）
            0,
            0,
            0,
            0, // FW_NORMAL：常规字重
            0,
            0,
            0,
            GB2312_CHARSET as u32,
            0,
            0,
            0,
            0,
            w("Microsoft YaHei UI").as_ptr(),
        )
    })
}

/// 给子控件应用统一的 UI 字体。
unsafe fn apply_ui_font(parent: HWND, control: HWND) {
    if control == 0 {
        return;
    }
    let k = dpi_scale_for_window(parent);
    let f = ui_font(k);
    if f != 0 {
        use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;
        use windows_sys::Win32::Foundation::WPARAM;
        SendMessageW(control, WM_SETFONT, f as WPARAM, 1);
    }
}

/// 在已创建的主窗口 `parent` 上填充子控件。
///
/// # Safety
/// 必须在主窗口已创建、且窗口类已注册后调用；hinst 须有效。
pub unsafe fn populate_main_window(parent: HWND, hinst: HINSTANCE) {
    let k = dpi_scale_for_window(parent);
    // 按 DPI 缩放坐标的辅助闭包
    let s = |v: i32| -> i32 { ((v as f64) * k).round() as i32 };
    let mx = s(26);
    let content_w = s(BASE_W - 2 * 26);

    // 大字号粗体字体（"当前主题"标签用），高度随 DPI 缩放
    let big_font: HFONT = CreateFontW(
        s(28),
        0,
        0,
        0,
        FW_BOLD as i32,
        0,
        0,
        0,
        GB2312_CHARSET as u32,
        0,
        0,
        0,
        0,
        w("Microsoft YaHei UI").as_ptr(),
    );

    // ---- 头部：大图标 + 当前主题大字 ----
    make_static(parent, hinst, ID_LBL_ICON, SS_ICON, mx, s(20), s(52), s(52), "");
    make_static(parent, hinst, ID_LBL_THEME, SS_LEFT, s(92), s(24), content_w - s(92 - 26), s(42), "");

    // ---- 状态信息行 ----
    let mut y = s(92);
    make_static(parent, hinst, ID_LBL_MODE, SS_LEFT, mx, y, content_w, s(20), "");
    y += s(22);
    make_static(parent, hinst, ID_LBL_POS, SS_LEFT, mx, y, content_w, s(20), "");
    y += s(22);
    make_static(parent, hinst, ID_LBL_NEXT, SS_LEFT, mx, y, content_w, s(20), "");
    y += s(28);

    // ---- 开机自启复选框 ----
    let auto_start = APP_STATE
        .get()
        .and_then(|s| s.lock().ok())
        .map(|st| st.cfg.auto_start)
        .unwrap_or(true);
    make_checkbox(parent, hinst, ID_CHK_AUTOSTART, mx, y, s(220), s(24), "开机自动启动", auto_start);
    y += s(26);

    // ---- 定时模式：时间编辑区 ----
    let row_h = s(24);
    const SS_CENTERIMAGE: u32 = 0x0200; // 文字垂直居中，便于与输入框对齐
    make_static(parent, hinst, 0, SS_LEFT | SS_CENTERIMAGE, mx, y, s(86), row_h, "浅色时刻:");
    make_edit(parent, hinst, ID_EDIT_LIGHT, mx + s(88), y, s(64), row_h);
    make_static(parent, hinst, 0, SS_LEFT | SS_CENTERIMAGE, mx + s(168), y, s(86), row_h, "深色时刻:");
    make_edit(parent, hinst, ID_EDIT_DARK, mx + s(254), y, s(64), row_h);
    make_button(parent, hinst, ID_BTN_SAVE_TIME, mx + s(344), y, s(68), row_h, "保存");
    y += s(34);

    // ---- 底部提示 ----
    make_static(parent, hinst, ID_LBL_HINT, SS_LEFT, mx, y, content_w, s(20), "");
    y += s(28);

    // ---- 按钮（两列）----
    let btn_w = s(186);
    let btn_h = s(36);
    let col2_x = mx + s(186) + s(18);
    make_button(parent, hinst, ID_BTN_TOGGLE, mx, y, btn_w, btn_h, "立即切换主题");
    make_button(parent, hinst, ID_BTN_CHECK, col2_x, y, btn_w, btn_h, "立即检查");
    y += s(46);
    make_button(parent, hinst, ID_BTN_CYCLE_MODE, mx, y, btn_w, btn_h, "切换模式");
    make_button(parent, hinst, ID_BTN_OPEN_CFG, col2_x, y, btn_w, btn_h, "打开配置文件");
    y += s(46);
    make_button(parent, hinst, ID_BTN_HIDE, mx, y, btn_w, btn_h, "隐藏到托盘");
    make_button(parent, hinst, ID_BTN_EXIT, col2_x, y, btn_w, btn_h, "退出程序");

    // 给大字体标签设字体
    let theme_lbl = GetDlgItem(parent, ID_LBL_THEME as i32);
    if theme_lbl != 0 && big_font != 0 {
        use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;
        use windows_sys::Win32::Foundation::WPARAM;
        SendMessageW(theme_lbl, WM_SETFONT, big_font as WPARAM, 1);
    }
    // 把 big_font 句柄存到窗口 GWLP_USERDATA，窗口销毁时 DeleteObject（避免字体泄露）
    if big_font != 0 {
        use windows_sys::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW;
        SetWindowLongPtrW(parent, raw::GWLP_USERDATA, big_font as isize);
    }
}

unsafe fn make_static(
    parent: HWND,
    hinst: HINSTANCE,
    id: u32,
    style: u32,
    x: i32,
    y: i32,
    cx: i32,
    cy: i32,
    text: &str,
) -> HWND {
    let t = w(text);
    let s = raw::WS_CHILD | raw::WS_VISIBLE | style;
    let h = CreateWindowExW(
        0,
        w("STATIC").as_ptr(),
        t.as_ptr(),
        s,
        x,
        y,
        cx,
        cy,
        parent,
        id as isize,
        hinst,
        ptr::null(),
    );
    apply_ui_font(parent, h);
    h
}

unsafe fn make_button(
    parent: HWND,
    hinst: HINSTANCE,
    id: u32,
    x: i32,
    y: i32,
    cx: i32,
    cy: i32,
    text: &str,
) -> HWND {
    let t = w(text);
    let s = raw::WS_CHILD | raw::WS_VISIBLE | raw::WS_TABSTOP | BS_PUSHBUTTON as u32;
    let h = CreateWindowExW(
        0,
        w("BUTTON").as_ptr(),
        t.as_ptr(),
        s,
        x,
        y,
        cx,
        cy,
        parent,
        id as isize,
        hinst,
        ptr::null(),
    );
    apply_ui_font(parent, h);
    h
}

unsafe fn make_checkbox(
    parent: HWND,
    hinst: HINSTANCE,
    id: u32,
    x: i32,
    y: i32,
    cx: i32,
    cy: i32,
    text: &str,
    checked: bool,
) -> HWND {
    const BS_AUTOCHECKBOX: u32 = 0x0003;
    const BM_SETCHECK: u32 = 0x00F1;
    const BST_CHECKED: usize = 1;
    let t = w(text);
    let s = raw::WS_CHILD | raw::WS_VISIBLE | raw::WS_TABSTOP | BS_AUTOCHECKBOX;
    let h = CreateWindowExW(
        0,
        w("BUTTON").as_ptr(),
        t.as_ptr(),
        s,
        x,
        y,
        cx,
        cy,
        parent,
        id as isize,
        hinst,
        ptr::null(),
    );
    apply_ui_font(parent, h);
    if h != 0 {
        use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;
        let w: usize = if checked { BST_CHECKED } else { 0 };
        SendMessageW(h, BM_SETCHECK, w, 0);
    }
    h
}

unsafe fn make_edit(
    parent: HWND,
    hinst: HINSTANCE,
    id: u32,
    x: i32,
    y: i32,
    cx: i32,
    cy: i32,
) -> HWND {
    let s = raw::WS_CHILD
        | raw::WS_VISIBLE
        | raw::WS_TABSTOP
        | ES_AUTOHSCROLL as u32
        | ES_NUMBER as u32;
    let h = CreateWindowExW(
        0, // 不用 WS_EX_CLIENTEDGE：避免深色模式下出现白色 3D 凹陷边框
        w("EDIT").as_ptr(),
        ptr::null(),
        s,
        x,
        y,
        cx,
        cy,
        parent,
        id as isize,
        hinst,
        ptr::null(),
    );
    apply_ui_font(parent, h);
    h
}

// ---- 刷新 UI 文本 ----

/// 刷新主窗口所有标签文本。
///
/// # Safety
/// hwnd 须有效。
pub unsafe fn refresh_main_window(hwnd: HWND) {
    let Some(app) = APP_STATE.get() else {
        return;
    };
    let Ok(st) = app.lock() else {
        return;
    };

    let cfg = st.cfg.clone();
    let mode = cfg.mode.clone();

    // 主题大标签
    let cur_theme = theme::get_theme().unwrap_or(Theme::Light);
    let theme_text = match cur_theme {
        Theme::Light => "浅色",
        Theme::Dark => "深色",
    };
    if let Some(h) = hwnd_opt(hwnd, ID_LBL_THEME) {
        let s = w(&format!("当前：{}", theme_text));
        SetWindowTextW(h, s.as_ptr());
    }

    // 大图标（尺寸随 DPI 缩放）
    if let Some(icon_h) = hwnd_opt(hwnd, ID_LBL_ICON) {
        let k = dpi_scale_for_window(hwnd);
        let hicon = icon::load_icon((48.0 * k).round() as i32);
        if hicon != 0 {
            use windows_sys::Win32::Foundation::WPARAM;
            use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;
            SendMessageW(icon_h, raw::STM_SETICON, hicon as WPARAM, 0);
        }
    }

    // 模式行
    if let Some(h) = hwnd_opt(hwnd, ID_LBL_MODE) {
        let line = match mode.as_str() {
            "sun" => match st.sun_cache {
                Some(s) => format!(
                    "模式：跟随日出日落    日出 {}    日落 {}",
                    fmt_hm(s.sunrise),
                    fmt_hm(s.sunset),
                ),
                None => "模式：跟随日出日落    尚未计算".to_string(),
            },
            "schedule" => format!(
                "模式：定时切换    浅色 {}    深色 {}",
                cfg.light_time, cfg.dark_time
            ),
            "off" => "模式：已暂停（仅手动）".to_string(),
            _ => format!("模式：未知（{}）", mode),
        };
        let s = w(&line);
        SetWindowTextW(h, s.as_ptr());
    }

    // 位置
    if let Some(h) = hwnd_opt(hwnd, ID_LBL_POS) {
        let line = match st.coords {
            Some((lat, lon)) => format!("位置：{:.4}, {:.4}    IP 定位", lat, lon),
            None if st.fetching_coords => "位置：正在后台获取...".to_string(),
            None => match (cfg.latitude, cfg.longitude) {
                (Some(_), Some(_)) => "位置：已配置（手动）".to_string(),
                _ => "位置：未确定".to_string(),
            },
        };
        let s = w(&line);
        SetWindowTextW(h, s.as_ptr());
    }

    // 下一动作
    if let Some(h) = hwnd_opt(hwnd, ID_LBL_NEXT) {
        let now = Local::now().naive_local().time();
        let line = match mode.as_str() {
            "sun" => match st.sun_cache {
                Some(s) => next_action_sun(now, &s),
                None => "下一动作：尚无今日日出日落数据".to_string(),
            },
            "schedule" => next_action_schedule(now, &cfg),
            "off" => "下一动作：手动".to_string(),
            _ => String::new(),
        };
        let s = w(&line);
        SetWindowTextW(h, s.as_ptr());
    }

    // 开机自启复选框：与配置同步
    if let Some(h) = hwnd_opt(hwnd, ID_CHK_AUTOSTART) {
        use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;
        const BM_SETCHECK: u32 = 0x00F1;
        let w: usize = if cfg.auto_start { 1 } else { 0 };
        SendMessageW(h, BM_SETCHECK, w, 0);
    }

    // 时间编辑框（仅 schedule 模式启用）
    let light_h = GetDlgItem(hwnd, ID_EDIT_LIGHT as i32);
    let dark_h = GetDlgItem(hwnd, ID_EDIT_DARK as i32);
    let save_h = GetDlgItem(hwnd, ID_BTN_SAVE_TIME as i32);
    let enable = mode == "schedule";    if light_h != 0 {
        send_message_enable(light_h, enable);
        let s = w(&cfg.light_time);
        SetWindowTextW(light_h, s.as_ptr());
    }
    if dark_h != 0 {
        send_message_enable(dark_h, enable);
        let s = w(&cfg.dark_time);
        SetWindowTextW(dark_h, s.as_ptr());
    }
    if save_h != 0 {
        send_message_enable(save_h, enable);
    }

    // 底部提示
    if let Some(h) = hwnd_opt(hwnd, ID_LBL_HINT) {
        let s = w(&format!(
            "每 {} 秒检查一次；右键系统托盘图标可快速切换",
            cfg.check_interval_secs
        ));
        SetWindowTextW(h, s.as_ptr());
    }
}

unsafe fn send_message_enable(h: HWND, enable: bool) {
    use windows_sys::Win32::Foundation::WPARAM;
    use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;
    const WM_ENABLE: u32 = 0x000A;
    SendMessageW(h, WM_ENABLE, enable as WPARAM, 0);
}

fn hwnd_opt(parent: HWND, id: u32) -> Option<HWND> {
    let h = unsafe { GetDlgItem(parent, id as i32) };
    if h == 0 {
        None
    } else {
        Some(h)
    }
}

fn fmt_hm(h: f64) -> String {
    let hh = h as u32;
    let mm = (((h - hh as f64) * 60.0) as u32) % 60;
    format!("{:02}:{:02}", hh, mm)
}

fn next_action_sun(now: NaiveTime, st: &sun::SunTimes) -> String {
    let nh = now.hour() as f64 + now.minute() as f64 / 60.0;
    if st.always_light {
        return "下一动作：极昼（一直浅色）".into();
    }
    if st.always_dark {
        return "下一动作：极夜（一直深色）".into();
    }
    let is_day = nh >= st.sunrise && nh < st.sunset;
    let (next_time, will_be_light) = if is_day {
        (st.sunset, false)
    } else {
        (st.sunrise, true)
    };
    let delta_h = next_time - nh;
    if delta_h < 0.0 {
        format!("下一动作：切换（今日 {} 已过）", fmt_hm(next_time))
    } else {
        let h = delta_h as u32;
        let m = (((delta_h - h as f64) * 60.0) as u32) % 60;
        format!(
            "{}：{:02}:{:02}（{} 小时 {} 分后）",
            if will_be_light {
                "下一次切到浅色"
            } else {
                "下一次切到深色"
            },
            next_time as u32,
            (((next_time - next_time as u32 as f64) * 60.0) as u32) % 60,
            h,
            m,
        )
    }
}

fn next_action_schedule(now: NaiveTime, cfg: &Config) -> String {
    let Ok(light) = NaiveTime::parse_from_str(&cfg.light_time, "%H:%M") else {
        return "下一动作：时间格式错误（light_time）".into();
    };
    let Ok(dark) = NaiveTime::parse_from_str(&cfg.dark_time, "%H:%M") else {
        return "下一动作：时间格式错误（dark_time）".into();
    };
    let desired = if light <= dark {
        if now >= light && now < dark {
            Theme::Light
        } else {
            Theme::Dark
        }
    } else if now >= light || now < dark {
        Theme::Light
    } else {
        Theme::Dark
    };
    let (label, time) = match desired {
        Theme::Light => ("下一次切到浅色", light),
        Theme::Dark => ("下一次切到深色", dark),
    };
    let now_s = now.hour() * 3600 + now.minute() * 60 + now.second();
    let tgt_s = time.hour() * 3600 + time.minute() * 60;
    let mut diff = tgt_s as i32 - now_s as i32;
    if diff < 0 {
        diff += 24 * 3600;
    }
    format!(
        "{}：{:02}:{:02}（{} 小时 {} 分后）",
        label,
        time.hour(),
        time.minute(),
        diff / 3600,
        (diff % 3600) / 60,
    )
}

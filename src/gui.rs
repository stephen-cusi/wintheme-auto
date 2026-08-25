// 主窗口 GUI：控件 ID、控件创建、状态刷新。
//
// 策略：所有 Win32 常量按原始 u32 处理，最后再包成 WINDOW_STYLE / WINDOW_EX_STYLE。
// windows-sys 0.52 的 NewType 没有实现 BitOr，所以位运算在 .0 上做。

#![allow(dead_code)]

use crate::config::Config;
use crate::icon;
use crate::sun;
use crate::theme::Theme;
use crate::APP_STATE;
use chrono::{Local, NaiveTime, Timelike};
use std::ptr;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, FONT_CHARSET, FONT_WEIGHT, FW_BOLD, GB2312_CHARSET, HFONT,
};
use windows_sys::Win32::Foundation::HINSTANCE;
use windows_sys::Win32::Graphics::Gdi::{CreateFontW, DeleteObject};
use windows_sys::Win32::System::SystemServices::{
    SS_CENTER, SS_ICON, SS_LEFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetDlgItem, GetWindowTextW, SetWindowTextW, BS_PUSHBUTTON, CW_USEDEFAULT,
    ES_AUTOHSCROLL, ES_NUMBER, WM_SETFONT,
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

pub const WIN_W: i32 = 420;
pub const WIN_H: i32 = 460;

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

/// 在已创建的主窗口 `parent` 上填充子控件。
///
/// # Safety
/// 必须在主窗口已创建、且窗口类已注册后调用；hinst 须有效。
pub unsafe fn populate_main_window(parent: HWND, hinst: HINSTANCE) {
    // 大字号粗体字体（"当前主题"标签用）
    let big_font: HFONT = CreateFontW(
        32,
        0,
        0,
        0,
        FW_BOLD,
        0,
        0,
        0,
        GB2312_CHARSET,
        0,
        0,
        0,
        0,
        w("Microsoft YaHei UI").as_ptr(),
    );

    // ---- 子控件布局（绝对坐标）----
    let btn_h: i32 = 32;
    let btn_w: i32 = 178;
    let x: i32 = 20;
    let mut y: i32 = 20;

    // 大图标（左上）
    make_static(parent, hinst, ID_LBL_ICON, SS_ICON, 20, 20, 56, 56, "");
    // 主题大字（右侧，紧贴图标）
    make_static(parent, hinst, ID_LBL_THEME, SS_LEFT, 88, 24, WIN_W - 88 - x, 48, "");
    y = 96;
    make_static(parent, hinst, ID_LBL_MODE, SS_LEFT, x, y, WIN_W - 2 * x, 22, "");
    y += 24;
    make_static(parent, hinst, ID_LBL_POS, SS_LEFT, x, y, WIN_W - 2 * x, 22, "");
    y += 24;
    make_static(parent, hinst, ID_LBL_NEXT, SS_LEFT, x, y, WIN_W - 2 * x, 22, "");
    y += 32;

    // 时间编辑区
    make_static(parent, hinst, 0, SS_LEFT, x, y, 110, 22, "浅色时刻 (HH:MM):");
    make_edit(parent, hinst, ID_EDIT_LIGHT, x + 120, y - 2, 70, 24);
    make_static(parent, hinst, 0, SS_LEFT, x + 200, y, 70, 22, "深色时刻:");
    make_edit(parent, hinst, ID_EDIT_DARK, x + 275, y - 2, 70, 24);
    make_button(parent, hinst, ID_BTN_SAVE_TIME, x + 350, y - 2, 50, 24, "保存");
    y += 36;

    // 底部提示
    make_static(parent, hinst, ID_LBL_HINT, SS_LEFT, x, y, WIN_W - 2 * x, 20, "");
    y += 28;

    // 按钮（两列）
    let col2_x = x + btn_w + 14;
    make_button(parent, hinst, ID_BTN_TOGGLE, x, y, btn_w, btn_h, "立即切换主题");
    make_button(parent, hinst, ID_BTN_CHECK, col2_x, y, btn_w, btn_h, "立即检查");
    y += btn_h + 10;
    make_button(parent, hinst, ID_BTN_CYCLE_MODE, x, y, btn_w, btn_h, "切换模式");
    make_button(parent, hinst, ID_BTN_OPEN_CFG, col2_x, y, btn_w, btn_h, "打开配置文件");
    y += btn_h + 10;
    make_button(parent, hinst, ID_BTN_HIDE, x, y, btn_w, btn_h, "隐藏到托盘");
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
    parent
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
    CreateWindowExW(
        0,
        w("STATIC").as_ptr(),
        t.as_ptr(),
        s,
        x,
        y,
        cx,
        cy,
        parent,
        id as *mut _,
        hinst,
        ptr::null(),
    )
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
    let s = 
        raw::WS_CHILD | raw::WS_VISIBLE | raw::WS_TABSTOP | BS_PUSHBUTTON as u32,
    ;
    CreateWindowExW(
        0,
        w("BUTTON").as_ptr(),
        t.as_ptr(),
        s,
        x,
        y,
        cx,
        cy,
        parent,
        id as *mut _,
        hinst,
        ptr::null(),
    )
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
    let s = 
        raw::WS_CHILD | raw::WS_VISIBLE | raw::WS_TABSTOP
            | ES_AUTOHSCROLL as u32
            | ES_NUMBER as u32,
    ;
    CreateWindowExW(
        raw::WS_EX_CLIENTEDGE,
        w("EDIT").as_ptr(),
        ptr::null(),
        s,
        x,
        y,
        cx,
        cy,
        parent,
        id as *mut _,
        hinst,
        ptr::null(),
    )
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
        let prefix = match mode.as_str() {
            "sun" => "跟随日出日落",
            "schedule" => "定时切换",
            "off" => "已暂停",
            _ => "未知",
        };
        let s = w(&format!("当前：{} · {}", theme_text, prefix));
        SetWindowTextW(h, s.as_ptr());
    }

    // 大图标
    if let Some(icon_h) = hwnd_opt(hwnd, ID_LBL_ICON) {
        let hicon = icon::load_icon(48);
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

    // 时间编辑框（仅 schedule 模式启用）
    let light_h = GetDlgItem(hwnd, ID_EDIT_LIGHT as i32);
    let dark_h = GetDlgItem(hwnd, ID_EDIT_DARK as i32);
    let save_h = GetDlgItem(hwnd, ID_BTN_SAVE_TIME as i32);
    let enable = mode == "schedule";
    if light_h != 0 {
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
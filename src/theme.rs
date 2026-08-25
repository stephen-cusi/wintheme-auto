use std::io;
use windows_sys::Win32::Foundation::LPARAM;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
};
use winreg::enums::*;
use winreg::RegKey;

const KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

/// 设置系统 + 应用 的浅色/深色主题，并广播 WM_SETTINGCHANGE 让 Explorer 立即生效。
pub fn set_theme(theme: Theme) -> io::Result<()> {
    let use_light: u32 = match theme {
        Theme::Light => 1,
        Theme::Dark => 0,
    };
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(KEY_PATH)?;
    key.set_value("AppsUseLightTheme", &use_light)?;
    key.set_value("SystemUsesLightTheme", &use_light)?;
    drop(key);

    // 通知系统重新加载颜色方案（ImmersiveColorSet）。
    let param: Vec<u16> = "ImmersiveColorSet\0".encode_utf16().collect();
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            param.as_ptr() as LPARAM,
            SMTO_ABORTIFHUNG,
            100,
            std::ptr::null_mut::<u32>(),
        );
    }
    Ok(())
}

/// 读取当前主题（依据 AppsUseLightTheme）。
pub fn get_theme() -> io::Result<Theme> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu.open_subkey(KEY_PATH)?;
    let apps: u32 = key.get_value("AppsUseLightTheme").unwrap_or(1);
    Ok(if apps == 1 { Theme::Light } else { Theme::Dark })
}

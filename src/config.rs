use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;
use winreg::enums::*;
use winreg::RegKey;

/// 程序配置（TOML）。所有字段都有默认值，缺省项会自动用默认值补齐。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 切换模式："sun"(跟随日出日落) / "schedule"(定时) / "off"(暂停，仅托盘手动控制)
    pub mode: String,
    /// 纬度（-90~90）。为 null 时自动通过 IP 获取。
    pub latitude: Option<f64>,
    /// 经度（-180~180）。为 null 时自动通过 IP 获取。
    pub longitude: Option<f64>,
    /// 定时模式：浅色时刻（24 小时制 "HH:MM"）
    pub light_time: String,
    /// 定时模式：深色时刻（24 小时制 "HH:MM"）
    pub dark_time: String,
    /// 检查间隔（秒），到点后判断是否该切换主题
    pub check_interval_secs: u64,
    /// 是否写入开机启动注册表
    pub auto_start: bool,
    /// 是否显示系统托盘图标与菜单
    pub tray: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: "sun".into(),
            latitude: None,
            longitude: None,
            light_time: "07:00".into(),
            dark_time: "19:00".into(),
            check_interval_secs: 60,
            auto_start: true,
            tray: true,
        }
    }
}

/// 配置所在目录：%APPDATA%\wintheme-auto
pub fn config_dir() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata).join("wintheme-auto")
    } else {
        PathBuf::from(".").join("wintheme-auto")
    }
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// 读取配置；若不存在则写入一份默认配置。
pub fn load() -> Result<Config> {
    let p = config_path();
    if p.exists() {
        let s = std::fs::read_to_string(&p)?;
        let cfg: Config = toml::from_str(&s)?;
        Ok(cfg)
    } else {
        let cfg = Config::default();
        save(&cfg)?;
        Ok(cfg)
    }
}

pub fn save(cfg: &Config) -> Result<()> {
    let p = config_path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, toml::to_string_pretty(cfg)?)?;
    Ok(())
}

/// 写入开机启动（当前用户登录时运行）。使用 HKCU\...\Run，无需管理员权限。
pub fn install_startup(exe_path: &str) -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run = hkcu.open_subkey_with_flags(
        r"Software\Microsoft\Windows\CurrentVersion\Run",
        KEY_WRITE,
    )?;
    // winreg 0.52 的 set_value 签名是 value: &T，&str 通过 ToRegValue 宏实现，
    // 因此字符串要传 &&str（如示例 &"www.example.com"）。
    run.set_value("WinThemeAuto", &exe_path)?;
    Ok(())
}

/// 移除开机启动注册表项。
pub fn uninstall_startup() -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(run) = hkcu.open_subkey_with_flags(
        r"Software\Microsoft\Windows\CurrentVersion\Run",
        KEY_WRITE,
    ) {
        let _ = run.delete_value("WinThemeAuto");
    }
    Ok(())
}

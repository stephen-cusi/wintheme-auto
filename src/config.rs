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
    /// 纬度（-90~90）。为 null 时自动通过系统位置 API / IP 获取。
    pub latitude: Option<f64>,
    /// 经度（-180~180）。为 null 时自动通过系统位置 API / IP 获取。
    pub longitude: Option<f64>,
    /// 定时模式：浅色时刻（24 小时制 "HH:MM"）
    pub light_time: String,
    /// 定时模式：深色时刻（24 小时制 "HH:MM"）
    pub dark_time: String,
    /// 检查间隔（秒），到点后判断是否该切换主题
    pub check_interval_secs: u64,
    /// 是否写入开机启动注册表
    pub auto_start: bool,
    /// 开机自启时是否静默启动（不弹主窗口，只在托盘里常驻）。
    /// 用户双击 exe 时不受此字段影响，总是显示主窗口。
    pub start_minimized: bool,
    /// 是否显示系统托盘图标与菜单
    pub tray: bool,
    /// 切深色主题时连带开启系统夜间模式（Night Light），切浅色时关闭
    pub night_light: bool,
    /// 切换主题时是否弹出 Windows 原生通知
    pub notifications: bool,
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
            // 默认开机自启时进托盘（避免每次登录都弹窗打扰）。
            // 用户需要开机就看到主窗口时，可在主窗口的「开机自启」下方关掉。
            start_minimized: true,
            tray: true,
            night_light: false,
            notifications: true,
        }
    }
}

/// 配置所在目录：`<exe 所在目录>\wintheme-auto\`
///
/// 放在 exe 旁边而不是 `%APPDATA%`，是为了让程序"绿色化"——把整个目录拷到
/// 任何地方都能直接跑（USB 盘、D:\portable\、测试 vm 等），日志/配置跟着走，
/// 不污染系统目录，也不会在多用户系统里和其他用户互相覆盖。
pub fn config_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("wintheme-auto");
        }
    }
    PathBuf::from(".").join("wintheme-auto")
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
///
/// `silent = true` 时在注册表项里加 `--silent` 参数，程序识别后会跳过主窗口
/// 创建，只在系统托盘里常驻（适合"我只想让它后台跑、不想每次登录都看到窗"）。
/// `silent = false` 时开机自启会弹主窗口（与用户手动双击行为一致）。
pub fn install_startup(exe_path: &str, silent: bool) -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run = hkcu.open_subkey_with_flags(
        r"Software\Microsoft\Windows\CurrentVersion\Run",
        KEY_WRITE,
    )?;
    // 路径加引号，兼容路径含空格。值整体要能正确解析：注册表 Run 项是命令行格式。
    let value = if silent {
        format!("\"{}\" --silent", exe_path)
    } else {
        format!("\"{}\"", exe_path)
    };
    run.set_value("WinThemeAuto", &value)?;
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

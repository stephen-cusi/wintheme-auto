# WinTheme Auto — Windows 11 浅色/深色主题自动切换器

[![build](https://github.com/stephen-cusi/wintheme-auto/actions/workflows/build.yml/badge.svg)](https://github.com/stephen-cusi/wintheme-auto/actions/workflows/build.yml)

用 Rust 编写，纯 Win32 API，**零额外运行时依赖**。跟随日出日落或定时切换 Windows 11 浅色/深色主题，
支持系统托盘常驻、登录自启。界面会**跟随当前主题自动切换深浅色**（标题栏、背景、文字、输入框、勾选框、自绘按钮）。

> 😮‍💨 本项目由 **vibe coding**（AI 辅助结对编程）驱动开发——Rust/Win32 细节密集、容易踩坑，
> 全部代码在对话式迭代中写出、编译、调优而成。详见文末 **Vibe Coding** 一栏。

## 功能

- **跟随日出日落**：通过 Windows 11 系统位置 API（`Windows.Devices.Geolocation`）获取经纬度，
  按当地日出/日落自动切换浅色/深色；也可在配置里写死经纬度（完全离线可用）。
- **定时切换**：在设定的两个时刻（如 07:00 浅色 / 19:00 深色）之间切换，支持跨午夜。
- **暂停模式**：`off` 模式下不动主题，仅保留托盘手动控制。
- **开机启动**：主界面「开机自动启动」勾选框，直接写/移除 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 注册表项（无需管理员）。
- **开机静默常驻**：「开机时只在托盘后台运行（不弹主窗口）」子选项，随开机自启开关联动显隐。
- **系统托盘**：右键菜单切换深浅色/模式、立即刷新位置、打开配置、退出（自绘大号菜单，勾选/悬停高亮）；
  左键单击反转当前主题；任务栏提示显示当前模式与主题。
- **深浅色自适应界面**：窗口背景/文字/输入框/勾选框/按钮随当前主题自动切换，标题栏同步变深/变浅，
  PerMonitorV2 高 DPI（如 200% 缩放）下不模糊。
- **关于对话框**：自绘主题化窗口，作者/仓库(可点击)链接/协议，含版本信息。
- **便携化**：配置与日志都写在 **exe 同目录**下的 `wintheme-auto\` 文件夹，拷走即用。

## 使用

双击 `wintheme-auto.exe` 即打开主窗口，同时常驻系统托盘。

- 主界面可直接勾选「开机自动启动」，无需命令行。
- 程序是**单实例**的：已有一个在运行时，再启动会弹出提示「已在运行，请勿重复打开」。
- 托盘没图标时，点任务栏右下角的 `^` 展开即可看到。
- 命令行参数仅保留自启动内部使用的 `--silent`（不弹主窗口、只在托盘跑），用户无需使用。

### 配置

配置与日志位于 exe 同目录的 `wintheme-auto\` 文件夹：

```
D:\path\to\wintheme-auto\wintheme-auto\config.toml
D:\path\to\wintheme-auto\wintheme-auto\wintheme-auto.log
```

### 配置示例

```toml
mode = "sun"            # sun=跟随日出日落 / schedule=定时 / off=暂停
latitude = null         # 留空则自动按系统位置获取；也可填固定值，如 31.23
longitude = null
light_time = "07:00"    # schedule 模式：浅色时刻
dark_time = "19:00"     # schedule 模式：深色时刻
check_interval_secs = 60
auto_start = true       # 是否写开机启动注册表
start_minimized = true  # 开机自启时静默进托盘，不弹主窗口
tray = true             # 是否显示托盘图标
```

修改配置后重启程序生效。

> 首次运行没有手动经纬度时，程序会通过 Windows 系统位置服务在后台获取；
> 若系统位置开关已关闭或设备无传感器，会在界面提示并给出「打开系统位置设置」按钮，
> **不会卡住程序**；也可以直接填死经纬度完全离线可用。

## 构建（Windows 11）

```powershell
cargo build --release
```

产物：`target\release\wintheme-auto.exe`。支持原生或 x64/ARM64 目标，直接双击运行即可。

> **关于构建脚本**：`build.rs` 会用 SDK 里的 `rc.exe` 把 `app.manifest`（PerMonitorV2 高 DPI、
> ComCtl32 v6 视觉样式）和图标嵌入 exe。若系统找不到 `rc.exe`（未装 Windows SDK），会
> 优雅降级：仍能编译运行，只是失去视觉样式/图标。

## 原理

- 主题开关写注册表 `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize` 的
  `AppsUseLightTheme` / `SystemUsesLightTheme`（1=浅色，0=深色），并广播 `WM_SETTINGCHANGE`
  （`ImmersiveColorSet`）让系统即时生效。
- 日出日落使用 NOAA「Almanac for Computers」算法，输入日期、经纬度、时区偏移，输出当地日出/日落时刻。
- 深色自适应通过 uxtheme 的 `SetPreferredAppMode(AllowDark)`（未公开 API，失败则静默保持现状）+
  `DwmSetWindowAttribute(DWMWA_USE_IMMERSIVE_DARK_MODE)` 让窗口/标题栏跟随主题。

## Vibe Coding

本项目全程采用 **vibe coding**（AI 结对编程）方式开发：以自然语言描述需求与 UI 效果，
由 AI 生成、迭代、修复 Rust/Win32 代码，作者负责在 Windows 上编译验证、提供截图与反馈，
最终打磨出可用的桌面应用。所有 UI 细节（深浅色适配、自绘控件、托盘菜单、关于窗口）均在此过程中收敛。

## 许可证

MIT © stephen-cusi

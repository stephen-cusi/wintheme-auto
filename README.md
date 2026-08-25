# WinTheme Auto — Windows 11 浅色/深色主题自动切换器

[![build](https://github.com/stephen-cusi/wintheme-auto/actions/workflows/build.yml/badge.svg)](https://github.com/stephen-cusi/wintheme-auto/actions/workflows/build.yml)

用 Rust 编写，纯 Win32 API。跟随日出日落或定时切换主题，支持托盘常驻、登录自启。
带一个轻量主窗口，界面会**跟随当前主题自动切换深浅色**（标题栏、背景、文字、输入框）。

## 功能

- **跟随日出日落（地理位置）**：自动通过公网 IP 获取经纬度，按当地日出/日落切换浅色/深色；也可在配置里写死经纬度（离线可用）。
- **定时切换**：在设定的两个时刻（如 07:00 浅色 / 19:00 深色）之间切换，支持跨午夜。
- **暂停模式**：`off` 模式下不动主题，仅保留托盘手动控制。
- **开机启动**：界面里「开机自动启动」勾选框，直接写/移除 `HKCU\...\Run` 注册表项（无需管理员）。
- **系统托盘**：右键菜单可切换深浅色、切换模式、立即检查、打开配置、退出；左键单击反转当前主题。任务栏提示显示当前模式与主题。
- **深浅色自适应界面**：窗口背景/文字/输入框随当前主题自动切换，标题栏也同步变深/变浅。

## 使用

点击 `wintheme-auto.exe`（或运行 `--install` 后登录自启）即打开主窗口，同时常驻系统托盘。

```powershell
# 首次安装（写开机启动 + 生成默认配置，随后常驻托盘并显示主窗口）
wintheme-auto.exe --install

# 立即切换
wintheme-auto.exe --light
wintheme-auto.exe --dark

# 查看当前主题
wintheme-auto.exe --status

# 终端调试模式：自建控制台，实时打印运行日志（适合看"它在跑"）
wintheme-auto.exe --console

# 取消开机启动
wintheme-auto.exe --uninstall
```

> **关于运行方式**：程序是 GUI 子系统，**双击不会弹出命令行黑窗**，直接显示主窗口；
> 命令行参数（`--status`/`--light` 等）在终端里使用时会输出结果。
> 程序是**单实例**的：已有一个在运行时，再启动会弹出提示「已在运行，请勿重复打开」。
> 托盘没图标时，点任务栏右下角的 `^` 展开即可看到。

配置位于：

```
%APPDATA%\wintheme-auto\config.toml
```

### 配置示例

```toml
mode = "sun"            # sun=跟随日出日落 / schedule=定时 / off=暂停
latitude = null         # 留空则自动按 IP 获取；也可填固定值，如 31.23
longitude = null
light_time = "07:00"    # schedule 模式：浅色时刻
dark_time = "19:00"     # schedule 模式：深色时刻
check_interval_secs = 60
auto_start = true       # 是否写开机启动注册表
tray = true             # 是否显示托盘图标
```

修改配置后重启程序生效。

> 首次运行没有手动经纬度时，会**在后台**通过 IP 自动获取位置，网络不通或超时（最多 5 秒）
> 会记录日志并稍后自动重试，**不会卡住程序**；也可以直接填死经纬度完全离线可用。

## 构建（ARM64 Windows 11）

本机是 ARM64，建议原生编译以获得最佳性能（也可编译 x64 版本在模拟层运行）。

1. 安装 Rust（rustup）。ARM64 Windows 默认宿主目标为 `aarch64-pc-windows-msvc`：
   ```powershell
   winget install Rustlang.Rustup
   ```
2. 安装 MSVC 链接所需的 **Visual Studio 2022 生成工具**，勾选：
   - **MSVC v143 - VS 2022 C++ ARM64 生成工具**（关键）
   - **Windows 11 SDK**
   - （若只想跑 x64 模拟版，则选「x64 生成工具」并把目标设为 `x86_64-pc-windows-msvc`）
3. 编译：
   ```powershell
   cargo build --release
   ```
   产物：`target\release\wintheme-auto.exe`
4. 安装并启动：
   ```powershell
   .\target\release\wintheme-auto.exe --install
   ```

> **关于构建脚本**：`build.rs` 会用 SDK 里的 `rc.exe` 把 `app.manifest`（PerMonitorV2 高 DPI、
> ComCtl32 v6 视觉样式）和图标嵌入 exe。若系统找不到 `rc.exe`（未装 Windows SDK），会
> 优雅降级：仍能编译运行，只是失去视觉样式/图标。若报 `linker 'link.exe' not found`，
> 说明第 2 步生成工具没装。

## 原理

- 主题开关写注册表 `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize` 的
  `AppsUseLightTheme` / `SystemUsesLightTheme`（1=浅色，0=深色），并广播 `WM_SETTINGCHANGE`
  （`ImmersiveColorSet`）让系统即时生效。
- 日出日落使用 NOAA「Almanac for Computers」算法，输入日期、经纬度、时区偏移，输出当地日出/日落时刻。
- 界面为 PerMonitorV2 DPI 感知，高分屏（如 200%）下不模糊。

## 日志

运行日志写入 `%APPDATA%\wintheme-auto\wintheme-auto.log`，可用于排查（如 IP 定位失败）。

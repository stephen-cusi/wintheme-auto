# WinTheme Auto — Windows 11 浅色/深色主题自动切换器

[![build](https://github.com/stephen-cusi/wintheme-auto/actions/workflows/build.yml/badge.svg)](https://github.com/stephen-cusi/wintheme-auto/actions/workflows/build.yml)

用 Rust 编写，纯 Win32 API，**零额外运行时依赖**。在 Windows 10/11 上跟随日出日落或定时切换浅色/深色主题，
支持系统托盘常驻、登录自启。界面会**跟随当前主题自动切换深浅色**（标题栏、背景、文字、输入框、勾选框、自绘按钮）。

> 😮‍💨 本项目由 **vibe coding**（AI 辅助结对编程）驱动开发——Rust/Win32 细节密集、容易踩坑，
> 全部代码在对话式迭代中写出、编译、调优而成。详见文末 **Vibe Coding** 一栏。

## 系统要求 / 兼容性

- **Windows 11**（推荐，Build 22000+）：标题栏深色用官方 `DWMWA_USE_IMMERSIVE_DARK_MODE=20`，全部特性完整可用。
- **Windows 10 1809–22H2**：可用。标题栏深色用同属性的旧值 `=19`（Win10 上 `20` 无效，代码已做 `20→19` 回退）；
  系统深色模式、定时/日出日落、托盘、开机自启、PerMonitorV2 高 DPI、系统位置 API（WinRT，Win10 1607+）都支持。
- **Windows 10 1703–1803**：可运行（PerMonitorV2 DPI 可用），但系统深色模式尚不完整，标题栏/主题切换效果打折。
- **Windows 10 < 1703**：不建议。`SetProcessDpiAwarenessContext` 用动态获取（缺失时交由 manifest 兜底），能启动，
  但无 PerMonitorV2、无系统深色模式。

> 定位依赖 **Windows 系统位置服务**（`Windows.Devices.Geolocation`，Win10 1607+），首次使用需在系统设置里
> 允许「位置」访问并打开定位；也可在配置里写死经纬度完全离线。

## 功能

- **跟随日出日落**：通过 Windows 系统位置 API（`Windows.Devices.Geolocation`，WinRT）获取经纬度，
  按当地日出/日落自动切换浅色/深色；也可在配置里写死经纬度（完全离线可用）。
- **定时切换**：在设定的两个时刻（如 07:00 浅色 / 19:00 深色）之间切换，支持跨午夜。
- **暂停模式**：`off` 模式下不动主题，仅保留托盘手动控制。
- **开机启动**：主界面「开机自动启动」勾选框，直接写/移除 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 注册表项（无需管理员）。
- **开机静默常驻**：「开机时只在托盘后台运行（不弹主窗口）」子选项，随开机自启开关联动显隐。
- **夜间模式联动**：勾选后，切换深色主题时自动开启系统 Night Light（护眼模式），切换浅色时自动关闭。通过直接操作 CloudStore 注册表二进制数据实现。
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

```text
D:\path\to\wintheme-auto\wintheme-auto\config.toml
D:\path\to\wintheme-auto\wintheme-auto\wintheme-auto.log
```

### 配置示例

```toml
mode = "sun"            # sun=跟随日出日落 / schedule=定时 / off=暂停
# latitude = 31.23      # 需要固定经纬度时，取消注释并填数值；留空(不写)则自动按系统位置获取
# longitude = 114.17
light_time = "07:00"    # schedule 模式：浅色时刻
dark_time = "19:00"     # schedule 模式：深色时刻
check_interval_secs = 60
auto_start = true       # 是否写开机启动注册表
start_minimized = true  # 开机自启时静默进托盘，不弹主窗口
tray = true             # 是否显示托盘图标
night_light = false     # 切深色时连带开启系统夜间模式（Night Light）
```
> 注意：TOML 没有 `null` 值。想"自动按系统位置获取"，直接把 `latitude`/`longitude` 两行**省略**即可（程序会取默认值并自动定位），不要写成 `latitude = null`——那样会导致配置解析失败。

修改配置后重启程序生效。

> 首次运行没有手动经纬度时，程序会通过 Windows 系统位置服务在后台获取；
> 若系统位置开关已关闭或设备无传感器，会在界面提示并给出「打开系统位置设置」按钮，
> **不会卡住程序**；也可以直接填死经纬度完全离线可用。

## 构建

依赖一套 MSVC 工具链即可（x64 和 ARM64 皆可，Windows 10 / 11 均可构建）：

1. **安装 Rust**（选择 `msvc` 工具链，默认 `x86_64-pc-windows-msvc`）：
   ```powershell
   winget install Rustlang.Rustup
   ```
2. **安装 Visual Studio 2022 生成工具**（Build Tools），「工作负载」勾选 **使用 C++ 的桌面开发**，
   它包含 `link.exe` 和 Windows SDK（SDK 里的 `rc.exe` 供 `build.rs` 嵌入 manifest/图标）。
   - 仅编译 x64 → 勾选 **MSVC v143 - VS 2022 C++ x64/x86 生成工具** 即可。
   - 想编译 **ARM64 原生版** → 额外勾选 **MSVC v143 - VS 2022 C++ ARM64 生成工具**，
     并把目标设为 `aarch64-pc-windows-msvc`（不装也能编译 x64 版，靠系统模拟运行）。
3. 编译：
   ```powershell
   cargo build --release
   ```
   产物：`target\release\wintheme-auto.exe`，双击即可运行。

> **常见报错排查**：
> - `linker 'link.exe' not found` → 第 2 步「使用 C++ 的桌面开发」没装或不完整。
> - 找不到 `rc.exe`（未装 Windows SDK）→ `build.rs` 会优雅降级：仍能编译运行，只是失去视觉样式/图标；
>   装上 SDK 后即可嵌入 `app.manifest`（PerMonitorV2 高 DPI、ComCtl32 v6）与图标。
> - 无需任何运行时依赖：产物是单个 exe，拷到别的机器即可运行。

## 原理

- 主题开关写注册表 `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize` 的
  `AppsUseLightTheme` / `SystemUsesLightTheme`（1=浅色，0=深色），并广播 `WM_SETTINGCHANGE`
  （`ImmersiveColorSet`）让系统即时生效。
- 日出日落使用 NOAA「Almanac for Computers」算法，输入日期、经纬度、时区偏移，输出当地日出/日落时刻。
- 深色自适应通过 uxtheme 的 `SetPreferredAppMode(AllowDark)`（未公开 API，失败则静默保持现状）+
  `DwmSetWindowAttribute(DWMWA_USE_IMMERSIVE_DARK_MODE)` 让窗口/标题栏跟随主题。
- 夜间模式通过直接读写 `HKCU\...\CloudStore\...\bluelightreductionstate` 注册表二进制数据（社区逆向格式），
  修改 byte 18（0x15=开/0x13=关）并插入/删除 bytes 23-24，自增 bytes 10-14 计数器让系统感知变更。

## Vibe Coding

本项目全程采用 **vibe coding**（AI 结对编程）方式开发：以自然语言描述需求与 UI 效果，
由 AI 生成、迭代、修复 Rust/Win32 代码，作者负责在 Windows 上编译验证、提供截图与反馈，
最终打磨出可用的桌面应用。所有 UI 细节（深浅色适配、自绘控件、托盘菜单、关于窗口）均在此过程中收敛。

## 许可证

MIT © stephen-cusi

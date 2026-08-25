# WinTheme Auto — Windows 11 浅色/深色主题自动切换器

[![build](https://github.com/stephen-cusi/wintheme-auto/actions/workflows/build.yml/badge.svg)](https://github.com/stephen-cusi/wintheme-auto/actions/workflows/build.yml)

用 Rust 编写，零运行时依赖（纯 Win32 API），支持两种切换方式，并可在登录时自动启动。

## 功能

- **跟随日出日落（地理位置）**：自动通过公网 IP 获取经纬度，按当地日出/日落时间切换浅色/深色；也可在配置里写死经纬度（离线可用）。
- **定时切换**：在设定的两个时刻（如 07:00 浅色 / 19:00 深色）之间切换，支持跨午夜。
- **暂停模式**：`off` 模式下不动主题，仅保留托盘手动控制。
- **开机启动**：写入 `HKCU\...\Run` 注册表项（无需管理员），登录即可运行。
- **系统托盘**：右键菜单可切换深浅色、切换模式、立即检查、打开配置、退出；左键单击反转当前主题。任务栏提示显示当前模式与主题。

## 使用

```powershell
# 首次安装（写开机启动 + 生成默认配置，随后常驻托盘）
wintheme-auto.exe --install

# 立即切换
wintheme-auto.exe --light
wintheme-auto.exe --dark

# 查看当前主题
wintheme-auto.exe --status

# 终端调试模式：不隐藏控制台，实时打印运行日志（明确看到"它在跑"）
wintheme-auto.exe --console

# 取消开机启动
wintheme-auto.exe --uninstall
```

> **关于"双击没反应"**：这是托盘常驻程序，正常启动后**没有窗口、没有输出**，只会在
> 系统托盘（任务栏右下角，可能需要点 ^ 展开）出现一个图标——这是正常的。
> 想确认它在跑：`wintheme-auto.exe --console` 从终端启动，会实时打印日志；
> 或看任务管理器里的 `wintheme-auto.exe` 进程。
> 程序是**单实例**的：已有一个在跑时，再启动新实例会直接退出。

直接双击运行（不带参数）即进入托盘常驻模式。配置位于：

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

修改配置后重启程序（托盘菜单「退出」再重新运行，或任务管理器结束进程）生效。

> 首次运行没有手动经纬度时，会**在后台**通过 IP 自动获取位置，网络不通或超时（最多 5 秒）
> 会记录日志并稍后自动重试，**不会卡住程序**；也可以直接填死经纬度（见上方配置）完全离线可用。

## 构建（ARM64 Windows 11）

本机是 ARM64，建议原生编译以获得最佳性能（也可编译 x64 版本在模拟层运行）。

1. 安装 Rust（rustup）。在 ARM64 Windows 上默认宿主目标为 `aarch64-pc-windows-msvc`：
   ```powershell
   winget install Rustlang.Rustup
   # 或运行官方 rustup-init.exe，选择默认（aarch64）工具链
   ```
2. 安装 MSVC 链接所需的 **Visual Studio 2022 生成工具**，勾选：
   - **MSVC v143 - VS 2022 C++ ARM64 生成工具**（关键）
   - **Windows 11 SDK**
   
   > 如果只想跑 x64（模拟）版本，则选「x64 生成工具」并把目标设为 `x86_64-pc-windows-msvc`。
3. 编译：
   ```powershell
   cargo build --release
   ```
   产物：`target\release\wintheme-auto.exe`
4. 安装并启动：
   ```powershell
   .\target\release\wintheme-auto.exe --install
   ```

> 构建报错 `linker 'link.exe' not found` 说明没装第 2 步的生成工具。

## 原理

- 主题开关写注册表 `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize` 的 `AppsUseLightTheme` / `SystemUsesLightTheme`（1=浅色，0=深色），并广播 `WM_SETTINGCHANGE`（`ImmersiveColorSet`）让系统即时生效。
- 日出日落使用 NOAA「Almanac for Computers」算法，输入日期、经纬度、时区偏移，输出当地日出/日落时刻。

## 日志

运行日志写入 `%APPDATA%\wintheme-auto\wintheme-auto.log`，可用于排查（如 IP 定位失败）。

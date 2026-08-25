use anyhow::{anyhow, Result};
use std::process::Command;
use std::time::Duration;

/// 获取当前经纬度（**仅**通过 Windows 系统位置 API，失败时直接返回错误）。
///
/// 之前是 "WinRT 位置 + IP 兜底"；现在按用户要求只走系统位置，因为：
/// 1. WinRT 位置对日出日落计算已足够精确（GPS + Wi-Fi 三角）
/// 2. 关闭 IP 后程序不会"悄悄"从公网 IP 库读你的家庭/公司大致地理位置
///
/// 失败原因（用于上层判定）：
/// - 系统设置里"位置"主开关关 → 1 秒内返回 E_DISABLED
/// - 用户拒绝权限 → GetGeopositionAsync 抛 UnauthorizedAccessException
/// - 设备无位置传感器 → 抛 E_NO_DATA
/// - PowerShell 未安装 / 不在 PATH → spawn 失败
/// - 整个流程超过 30 秒 → 主进程强杀子进程
pub fn fetch_location() -> Result<(f64, f64)> {
    log("正在获取位置：使用 win11 WinRT 位置 API…");
    let coords = fetch_location_system()?;
    log(&format!(
        "位置获取成功（win11 WinRT 位置 API）：{:.4}, {:.4}",
        coords.0, coords.1
    ));
    Ok(coords)
}

// ---------------------------------------------------------------------------
// 路径 1（仅剩这一条）: Win11 WinRT 位置 API
// ---------------------------------------------------------------------------

/// 通过 PowerShell 子进程调用 WinRT `Windows.Devices.Geolocation.Geolocator.GetGeopositionAsync`。
///
/// Win11 在首次调用时会弹出系统的"是否允许此应用访问位置位置"对话框。此后用户可以
/// 在「设置 → 隐私和安全性 → 位置」中更改授权。
///
/// 失败原因（用于回退判定）：
/// - 用户拒绝权限 → GetGeopositionAsync 抛 UnauthorizedAccessException
/// - 设备无 GPS / Wi-Fi → API 抛 E_NO_SUCH_DATA
/// - PowerShell 未安装 / 不在 PATH → spawn 失败
/// - 整个流程超过 `_SYSTEM_TIMEOUT` 秒 → 主动 terminate
fn fetch_location_system() -> Result<(f64, f64)> {
    let script = r#"
try {
    $ErrorActionPreference = 'Stop'
    Write-Output "STAGE:loading_runtime"
    Add-Type -AssemblyName System.Runtime.WindowsRuntime

    Write-Output "STAGE:loading_geolocator"
    [void][Windows.Devices.Geolocation.Geolocator,Windows.Devices.Geolocation,ContentType=WindowsRuntime]

    Write-Output "STAGE:locating_as_task"
    $asTaskGeneric = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object { $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' })[0]
    if (-not $asTaskGeneric) { throw "AS_TASK_NOT_FOUND" }

    Write-Output "STAGE:creating_geolocator"
    $loc = New-Object Windows.Devices.Geolocation.Geolocator
    $loc.DesiredAccuracy = [Windows.Devices.Geolocation.PositionAccuracy]::Default

    # 重要：直接调 GetGeopositionAsync。**不**先调 RequestAccessAsync ——
    # PowerShell 5.1 -STA 线程没有消息循环，RequestAccessAsync 触发的系统
    # Consent UI 弹不出来，会死锁。
    # 桌面应用想拿到位置，依赖「设置 -> 隐私和安全性 -> 位置 -> 让桌面应用访问位置」
    # 这个全局开关。UI 提供"打开系统位置设置"按钮引导用户。
    Write-Output "STAGE:calling_async"
    $op = $loc.GetGeopositionAsync()
    $task = $asTaskGeneric.MakeGenericMethod([Windows.Devices.Geolocation.Geoposition]).Invoke($null, @($op))

    Write-Output "STAGE:awaiting"
    $pos = $task.GetAwaiter().GetResult()
    $coord = $pos.Coordinate.Point.Position
    Write-Output ("RESULT:{0},{1}" -f $coord.Latitude, $coord.Longitude)
} catch {
    [Console]::Error.WriteLine(("CAUGHT:{0}" -f $_.Exception.Message))
    if ($_.Exception.InnerException) {
        [Console]::Error.WriteLine(("INNER:{0}" -f $_.Exception.InnerException.Message))
    }
    exit 1
}
"#;

    // PowerShell + GetGeopositionAsync 整套流程最多等 30 秒（系统权限对话框会让它卡住）。
    // 此处对子进程做一次总超时，避免无限制等待。
    //
    // CREATE_NO_WINDOW (0x08000000)：powershell.exe 本身是 console 子系统，
    // 不加这个标志会在我们（GUI subsystem）里临时弹一个 cmd 窗口闪烁一下。
    // 通过 std::os::windows::process::CommandExt::creation_flags 设置。
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut child = Command::new("powershell")
        .args([
            // 关键：-STA(Single-Threaded Apartment)。WinRT 必须在 STA 线程上调用，
            // PowerShell 默认 MTA 模式会导致 GetGeopositionAsync 无响应。
            "-STA",
            "-NoProfile",
            "-ExecutionPolicy", "Bypass",
            "-Command", script,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| anyhow!("无法启动 PowerShell: {e}（请确认 PowerShell 5+ 在 PATH 中）"))?;

    // 主线程限时：到时间还不退出就强杀（容忍权限弹窗卡住进程）。
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(30);
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(anyhow!("PowerShell 等待超时（{}秒）— 通常是用户未响应位置权限对话框", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => break Err(anyhow!("wait 子进程失败: {e}")),
        }
    }?;

    use std::io::Read as _;
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut err);
    }

    if !exit_status.success() {
        let stderr = err.trim();
        let stdout = out.trim();
        // 优先把脚本自抛的 "CAUGHT:..." 透出来，方便上层判断是否是权限问题
        let detail = if stderr.contains("CAUGHT") || stderr.contains("INNER") {
            stderr.to_string()
        } else if !stderr.is_empty() {
            stderr.to_string()
        } else if !stdout.is_empty() {
            stdout.to_string()
        } else {
            format!("退出状态: {:?}", exit_status.code())
        };
        log(&format!("系统位置 stderr: {}", detail));
        return Err(anyhow!("系统位置 API 调用失败: {detail}"));
    }

    let result_line = out
        .lines()
        .map(|l| l.trim())
        .find(|l| l.starts_with("RESULT:"))
        .map(|l| l.trim_start_matches("RESULT:").to_string());

    match result_line.as_deref() {
        Some(rest) if rest.starts_with("DISABLED:") => {
            // LocationStatus 不是 Ready（系统位置主开关关 / 设备无传感器 / 还没初始化）
            let status = rest.trim_start_matches("DISABLED:").to_string();
            Err(anyhow!("location_disabled:{}", status))
        }
        Some(rest) if rest.starts_with("DENIED:") => {
            // RequestAccessAsync 返回非 Allowed（用户拒绝或没响应）
            let status = rest.trim_start_matches("DENIED:").to_string();
            Err(anyhow!("location_denied:{}", status))
        }
        Some(coords) if coords.contains(',') => {
            let (lat, lon) = parse_lat_lon(coords)?;
            Ok((lat, lon))
        }
        _ => Err(anyhow!("系统位置 API 返回异常: {:?}", out.trim())),
    }
}

// ---------------------------------------------------------------------------
// 路径 2: 基于公网 IP 的近似定位（fallback）
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 解析与日志小工具
// ---------------------------------------------------------------------------

/// 把 "lat,lon" 字符串解析成 (f64, f64)。
fn parse_lat_lon(s: &str) -> Result<(f64, f64)> {
    let mut parts = s.split(',');
    let lat = parts
        .next()
        .ok_or_else(|| anyhow!("解析失败: \"{s}\""))?
        .trim()
        .parse::<f64>()
        .map_err(|_| anyhow!("lat 不是数字: \"{s}\""))?;
    let lon = parts
        .next()
        .ok_or_else(|| anyhow!("解析失败: \"{s}\""))?
        .trim()
        .parse::<f64>()
        .map_err(|_| anyhow!("lon 不是数字: \"{s}\""))?;
    Ok((lat, lon))
}

/// 共用的日志入口（与 main.rs 的 log 行为保持一致；本模块被 main.rs 单独调用时
/// 会通过 Rc<...> 的方式互不可见，故此处写一个独立的 logger，目标同样写到
/// `<exe 目录>\wintheme-auto\wintheme-auto.log`）。
fn log(msg: &str) {
    use std::io::Write;
    let dir = crate::config::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("wintheme-auto.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}

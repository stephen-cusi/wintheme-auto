use anyhow::{anyhow, Result};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::Command;
use std::time::Duration;

/// 获取当前位置（纬度, 经度）。
///
/// 优先调用 Windows 系统位置 API（Windows.Devices.Geolocation.Geolocator）。
/// Win11 会弹出"是否允许 wintheme-auto 访问你的位置"对话框，用户授权后
/// 走 GPS / Wi-Fi 三角定位。权限被拒绝或设备无定位能力时静默回退到
/// 基于公网 IP 的近似定位（ip-api.com），两条路径所得坐标都会在落盘前
/// 通过日志明示来源。
///
/// 该函数应在后台线程中调用，且需要时**必须设置超时**（PowerShell 进程和
/// GetGeopositionAsync 都会等待用户的权限对话框，最坏情况要阻塞数十秒）。
pub fn fetch_location() -> Result<(f64, f64)> {
    log_location_attempt("win11 WinRT 位置 API");
    match fetch_location_system() {
        Ok(coords) => {
            log_location_success("win11 WinRT 位置 API", coords);
            Ok(coords)
        }
        Err(e) => {
            log(&format!(
                "系统位置 API 不可用（{e}）— 可能是权限被拒绝、设备无位置传感器、\
                 或 PowerShell 启动失败"
            ));
            log_location_attempt("基于公网 IP 的近似定位（ip-api.com）");
            let coords = fetch_location_ip()?;
            log_location_success("基于公网 IP 的近似定位（ip-api.com）", coords);
            Ok(coords)
        }
    }
}

fn log_location_attempt(what: &str) {
    log(&format!("正在获取位置：使用 {what}…"));
}

fn log_location_success(what: &str, c: (f64, f64)) {
    log(&format!(
        "位置获取成功（{what}）：{:.4}, {:.4}",
        c.0, c.1
    ));
}

// ---------------------------------------------------------------------------
// 路径 1: Win11 WinRT 位置 API（通过 PowerShell 调 Windows.Devices.Geolocation）
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
    let mut child = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy", "Bypass",
            "-Command", script,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
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

    let line = out
        .lines()
        .map(|l| l.trim())
        .find(|l| l.starts_with("RESULT:"))
        .map(|l| l.trim_start_matches("RESULT:").to_string())
        .ok_or_else(|| anyhow!("系统位置 API 返回为空：完整输出 = {}", out.trim()))?;
    let (lat, lon) = parse_lat_lon(&line)?;
    Ok((lat, lon))
}

// ---------------------------------------------------------------------------
// 路径 2: 基于公网 IP 的近似定位（fallback）
// ---------------------------------------------------------------------------

/// 通过 ip-api.com（明文 HTTP，无需 TLS）获取当前公网 IP 对应的经纬度。
///
/// 注意：DNS 解析和 TCP 连接都限时（各 5 秒），宁可失败也不能长时间挂起。
fn fetch_location_ip() -> Result<(f64, f64)> {
    let host = "ip-api.com";
    let request = format!(
        "GET /json/ HTTP/1.1\r\nHost: {host}\r\nUser-Agent: wintheme-auto/0.1\r\nAccept: */*\r\nConnection: close\r\r\n"
    );
    let addrs = (host, 80)
        .to_socket_addrs()
        .map_err(|e| anyhow!("DNS 解析失败: {e}（请检查网络，或改用手动经纬度）"))?
        .collect::<Vec<_>>();
    let addr = addrs
        .first()
        .ok_or_else(|| anyhow!("无可用 IP 地址"))?;
    let mut stream = TcpStream::connect_timeout(addr, Duration::from_secs(5))
        .map_err(|e| anyhow!("无法连接 ip-api.com: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    stream.write_all(request.as_bytes())?;
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp)?;
    let body = String::from_utf8_lossy(&resp);
    let json = body.split("\r\n\r\n").last().unwrap_or("");
    let lat = extract_number(json, "\"lat\":")?;
    let lon = extract_number(json, "\"lon\":")?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(anyhow!("地理坐标超出合理范围"));
    }
    Ok((lat, lon))
}

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

fn extract_number(s: &str, key: &str) -> Result<f64> {
    let idx = s.find(key).ok_or_else(|| anyhow!("响应中未找到 {key}"))?;
    let rest = s[idx + key.len()..].trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(rest.len());
    let num_str = &rest[..end];
    num_str
        .trim()
        .parse::<f64>()
        .map_err(|e| anyhow!("解析数字失败 ({num_str}): {e}"))
}

/// 共用的日志入口（与 main.rs 的 log 行为保持一致；本模块被 main.rs 单独调用时
/// 会通过 Rc<...> 的方式互不可见，故此处写一个独立的 logger，目标同样写到
/// %APPDATA%\wintheme-auto\wintheme-auto.log）。
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

use anyhow::{anyhow, Result};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// 通过 ip-api.com（明文 HTTP，无需 TLS）获取当前公网 IP 对应的经纬度。
/// 返回 (纬度, 经度)。
pub fn fetch_location() -> Result<(f64, f64)> {
    let host = "ip-api.com";
    let request = format!(
        "GET /json/ HTTP/1.1\r\nHost: {host}\r\nUser-Agent: wintheme-auto/0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    let mut stream = TcpStream::connect((host, 80))
        .map_err(|e| anyhow!("无法连接 ip-api.com: {e}（请检查网络或使用固定经纬度配置）"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    stream.write_all(request.as_bytes())?;

    let mut resp = Vec::new();
    stream.read_to_end(&mut resp)?;
    let body = String::from_utf8_lossy(&resp);
    // 去掉 HTTP 头，只保留 JSON 主体
    let json = body.split("\r\n\r\n").last().unwrap_or("");

    let lat = extract_number(json, "\"lat\":")?;
    let lon = extract_number(json, "\"lon\":")?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(anyhow!("地理坐标超出合理范围"));
    }
    Ok((lat, lon))
}

/// 从 JSON 文本中提取某个键后的浮点数（例如 "lat": 31.23）。
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

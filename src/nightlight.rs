/// Windows 夜间模式（Night Light）控制。
///
/// 直接操作 CloudStore 注册表二进制数据：
/// - byte 18: 0x15 = 开启, 0x13 = 关闭
/// - 开启时总长 43 字节（bytes 23-24 = 0x10 0x00）
/// - 关闭时总长 41 字节
/// - bytes 10-14: 自增计数器，每次改动 +1

use anyhow::{anyhow, Result};
use winreg::enums::*;
use winreg::{RegKey, RegValue};

const REG_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\CloudStore\Store\DefaultAccount\Current\default$windows.data.bluelightreduction.bluelightreductionstate\windows.data.bluelightreduction.bluelightreductionstate";

fn open_key(writable: bool) -> Result<RegKey> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let flags = if writable { KEY_READ | KEY_WRITE } else { KEY_READ };
    hkcu.open_subkey_with_flags(REG_PATH, flags)
        .map_err(|e| anyhow!("打不开 Night Light 注册表项: {e}"))
}

pub fn is_enabled() -> Result<bool> {
    let key = open_key(false)?;
    let data = key.get_raw_value("Data")?;
    Ok(data.bytes.get(18) == Some(&0x15))
}

pub fn set_enabled(enable: bool) -> Result<()> {
    let key = open_key(true)?;
    let old = key.get_raw_value("Data")?.bytes;

    let currently = old.get(18) == Some(&0x15);
    if currently == enable {
        return Ok(());
    }

    let new_data: Vec<u8> = if currently {
        // 开 -> 关: 43 -> 41 字节
        if old.len() < 43 {
            return Err(anyhow!("注册表数据长度异常（{}），放弃修改", old.len()));
        }
        let mut d = vec![0u8; 41];
        d[0..22].copy_from_slice(&old[0..22]);
        d[23..41].copy_from_slice(&old[25..43]);
        d[18] = 0x13;
        d
    } else {
        // 关 -> 开: 41 -> 43 字节
        if old.len() < 41 {
            return Err(anyhow!("注册表数据长度异常（{}），放弃修改", old.len()));
        }
        let mut d = vec![0u8; 43];
        d[0..22].copy_from_slice(&old[0..22]);
        d[25..43].copy_from_slice(&old[23..41]);
        d[18] = 0x15;
        d[23] = 0x10;
        d[24] = 0x00;
        d
    };

    let mut final_data = new_data;
    // 自增 bytes 10-14 的计数器
    for i in 10..15 {
        if final_data[i] != 0xFF {
            final_data[i] += 1;
            break;
        }
    }

    key.set_raw_value(
        "Data",
        &RegValue {
            bytes: final_data,
            vtype: REG_BINARY,
        },
    )?;
    crate::log(&format!("夜间模式已{}", if enable { "开启" } else { "关闭" }));
    Ok(())
}

pub fn toggle() -> Result<bool> {
    let enabled = is_enabled()?;
    set_enabled(!enabled)?;
    Ok(!enabled)
}
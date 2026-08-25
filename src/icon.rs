// 自定义应用图标：解析嵌入的多尺寸 .ico，按需创建 HICON。
// .ico 由 assets/icon.ico 提供（用户提供的 PNG 经 Pillow 生成）。
//
// CreateIconFromResourceEx 既支持 BMP 帧也支持 PNG 帧（Vista+），
// 我们用 Pillow 默认输出的 BMP 帧，兼容性最好。

use std::sync::OnceLock;
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconFromResourceEx, HICON};

/// 嵌入的 .ico 文件（多个尺寸，包含 16/24/32/48/64/128/256）
static ICON_BYTES: &[u8] = include_bytes!("../assets/icon.ico");

#[derive(Clone, Copy)]
struct IcoImage {
    cx: i32,
    bytes: &'static [u8],
}

/// 解析 .ico，缓存全部可用尺寸（首次调用时一次性解析）
fn parse_ico() -> &'static [IcoImage] {
    static CACHE: OnceLock<Vec<IcoImage>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let b = ICON_BYTES;
            if b.len() < 6 {
                return vec![];
            }
            // ICONDIR: reserved(2)=0 type(2)=1 count(2)
            let count = u16::from_le_bytes([b[4], b[5]]) as usize;
            let mut out = Vec::with_capacity(count);
            for i in 0..count {
                let off = 6 + i * 16;
                if off + 16 > b.len() {
                    break;
                }
                let e = &b[off..off + 16];
                let raw_w = e[0] as i32;
                let raw_h = e[1] as i32;
                // width/height 为 0 时表示 256
                let cx = if raw_w == 0 { 256 } else { raw_w };
                let _ = cy_from(raw_w, raw_h);
                let bytes_in =
                    u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as usize;
                let img_off =
                    u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as usize;
                if img_off + bytes_in > b.len() {
                    continue;
                }
                out.push(IcoImage {
                    cx,
                    bytes: &b[img_off..img_off + bytes_in],
                });
            }
            out
        })
        .as_slice()
}

#[allow(dead_code)]
fn cy_from(_w: i32, _h: i32) -> i32 {
    // 保留备用：当前只用宽度挑图（我们的 .ico 都是方形）
    let _ = _w;
    _h
}

/// 加载指定像素尺寸的 HICON，找最接近的内嵌尺寸（方形）。
/// 返回的 HICON 不再使用时需 DestroyIcon 释放。
///
/// # Safety
/// 调用方负责 DestroyIcon。
pub unsafe fn load_icon(cx: i32) -> HICON {
    let images = parse_ico();
    if images.is_empty() {
        return 0;
    }
    let mut best: Option<&IcoImage> = None;
    let mut best_diff = i32::MAX;
    for img in images {
        // 优先匹配同等大小；同等大小里再选更小（绘制更锐利）
        let diff = (img.cx - cx).abs();
        if diff < best_diff {
            best_diff = diff;
            best = Some(img);
        }
    }
    let img = match best {
        Some(i) => i,
        None => return 0,
    };
    CreateIconFromResourceEx(
        img.bytes.as_ptr(),
        img.bytes.len() as u32,
        1,            // fIcon = TRUE
        0x00030000,   // dwVer
        cx,
        cx,
        0,
    )
}
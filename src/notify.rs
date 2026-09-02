/// 切换主题时的 Toast 通知（借用 Explorer AUMID + 自定义图标覆盖）

pub use win32_notif::ToastsNotifier;
use win32_notif::NotificationBuilder;
use win32_notif::notification::visual::{Image, Placement, Text};
use win32_notif::notification::visual::image::ImageCrop;
use win32_notif::notification::visual::text::HintStyle;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const TOAST_GROUP: &str = "wintheme-auto";
static SEQ: AtomicU32 = AtomicU32::new(1);

/// appLogoOverride 用的品牌图标（普通 PNG，和 icon.rs 里 HICON 用的
/// icon.ico 是两份不同文件——Toast 图片必须是磁盘上的真实文件路径）。
/// 请确保 assets/icon.png 存在，且清晰度足够(建议至少 256x256，
/// 直接从原始设计稿导出，不要从 .ico 里的小尺寸帧放大)。
static LOGO_PNG: &[u8] = include_bytes!("../assets/icon.png");

/// 把嵌入的 PNG 落盘一份，返回 file:// 路径；只在第一次调用时真正写文件
fn logo_file_uri() -> &'static str {
    static URI: OnceLock<String> = OnceLock::new();
    URI.get_or_init(|| {
        let dir = std::env::var("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        let dir = dir.join("WinThemeAuto");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("toast-logo.png");
        if std::fs::write(&path, LOGO_PNG).is_err() {
            crate::log("写入通知图标文件失败，appLogoOverride 将不可用");
        }
        format!("file:///{}", path.display().to_string().replace('\\', "/"))
    })
}

/// 每次通知生成一个唯一 tag：避免复用同一个 tag 时 Windows 把新通知当成
/// "替换"旧通知处理——这正是快速连续切换主题时通知"延迟"或被吞的原因。
fn unique_tag() -> String {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("theme-{}-{}", ts, n)
}

pub fn init() -> Option<ToastsNotifier> {
    match ToastsNotifier::new(Some("Microsoft.Windows.Explorer")) {
        Ok(n) => {
            crate::log("通知器初始化成功");
            Some(n)
        }
        Err(e) => {
            crate::log(&format!("通知器初始化失败: {:?}", e));
            None
        }
    }
}

fn build_and_show(n: &ToastsNotifier, title: &str, msg: &str) {
    let tag = unique_tag();
    match NotificationBuilder::new()
        .visual(
            Image::create(0, logo_file_uri())
                .with_placement(Placement::AppLogoOverride)
                .with_crop(ImageCrop::Circle),
        )
        // 注意：标题和正文各自独立一个 Text，直接挂在顶层，不要套
        // Group/SubGroup。Group+SubGroup 是并排分栏布局，不是"标题+正文"
        // 堆叠布局，套进去会导致标题被挤到很窄的一列里截断显示。
        .visual(Text::create(0, title).with_style(HintStyle::Title))
        .visual(Text::create(1, msg))
        .build(1, n, &tag, TOAST_GROUP)
    {
        Ok(toast) => {
            let _ = toast.show();
            crate::log("通知已发送");
        }
        Err(e) => {
            crate::log(&format!("通知失败: {:?}", e));
        }
    }
}

pub fn show_raw(n: &ToastsNotifier, title: &str, msg: &str) {
    build_and_show(n, title, msg);
}

pub fn show(notifier: &Option<ToastsNotifier>, title: &str, msg: &str) {
    let Some(n) = notifier else { return };
    build_and_show(n, title, msg);
}

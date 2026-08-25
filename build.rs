fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winres::WindowsResource::new();
        res.set_manifest_file("app.manifest");
        res.set_icon("assets/icon.ico");
        res.set("FileDescription", "WinTheme Auto");
        res.set("ProductName", "WinTheme Auto");
        // 若 rc.exe 缺失则优雅退化：不影响编译，仅失去视觉样式/高 DPI 声明。
        if res.compile().is_err() {
            println!("cargo:warning=未能嵌入 manifest（可能缺少 rc.exe），界面将退回经典样式。");
        }
    }
}

/// 取当前 git 提交的短 SHA（CI 里即本次构建的 commit），失败则回退 "dev"。
fn git_sha_short() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dev".into())
}

fn main() {
    // 把当前 git 提交注入编译环境，供「关于」对话框显示（CI 构建即本次提交 SHA）。
    println!("cargo:rustc-env=GIT_SHA={}", git_sha_short());

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

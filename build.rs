fn main() {
    slint_build::compile("UI/ui.slint").unwrap();
    // Windows 资源（图标/清单）只在 Windows 目标上编译；其他平台 skip。
    // `winresource` 的文档也要求调用方按 target_os 自行门控。
    // Compile Windows resources only for a Windows target; skip elsewhere (as `winresource`
    // documents: the caller must gate on the target OS).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icons/icon.ico");
        res.compile().unwrap();
    }
}

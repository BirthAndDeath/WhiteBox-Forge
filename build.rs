fn main() {
    slint_build::compile("UI/ui.slint").unwrap();
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icons/icon.ico");
    res.compile().unwrap();
}

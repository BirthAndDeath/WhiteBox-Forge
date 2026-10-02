//显示图标(原谅我的设计能力，我知道它不好看)
fn main() {
    slint_build::compile("UI/ui.slint").unwrap();
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icons/icon.ico");
    res.compile().unwrap();
}

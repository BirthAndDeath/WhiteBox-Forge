//显示图标(原谅我的设计能力，我知道它不好看)
fn main() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/ico.ico");
    res.compile().unwrap();
}
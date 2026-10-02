#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

//对于大多数可兼容操作，进行链式处理，一步步回退，尽可能软件上处理而不是宏分类
//实在无法回退，干脆没有api的，就使用宏分类对待

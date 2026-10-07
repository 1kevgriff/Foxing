#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[cfg(windows)]
mod folderpick;
#[cfg(windows)]
mod gdi;
#[cfg(windows)]
mod ids;
#[cfg(windows)]
mod listview;
#[cfg(windows)]
mod menuview;
#[cfg(windows)]
mod statusview;
#[cfg(windows)]
mod textview;
#[cfg(windows)]
mod uia;
#[cfg(windows)]
mod win;

#[cfg(windows)]
fn main() {
    win::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Foxing currently runs on Windows only.");
    std::process::exit(1);
}

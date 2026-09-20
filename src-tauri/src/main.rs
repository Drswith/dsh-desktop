// Windows release 下不弹一个多余的控制台窗口；这一行不能删。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    dsh_launcher_lib::run();
}

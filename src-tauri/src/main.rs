// The launcher has no console window to hide; the attribute keeps parity with
// Tauri's template for other platforms.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    dsh_launcher::run();
}

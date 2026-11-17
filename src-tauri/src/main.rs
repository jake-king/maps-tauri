//! The app's entry point; everything is in the library crate's `run`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    maps_tauri_lib::run()
}

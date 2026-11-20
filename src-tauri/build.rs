//! Build script: generates Tauri's context and the app's command permissions.

fn main() {
    // Declaring the app's commands makes each one a permission (allow-report, allow-search) that a
    // capability must grant. Without this list, every app command is callable by default.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "report",
            "search",
            "dev_stats",
            "dev_flags",
            "bench_done",
            "archive_info",
            "save_snapshot",
            "wx_enable",
            "wx_state",
            "wx_frame",
            "wx_satellite",
            "wx_metars",
            "wx_metar",
            "wx_wind",
            "wx_sigmets",
            "wx_satellite_global",
            "wx_prefetch",
            "update_check",
            "update_install",
            "set_window_size",
        ]),
    ))
    .expect("failed to run tauri-build");
}

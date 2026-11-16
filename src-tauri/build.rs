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
        ]),
    ))
    .expect("failed to run tauri-build");
}

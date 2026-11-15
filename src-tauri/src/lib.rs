/// Prints a status line from the frontend, so a run can be checked from the terminal.
#[tauri::command]
fn report(msg: String) {
    println!("[frontend] {msg}");
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![report])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

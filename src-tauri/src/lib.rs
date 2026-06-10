mod pty_manager;

use pty_manager::{
    attach_session, create_shell_session, detach_session, kill_session, list_sessions,
    resize_session, write_session, AppState,
};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            create_shell_session,
            list_sessions,
            attach_session,
            detach_session,
            write_session,
            resize_session,
            kill_session,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run AgentRelay");
}


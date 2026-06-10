mod pty_manager;

use pty_manager::{
    attach_session, continue_session, create_agent_session, create_shell_session, default_workspace,
    detach_session, forward_session, kill_session, list_agent_presets, list_sessions,
    resize_session, write_session, AppState,
};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            create_shell_session,
            create_agent_session,
            list_agent_presets,
            default_workspace,
            list_sessions,
            attach_session,
            detach_session,
            write_session,
            resize_session,
            kill_session,
            forward_session,
            continue_session,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run waypoint");
}

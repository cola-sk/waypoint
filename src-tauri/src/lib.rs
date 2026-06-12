mod pty_manager;

use pty_manager::{
    attach_session, continue_session, create_agent_session, default_workspace, delete_session,
    detach_session, forward_session, kill_session, list_agent_presets, list_chat_messages,
    list_sessions, reactivate_session, resize_session, write_session, AppState,
};

#[tauri::command]
fn select_directory() -> Option<String> {
    let dialog = rfd::FileDialog::new().pick_folder();
    dialog.map(|path| path.to_string_lossy().to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            create_agent_session,
            list_agent_presets,
            default_workspace,
            list_sessions,
            attach_session,
            reactivate_session,
            detach_session,
            write_session,
            resize_session,
            kill_session,
            delete_session,
            forward_session,
            continue_session,
            list_chat_messages,
            select_directory,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run waypoint");
}

mod pty_manager;

use std::process::Command;
use pty_manager::{
    attach_session, continue_session, create_agent_session, default_workspace, delete_session,
    detach_session, forward_session, kill_session, list_agent_presets, list_chat_messages,
    list_sessions, reactivate_session, resize_session, write_session, AppState,
};
use serde::Serialize;

#[tauri::command]
fn select_directory() -> Option<String> {
    let dialog = rfd::FileDialog::new().pick_folder();
    dialog.map(|path| path.to_string_lossy().to_string())
}

#[derive(Serialize)]
struct EditorInfo {
    id: String,
    name: String,
    bin: String,
}

/// Returns the list of supported editors that are currently installed.
#[tauri::command]
fn detect_editors() -> Vec<EditorInfo> {
    let candidates: &[(&str, &str, &str)] = &[
        ("antigravity", "Antigravity IDE", "antigravity"),
        ("vscode", "Visual Studio Code", "code"),
    ];

    candidates
        .iter()
        .filter_map(|(id, name, bin)| {
            let probe = Command::new("sh")
                .arg("-c")
                .arg(format!("command -v {bin}"))
                .output();
            if let Ok(out) = probe {
                if out.status.success() {
                    return Some(EditorInfo {
                        id: id.to_string(),
                        name: name.to_string(),
                        bin: bin.to_string(),
                    });
                }
            }
            None
        })
        .collect()
}

/// Opens `path` with the editor identified by `editor_bin`.
/// Returns the editor name on success, or an error string.
#[tauri::command]
fn open_in_editor(path: String, editor_bin: String) -> Result<(), String> {
    Command::new(&editor_bin)
        .arg(&path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Failed to launch {editor_bin}: {e}"))
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
            detect_editors,
            open_in_editor,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run waypoint");
}

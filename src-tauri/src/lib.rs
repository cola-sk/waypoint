mod pty_manager;

use pty_manager::{
    attach_session, continue_session, create_agent_session, create_handover_file,
    default_workspace, delete_session, delete_session_attachment, detach_session, forward_session,
    get_handover_draft, get_handover_preview, kill_session, list_agent_presets,
    list_chat_messages, list_session_attachments, list_sessions, reactivate_session,
    resize_session, save_session_attachment, write_session, AppState,
};
use serde::Serialize;
use std::process::Command;

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

struct EditorCandidate {
    id: &'static str,
    name: &'static str,
    bins: &'static [&'static str],
    macos_paths: &'static [&'static str],
}

/// Returns the list of supported editors that are currently installed.
#[tauri::command]
fn detect_editors() -> Vec<EditorInfo> {
    let candidates = &[
        EditorCandidate {
            id: "antigravity",
            name: "Antigravity IDE",
            bins: &["antigravity-ide", "antigravity"],
            macos_paths: &[
                "/Applications/Antigravity IDE.app/Contents/Resources/app/bin/antigravity-ide",
                "/Applications/Antigravity.app/Contents/Resources/app/bin/antigravity-ide",
            ],
        },
        EditorCandidate {
            id: "vscode",
            name: "Visual Studio Code",
            bins: &["code"],
            macos_paths: &["/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"],
        },
    ];

    candidates
        .iter()
        .filter_map(|cand| {
            // 1. Check in PATH
            for bin in cand.bins {
                let probe = Command::new("sh")
                    .arg("-c")
                    .arg(format!("command -v {bin}"))
                    .output();
                if let Ok(out) = probe {
                    if out.status.success() {
                        return Some(EditorInfo {
                            id: cand.id.to_string(),
                            name: cand.name.to_string(),
                            bin: bin.to_string(),
                        });
                    }
                }
            }

            // 2. Check macOS app package paths
            #[cfg(target_os = "macos")]
            {
                for path in cand.macos_paths {
                    if std::path::Path::new(path).exists() {
                        return Some(EditorInfo {
                            id: cand.id.to_string(),
                            name: cand.name.to_string(),
                            bin: path.to_string(),
                        });
                    }
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
            save_session_attachment,
            list_session_attachments,
            delete_session_attachment,
            resize_session,
            kill_session,
            delete_session,
            forward_session,
            continue_session,
            create_handover_file,
            get_handover_draft,
            get_handover_preview,
            list_chat_messages,
            select_directory,
            detect_editors,
            open_in_editor,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run waypoint");
}

mod pty_manager;

use base64::Engine;
use pty_manager::{
    attach_session, continue_session, create_agent_session, create_handover_file,
    default_workspace, delete_session, delete_session_attachment, detach_session, forward_session,
    get_handover_draft, get_handover_preview, kill_session, list_agent_presets, list_chat_messages,
    list_session_attachments, list_sessions, reactivate_session, resize_session,
    save_session_attachment, write_session, AppState,
};
use serde::Serialize;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_FILE_PREVIEW_BYTES: u64 = 8 * 1024 * 1024;

#[tauri::command]
fn select_directory() -> Option<String> {
    let dialog = rfd::FileDialog::new().pick_folder();
    dialog.map(|path| path.to_string_lossy().to_string())
}

#[tauri::command]
fn select_file() -> Option<String> {
    let dialog = rfd::FileDialog::new().pick_file();
    dialog.map(|path| path.to_string_lossy().to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FilePreview {
    path: String,
    name: String,
    extension: String,
    kind: String,
    mime: String,
    size_bytes: u64,
    modified_at: Option<u64>,
    content: String,
    data_url: Option<String>,
    truncated: bool,
}

#[tauri::command]
fn preview_file(path: String, base_dir: Option<String>) -> Result<FilePreview, String> {
    let resolved = resolve_preview_path(&path, base_dir.as_deref())?;
    let metadata = fs::metadata(&resolved)
        .map_err(|err| format!("无法读取文件信息：{} ({err})", resolved.display()))?;
    if !metadata.is_file() {
        return Err(format!("路径不是文件：{}", resolved.display()));
    }

    let extension = resolved
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());
    let common = FilePreviewCommon {
        path: resolved.to_string_lossy().to_string(),
        name: resolved
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string(),
        extension,
        size_bytes: metadata.len(),
        modified_at,
    };

    if let Some(mime) = image_mime_for_extension(&common.extension) {
        if metadata.len() > MAX_FILE_PREVIEW_BYTES {
            return Err(format!(
                "图片超过预览上限（{} MB）。",
                MAX_FILE_PREVIEW_BYTES / 1024 / 1024
            ));
        }
        let bytes = fs::read(&resolved)
            .map_err(|err| format!("无法读取图片：{} ({err})", resolved.display()))?;
        let data_url = format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
        return Ok(common.into_preview("image", mime, "", Some(data_url), false));
    }

    let mut file = fs::File::open(&resolved)
        .map_err(|err| format!("无法打开文件：{} ({err})", resolved.display()))?;
    let mut bytes = Vec::new();
    let max_with_sentinel = MAX_FILE_PREVIEW_BYTES.saturating_add(1);
    file.by_ref()
        .take(max_with_sentinel)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("无法读取文件：{} ({err})", resolved.display()))?;

    let truncated = bytes.len() as u64 > MAX_FILE_PREVIEW_BYTES;
    if truncated {
        bytes.truncate(MAX_FILE_PREVIEW_BYTES as usize);
    }
    if bytes.contains(&0) {
        return Err("该文件看起来是二进制内容，暂不支持预览。".to_string());
    }

    let content = match String::from_utf8(bytes) {
        Ok(value) => value,
        Err(err) => {
            let valid_up_to = err.utf8_error().valid_up_to();
            if truncated && valid_up_to > 0 && err.utf8_error().error_len().is_none() {
                let mut valid_bytes = err.into_bytes();
                valid_bytes.truncate(valid_up_to);
                String::from_utf8(valid_bytes)
                    .map_err(|_| "该文件不是有效的 UTF-8 文本，暂不支持预览。".to_string())?
            } else {
                return Err("该文件不是有效的 UTF-8 文本，暂不支持预览。".to_string());
            }
        }
    };

    Ok(common.into_preview(
        "text",
        "text/plain; charset=utf-8",
        content,
        None,
        truncated,
    ))
}

struct FilePreviewCommon {
    path: String,
    name: String,
    extension: String,
    size_bytes: u64,
    modified_at: Option<u64>,
}

impl FilePreviewCommon {
    fn into_preview(
        self,
        kind: &str,
        mime: &str,
        content: impl Into<String>,
        data_url: Option<String>,
        truncated: bool,
    ) -> FilePreview {
        FilePreview {
            path: self.path,
            name: self.name,
            extension: self.extension,
            kind: kind.to_string(),
            mime: mime.to_string(),
            size_bytes: self.size_bytes,
            modified_at: self.modified_at,
            content: content.into(),
            data_url,
            truncated,
        }
    }
}

fn image_mime_for_extension(extension: &str) -> Option<&'static str> {
    match extension {
        "apng" => Some("image/apng"),
        "avif" => Some("image/avif"),
        "bmp" => Some("image/bmp"),
        "gif" => Some("image/gif"),
        "ico" => Some("image/x-icon"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "svg" => Some("image/svg+xml"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn resolve_preview_path(path: &str, base_dir: Option<&str>) -> Result<PathBuf, String> {
    let trimmed = path
        .trim()
        .trim_matches(|ch| matches!(ch, '"' | '\'' | '`'));
    if trimmed.is_empty() {
        return Err("请输入文件路径。".to_string());
    }

    let candidate = expand_preview_path(trim_preview_line_suffix(trimmed), base_dir);
    match fs::canonicalize(&candidate) {
        Ok(path) => Ok(path),
        Err(_) => {
            let fallback = expand_preview_path(trimmed, base_dir);
            fs::canonicalize(&fallback)
                .map_err(|err| format!("无法解析文件路径：{} ({err})", fallback.display()))
        }
    }
}

fn trim_preview_line_suffix(value: &str) -> &str {
    let mut end = value.len();
    let bytes = value.as_bytes();
    let mut parts = 0;
    while end > 0 {
        let digit_end = end;
        while end > 0 && bytes[end - 1].is_ascii_digit() {
            end -= 1;
        }
        if end == digit_end || end == 0 || bytes[end - 1] != b':' {
            return value;
        }
        end -= 1;
        parts += 1;
        if parts == 2 {
            break;
        }
    }
    if parts == 0 {
        value
    } else {
        value[..end].trim_end()
    }
}

fn expand_preview_path(path: &str, base_dir: Option<&str>) -> PathBuf {
    let expanded = if path == "~" {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(path))
    } else if let Some(rest) = path.strip_prefix("~/") {
        std::env::var("HOME")
            .map(|home| PathBuf::from(home).join(rest))
            .unwrap_or_else(|_| PathBuf::from(path))
    } else {
        PathBuf::from(path)
    };

    if expanded.is_absolute() {
        return expanded;
    }

    base_dir
        .filter(|value| !value.trim().is_empty())
        .map(|value| Path::new(value).join(&expanded))
        .unwrap_or(expanded)
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
        .plugin(tauri_plugin_clipboard_manager::init())
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
            select_file,
            preview_file,
            detect_editors,
            open_in_editor,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run waypoint");
}

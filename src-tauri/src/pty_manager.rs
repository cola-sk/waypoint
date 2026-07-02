use std::{
    collections::{HashMap, HashSet},
    env, fs,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::Engine;
use parking_lot::Mutex;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

const RING_LIMIT_CHARS: usize = 200_000;
const INPUT_RING_LIMIT_CHARS: usize = 40_000;
const RENDER_RING_LIMIT_BYTES: usize = 400_000;
const PERSISTED_REPLAY_LIMIT_BYTES: usize = 1_500_000;
const CHAT_HISTORY_LIMIT: usize = 200;
const CHAT_MESSAGE_CONTENT_LIMIT_CHARS: usize = 120_000;
const CHAT_STREAM_IDLE_FINALIZE_MS: u64 = 1_200;
const HANDOVER_CONTEXT_CHARS: usize = 20_000;
const HANDOVER_USER_INPUT_CHARS: usize = 4_000;
const COMPACT_HANDOVER_CONTEXT_CHARS: usize = 4_000;
const COMPACT_USER_INPUT_CHARS: usize = 1_500;
const HANDOVER_INHERITED_CONTEXT_CHARS: usize = 12_000;
const COMPACT_HANDOVER_INHERITED_CONTEXT_CHARS: usize = 6_000;
const HANDOVER_INHERITED_STORE_CHARS: usize = 24_000;
const HANDOVER_LARGE_THRESHOLD_CHARS: usize = 24_000;
const HANDOVER_INJECT_ATTEMPTS: usize = 8;
const HANDOVER_INJECT_DELAY_MS: u64 = 350;
const CODEX_HANDOVER_STARTUP_DELAY_MS: u64 = 1_800;
const MAX_PTY_ROWS: u16 = 240;
const MAX_PTY_COLS: u16 = 600;
const MAX_ATTACHMENT_BYTES: usize = 15 * 1024 * 1024;
const MAX_ATTACHMENT_PREVIEW_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Default)]
pub struct AppState {
    manager: SessionManager,
}

#[derive(Default)]
struct SessionManager {
    sessions: Mutex<HashMap<String, Arc<PtySession>>>,
}

struct PtySession {
    meta: Mutex<SessionMeta>,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn Child + Send>>,
    transcript_path: PathBuf,
    ring: Mutex<String>,
    input_ring: Mutex<String>,
    render_ring: Mutex<Vec<u8>>,
    pending_user_input: Mutex<String>,
    inherited_handover: Mutex<String>,
    chat_messages: Mutex<Vec<ChatMessage>>,
    open_assistant_index: Mutex<Option<usize>>,
    last_assistant_output_at_ms: Mutex<Option<u64>>,
    deleted: Mutex<bool>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionMeta {
    id: String,
    agent_id: String,
    agent_name: String,
    title: String,
    command: String,
    cwd: String,
    status: SessionStatus,
    attached: bool,
    created_at: u64,
    last_active_at: u64,
    #[serde(default)]
    first_user_message: Option<String>,
    #[serde(default)]
    native_session_ref: Option<NativeSessionRef>,
    #[serde(default)]
    parent_session_id: Option<String>,
    #[serde(default)]
    handover_root_id: Option<String>,
    #[serde(default)]
    dangerous: bool,
    #[serde(default)]
    none_workspace: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSessionRef {
    provider: String,
    id: Option<String>,
    name: Option<String>,
    #[serde(default)]
    project: Option<String>,
    resume_command: Option<String>,
    discovered_at: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    id: String,
    agent_id: String,
    agent_name: String,
    title: String,
    command: String,
    cwd: String,
    status: SessionStatus,
    attached: bool,
    created_at: u64,
    last_active_at: u64,
    first_user_message: Option<String>,
    native_session_ref: Option<NativeSessionRef>,
    parent_session_id: Option<String>,
    handover_root_id: Option<String>,
    dangerous: bool,
    none_workspace: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SessionStatus {
    Running,
    Exited,
    Error,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    session: SessionInfo,
    replay: String,
    replay_base64: String,
    mode: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoverResult {
    prompt: String,
    source_session: SessionInfo,
    target_session: SessionInfo,
    mode: String,
    handover_mode: String,
    handover_path: Option<String>,
    evidence_path: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoverFileResult {
    prompt: String,
    source_session: SessionInfo,
    handover_mode: String,
    handover_path: String,
    evidence_path: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoverDraft {
    prompt: String,
    effective_mode: String,
    estimated_chars: usize,
    evidence_path: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAttachmentInfo {
    id: String,
    filename: String,
    path: String,
    mime: String,
    size_bytes: u64,
    created_at: u64,
    preview_data_url: Option<String>,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HandoverContentMode {
    Recommended,
    Compact,
    Full,
}

impl Default for HandoverContentMode {
    fn default() -> Self {
        Self::Recommended
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoverPreview {
    estimated_chars: usize,
    large_threshold_chars: usize,
    is_large: bool,
    recommended_mode: String,
    terminal_context_chars: usize,
    user_input_chars: usize,
    inherited_context_chars: usize,
}

#[derive(Clone, Copy, Serialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    id: String,
    role: ChatRole,
    content: String,
    pending: bool,
    created_at: u64,
    updated_at: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDiffSnapshot {
    branch: String,
    status: String,
    unstaged_diff: String,
    staged_diff: String,
    captured_at: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPresetInfo {
    id: String,
    name: String,
    description: String,
    available: bool,
    command: String,
    resolved_command: Option<String>,
}

#[derive(Clone)]
struct AgentDefinition {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    candidates: &'static [CommandCandidate],
}

#[derive(Clone)]
struct CommandCandidate {
    executable: &'static str,
    args: &'static [&'static str],
    display: &'static str,
    verify: VerifyStrategy,
}

#[derive(Clone, Copy)]
enum VerifyStrategy {
    CommandExists,
    ShellHelp(&'static str),
}

#[derive(Clone)]
struct ResolvedAgentCommand {
    executable: String,
    args: Vec<String>,
    display: String,
    resolved_display: String,
}

struct NativeResumeCommand {
    executable: String,
    args: Vec<String>,
    display_command: String,
    native_session_ref: Option<NativeSessionRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AgyResumeRef {
    conversation_id: String,
    project: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PtyDataEvent {
    session_id: String,
    data_base64: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionEvent {
    session: SessionInfo,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionErrorEvent {
    session_id: String,
    message: String,
}

#[tauri::command]
pub fn create_agent_session(
    state: State<'_, AppState>,
    app: AppHandle,
    agent_id: String,
    cwd: String,
    dangerous: Option<bool>,
    none_workspace: Option<bool>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<SessionInfo, String> {
    state.manager.create_agent_session(
        app,
        &agent_id,
        cwd,
        dangerous.unwrap_or(false),
        none_workspace.unwrap_or(false),
        rows,
        cols,
    )
}

#[tauri::command]
pub fn list_agent_presets() -> Vec<AgentPresetInfo> {
    agent_definitions()
        .into_iter()
        .map(|definition| {
            let resolved = resolve_agent_command(&definition);
            AgentPresetInfo {
                id: definition.id.to_string(),
                name: definition.name.to_string(),
                description: definition.description.to_string(),
                available: resolved.is_some(),
                command: definition
                    .candidates
                    .first()
                    .map(|candidate| candidate.display.to_string())
                    .unwrap_or_default(),
                resolved_command: resolved.map(|command| command.resolved_display),
            }
        })
        .collect()
}

#[tauri::command]
pub fn default_workspace() -> String {
    default_cwd()
}

#[tauri::command]
pub fn list_sessions(state: State<'_, AppState>) -> Vec<SessionInfo> {
    state.manager.list_sessions()
}

#[tauri::command]
pub fn attach_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<SessionSnapshot, String> {
    state.manager.attach_session(&session_id)
}

#[tauri::command]
pub fn reactivate_session(
    state: State<'_, AppState>,
    app: AppHandle,
    session_id: String,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<SessionInfo, String> {
    state
        .manager
        .reactivate_session(app, &session_id, rows, cols)
}

#[tauri::command]
pub fn detach_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    state.manager.detach_session(&session_id)
}

#[tauri::command]
pub fn write_session(
    state: State<'_, AppState>,
    session_id: String,
    data: String,
) -> Result<(), String> {
    state.manager.write_session(&session_id, data)
}

#[tauri::command]
pub fn save_session_attachment(
    state: State<'_, AppState>,
    session_id: String,
    mime: String,
    data_base64: String,
) -> Result<SessionAttachmentInfo, String> {
    state
        .manager
        .save_session_attachment(&session_id, &mime, &data_base64)
}

#[tauri::command]
pub fn list_session_attachments(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<SessionAttachmentInfo>, String> {
    state.manager.list_session_attachments(&session_id)
}

#[tauri::command]
pub fn delete_session_attachment(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> Result<(), String> {
    state.manager.delete_session_attachment(&session_id, &path)
}

#[tauri::command]
pub fn resize_session(
    state: State<'_, AppState>,
    session_id: String,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    state.manager.resize_session(&session_id, rows, cols)
}

#[tauri::command]
pub fn kill_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    state.manager.kill_session(&session_id)
}

#[tauri::command]
pub fn delete_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    state.manager.delete_session(&session_id)
}

#[tauri::command]
pub fn update_session_title(
    state: State<'_, AppState>,
    session_id: String,
    title: String,
) -> Result<SessionInfo, String> {
    state.manager.update_session_title(&session_id, &title)
}

#[tauri::command]
pub fn forward_session(
    state: State<'_, AppState>,
    source_session_id: String,
    target_session_id: String,
    note: Option<String>,
    handover_mode: Option<HandoverContentMode>,
    edited_prompt: Option<String>,
) -> Result<HandoverResult, String> {
    state.manager.forward_session(
        &source_session_id,
        &target_session_id,
        note,
        handover_mode.unwrap_or_default(),
        edited_prompt,
    )
}

#[tauri::command]
pub fn continue_session(
    state: State<'_, AppState>,
    app: AppHandle,
    source_session_id: String,
    target_agent_id: String,
    cwd: String,
    note: Option<String>,
    handover_mode: Option<HandoverContentMode>,
    edited_prompt: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<HandoverResult, String> {
    state.manager.continue_session(
        app,
        &source_session_id,
        &target_agent_id,
        cwd,
        note,
        handover_mode.unwrap_or_default(),
        edited_prompt,
        rows,
        cols,
    )
}

#[tauri::command]
pub fn get_handover_preview(
    state: State<'_, AppState>,
    source_session_id: String,
) -> Result<HandoverPreview, String> {
    state.manager.get_handover_preview(&source_session_id)
}

#[tauri::command]
pub fn get_handover_draft(
    state: State<'_, AppState>,
    source_session_id: String,
    target_mode: String,
    target_session_id: Option<String>,
    target_agent_id: Option<String>,
    cwd: Option<String>,
    note: Option<String>,
    handover_mode: Option<HandoverContentMode>,
) -> Result<HandoverDraft, String> {
    state.manager.get_handover_draft(
        &source_session_id,
        &target_mode,
        target_session_id.as_deref(),
        target_agent_id.as_deref(),
        cwd.as_deref(),
        note,
        handover_mode.unwrap_or_default(),
    )
}

#[tauri::command]
pub fn create_handover_file(
    state: State<'_, AppState>,
    source_session_id: String,
    note: Option<String>,
    handover_mode: Option<HandoverContentMode>,
    edited_prompt: Option<String>,
) -> Result<HandoverFileResult, String> {
    state.manager.create_handover_file(
        &source_session_id,
        note,
        handover_mode.unwrap_or_default(),
        edited_prompt,
    )
}

#[tauri::command]
pub fn send_chat_message(
    state: State<'_, AppState>,
    session_id: String,
    message: String,
) -> Result<(), String> {
    state.manager.send_chat_message(&session_id, &message)
}

#[tauri::command]
pub fn list_chat_messages(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<ChatMessage>, String> {
    state.manager.list_chat_messages(&session_id)
}

#[tauri::command]
pub fn get_session_diff_snapshot(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<SessionDiffSnapshot, String> {
    state.manager.get_session_diff_snapshot(&session_id)
}

impl SessionManager {
    fn create_agent_session(
        &self,
        app: AppHandle,
        agent_id: &str,
        cwd: String,
        dangerous: bool,
        none_workspace: bool,
        rows: Option<u16>,
        cols: Option<u16>,
    ) -> Result<SessionInfo, String> {
        let definition = agent_definitions()
            .into_iter()
            .find(|definition| definition.id == agent_id)
            .ok_or_else(|| format!("unknown agent preset: {agent_id}"))?;
        let resolved = resolve_agent_command(&definition).ok_or_else(|| {
            format!(
                "{} is not available in PATH. Install it or make sure your login shell can resolve it.",
                definition.name
            )
        })?;
        let mut args = resolved.args;
        if dangerous {
            apply_dangerous_flag(agent_id, &mut args);
        }
        self.spawn_session(
            app,
            definition.id,
            definition.name,
            definition.name.to_string(),
            resolved.display,
            resolved.executable,
            args,
            cwd,
            rows,
            cols,
            Vec::new(),
            dangerous,
            none_workspace,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_session(
        &self,
        app: AppHandle,
        agent_id: &str,
        agent_name: &str,
        title: String,
        display_command: String,
        executable: String,
        args: Vec<String>,
        cwd: String,
        rows: Option<u16>,
        cols: Option<u16>,
        extra_env: Vec<(String, String)>,
        dangerous: bool,
        none_workspace: bool,
    ) -> Result<SessionInfo, String> {
        self.spawn_session_with_identity(
            app,
            agent_id,
            agent_name,
            title,
            display_command,
            executable,
            args,
            cwd,
            rows,
            cols,
            extra_env,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            dangerous,
            none_workspace,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_session_with_identity(
        &self,
        app: AppHandle,
        agent_id: &str,
        agent_name: &str,
        title: String,
        display_command: String,
        executable: String,
        mut args: Vec<String>,
        cwd: String,
        rows: Option<u16>,
        cols: Option<u16>,
        extra_env: Vec<(String, String)>,
        existing_id: Option<String>,
        title_override: Option<String>,
        created_at_override: Option<u64>,
        mut native_session_ref: Option<NativeSessionRef>,
        first_user_message: Option<String>,
        parent_session_id: Option<String>,
        handover_root_id: Option<String>,
        dangerous: bool,
        none_workspace: bool,
    ) -> Result<SessionInfo, String> {
        let cwd_path = PathBuf::from(&cwd);
        if !cwd_path.is_dir() {
            return Err(format!("workspace directory does not exist: {cwd}"));
        }
        let cwd_path = fs::canonicalize(&cwd_path).unwrap_or(cwd_path);
        let normalized_cwd = cwd_path.to_string_lossy().to_string();

        let id = existing_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let now = unix_timestamp();
        let session_title =
            title_override.unwrap_or_else(|| format!("{title} {}", &id[..id.len().min(8)]));
        let session_dir = session_dir(&id)?;
        fs::create_dir_all(&session_dir).map_err(|err| {
            format!(
                "failed to create session directory {}: {err}",
                session_dir.display()
            )
        })?;
        let transcript_path = session_dir.join("transcript.log");

        if agent_id == "copilot"
            && native_session_ref.is_none()
            && !copilot_args_have_session_identity(&args)
        {
            append_copilot_cli_option(&mut args, format!("--session-id={id}"));
            native_session_ref = Some(NativeSessionRef {
                provider: agent_id.to_string(),
                id: Some(id.clone()),
                name: None,
                project: None,
                resume_command: Some(format!("{} --resume={}", display_command, shell_quote(&id))),
                discovered_at: now,
            });
        }

        if agent_id == "claude-code"
            && native_session_ref.is_none()
            && !claude_args_have_session_identity(&args)
        {
            append_claude_session_id(&mut args, &id);
            native_session_ref = Some(NativeSessionRef {
                provider: agent_id.to_string(),
                id: Some(id.clone()),
                name: None,
                project: None,
                resume_command: Some(format!("{} --resume={}", display_command, shell_quote(&id))),
                discovered_at: now,
            });
        }

        let pty_system = native_pty_system();
        let initial_rows = rows.unwrap_or(30);
        let initial_cols = cols.unwrap_or(100);
        let initial_rows = if initial_rows < 5 {
            30
        } else {
            initial_rows.min(MAX_PTY_ROWS)
        };
        let initial_cols = if initial_cols < 10 {
            100
        } else {
            initial_cols.min(MAX_PTY_COLS)
        };
        let pair = pty_system
            .openpty(PtySize {
                rows: initial_rows,
                cols: initial_cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| format!("failed to open PTY: {err}"))?;

        let mut cmd = CommandBuilder::new(&executable);
        for arg in &args {
            cmd.arg(arg);
        }
        // portable-pty clears the inherited env on spawn, so we must
        // re-inject the user's login-shell env (PATH, HOME, etc.) or
        // agent-spawned MCP servers (npx/uvx/node) won't be found.
        for (key, value) in login_shell_env() {
            cmd.env(key, value);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("WT_SESSION", "waypoint");
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        cmd.cwd(cwd_path);

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|err| format!("failed to spawn {display_command}: {err}"))?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| format!("failed to clone PTY reader: {err}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|err| format!("failed to take PTY writer: {err}"))?;

        let meta = SessionMeta {
            id: id.clone(),
            agent_id: agent_id.to_string(),
            agent_name: agent_name.to_string(),
            title: session_title,
            command: display_command,
            cwd: normalized_cwd,
            status: SessionStatus::Running,
            attached: false,
            created_at: created_at_override.unwrap_or(now),
            last_active_at: now,
            first_user_message,
            native_session_ref,
            parent_session_id,
            handover_root_id,
            dangerous,
            none_workspace,
        };

        let session = Arc::new(PtySession {
            meta: Mutex::new(meta),
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
            transcript_path,
            ring: Mutex::new(String::new()),
            input_ring: Mutex::new(String::new()),
            render_ring: Mutex::new(Vec::new()),
            pending_user_input: Mutex::new(String::new()),
            inherited_handover: Mutex::new(String::new()),
            chat_messages: Mutex::new(Vec::new()),
            open_assistant_index: Mutex::new(None),
            last_assistant_output_at_ms: Mutex::new(None),
            deleted: Mutex::new(false),
        });

        session.persist_meta();
        self.sessions.lock().insert(id.clone(), session.clone());

        let reader_session = session.clone();
        let reader_id = id.clone();
        let reader_app = app.clone();
        thread::spawn(move || {
            let mut buf = [0_u8; 8192];
            let mut transcript = open_transcript_append(&reader_session.transcript_path).ok();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        reader_session.finalize_open_assistant_message();
                        reader_session.mark_status(SessionStatus::Exited);
                        cache_agy_resume_ref_on_session(&reader_session);
                        let _ = reader_app.emit(
                            "session:exited",
                            SessionEvent {
                                session: reader_session.info(),
                            },
                        );
                        break;
                    }
                    Ok(n) => {
                        let data = String::from_utf8_lossy(&buf[..n]).to_string();
                        let chat_chunk = clean_chat_chunk(&data);
                        let replace_chat = has_chat_repaint_hint(&data);
                        let encoded = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
                        if let Some(file) = transcript.as_mut() {
                            let _ = file.write_all(&buf[..n]);
                        }
                        reader_session.append_ring(&data);
                        reader_session.append_render(&buf[..n]);
                        reader_session.append_chat_assistant_output(&chat_chunk, replace_chat);
                        let _ = reader_app.emit(
                            "pty:data",
                            PtyDataEvent {
                                session_id: reader_id.clone(),
                                data_base64: encoded,
                            },
                        );
                    }
                    Err(err) if err.raw_os_error() == Some(5) => {
                        reader_session.finalize_open_assistant_message();
                        reader_session.mark_status(SessionStatus::Exited);
                        cache_agy_resume_ref_on_session(&reader_session);
                        let _ = reader_app.emit(
                            "session:exited",
                            SessionEvent {
                                session: reader_session.info(),
                            },
                        );
                        break;
                    }
                    Err(err) => {
                        reader_session.finalize_open_assistant_message();
                        reader_session.mark_status(SessionStatus::Error);
                        cache_agy_resume_ref_on_session(&reader_session);
                        let _ = reader_app.emit(
                            "session:error",
                            SessionErrorEvent {
                                session_id: reader_id.clone(),
                                message: err.to_string(),
                            },
                        );
                        break;
                    }
                }
            }
        });

        let info = session.info();
        let _ = app.emit(
            "session:created",
            SessionEvent {
                session: info.clone(),
            },
        );

        Ok(info)
    }

    fn list_sessions(&self) -> Vec<SessionInfo> {
        let live_sessions = self.sessions.lock();
        let mut sessions = live_sessions
            .values()
            .map(|session| session.info())
            .collect::<Vec<_>>();
        let live_ids = live_sessions.keys().cloned().collect::<Vec<_>>();
        drop(live_sessions);

        for mut meta in load_all_session_metas() {
            if live_ids.iter().any(|id| id == &meta.id) {
                continue;
            }
            meta.attached = false;
            if matches!(meta.status, SessionStatus::Running) {
                meta.status = SessionStatus::Exited;
            }
            sessions.push(meta.to_info());
        }

        sessions.sort_by_key(|session| session.created_at);
        sessions
    }

    fn attach_session(&self, session_id: &str) -> Result<SessionSnapshot, String> {
        if let Some(session) = self.sessions.lock().get(session_id).cloned() {
            {
                let mut meta = session.meta.lock();
                meta.attached = true;
                meta.last_active_at = unix_timestamp();
            }
            session.persist_meta();
            let replay = session.ring.lock().clone();
            let replay_bytes = session.render_ring.lock().clone();
            let info = session.info();
            let mode = if matches!(info.status, SessionStatus::Running) {
                "live"
            } else {
                "replay-only"
            };
            return Ok(SessionSnapshot {
                session: info,
                replay,
                replay_base64: base64::engine::general_purpose::STANDARD.encode(replay_bytes),
                mode: mode.to_string(),
            });
        }

        let mut meta = load_session_meta(session_id)?;
        meta.attached = false;
        if matches!(meta.status, SessionStatus::Running) {
            meta.status = SessionStatus::Exited;
        }
        let replay_bytes = read_persisted_replay(session_id)?;
        let replay = String::from_utf8_lossy(&replay_bytes).to_string();
        Ok(SessionSnapshot {
            session: meta.to_info(),
            replay,
            replay_base64: base64::engine::general_purpose::STANDARD.encode(replay_bytes),
            mode: "replay-only".to_string(),
        })
    }

    fn detach_session(&self, session_id: &str) -> Result<(), String> {
        let Some(session) = self.sessions.lock().get(session_id).cloned() else {
            return Ok(());
        };
        let mut meta = session.meta.lock();
        meta.attached = false;
        meta.last_active_at = unix_timestamp();
        drop(meta);
        session.persist_meta();
        Ok(())
    }

    fn reactivate_session(
        &self,
        app: AppHandle,
        session_id: &str,
        rows: Option<u16>,
        cols: Option<u16>,
    ) -> Result<SessionInfo, String> {
        if let Some(session) = self.sessions.lock().get(session_id).cloned() {
            let info = session.info();
            if matches!(info.status, SessionStatus::Running) {
                return Ok(info);
            }
        }

        let mut meta = load_session_meta(session_id)?;
        if !PathBuf::from(&meta.cwd).is_dir() {
            return Err(format!("workspace directory does not exist: {}", meta.cwd));
        }
        if cache_agy_resume_ref_in_meta(&mut meta) {
            persist_session_meta(&meta)?;
        }

        let command = native_resume_command_for(&meta)?.ok_or_else(|| {
            format!(
                "{} does not have a saved native resume id. Open a new session and use the agent's resume command manually.",
                meta.agent_name
            )
        })?;

        self.spawn_session_with_identity(
            app,
            &meta.agent_id,
            &meta.agent_name,
            meta.agent_name.clone(),
            command.display_command,
            command.executable,
            command.args,
            meta.cwd.clone(),
            rows,
            cols,
            Vec::new(),
            Some(meta.id.clone()),
            Some(meta.title.clone()),
            Some(meta.created_at),
            command.native_session_ref,
            meta.first_user_message.clone(),
            meta.parent_session_id.clone(),
            meta.handover_root_id.clone(),
            meta.dangerous,
            meta.none_workspace,
        )
    }

    fn write_session(&self, session_id: &str, data: String) -> Result<(), String> {
        let session = self.get(session_id)?;
        if !matches!(session.info().status, SessionStatus::Running) {
            return Err("session is not running".to_string());
        }
        let data_to_write = if should_append_agy_session_marker(&session, &data) {
            format!("{data}<!-- waypoint_session_id: {session_id} -->\r")
        } else {
            data.clone()
        };
        session.append_input(&data);
        session.capture_user_input(&data);
        session
            .writer
            .lock()
            .write_all(data_to_write.as_bytes())
            .map_err(|err| format!("failed to write to PTY: {err}"))?;
        session.meta.lock().last_active_at = unix_timestamp();
        session.persist_meta();
        Ok(())
    }

    fn save_session_attachment(
        &self,
        session_id: &str,
        mime: &str,
        data_base64: &str,
    ) -> Result<SessionAttachmentInfo, String> {
        let info = self.session_info_for_storage(session_id)?;
        let (normalized_mime, extension) = normalize_attachment_mime(mime)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data_base64)
            .map_err(|err| format!("failed to decode attachment: {err}"))?;
        if bytes.is_empty() {
            return Err("attachment is empty".to_string());
        }
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(format!(
                "attachment is too large: max {} MB",
                MAX_ATTACHMENT_BYTES / 1024 / 1024
            ));
        }

        let dir = session_attachment_dir(&info.cwd, session_id)?;
        ensure_session_attachment_dir(&dir)?;
        let suffix = Uuid::new_v4().to_string();
        let filename = format!(
            "screenshot-{}-{}.{}",
            unix_timestamp_ms(),
            &suffix[..8],
            extension
        );
        let path = dir.join(filename);
        fs::write(&path, &bytes)
            .map_err(|err| format!("failed to write attachment {}: {err}", path.display()))?;

        attachment_info_from_path(&path, Some(normalized_mime), true)
    }

    fn list_session_attachments(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionAttachmentInfo>, String> {
        let info = self.session_info_for_storage(session_id)?;
        list_session_attachment_infos(&info.cwd, session_id, true)
    }

    fn delete_session_attachment(&self, session_id: &str, path: &str) -> Result<(), String> {
        let info = self.session_info_for_storage(session_id)?;
        let attachments_dir = fs::canonicalize(session_attachment_dir(&info.cwd, session_id)?)
            .map_err(|err| format!("failed to resolve attachment directory: {err}"))?;
        let target = fs::canonicalize(path)
            .map_err(|err| format!("failed to resolve attachment path: {err}"))?;
        if !target.starts_with(&attachments_dir) {
            return Err("attachment path is outside this session".to_string());
        }
        fs::remove_file(&target)
            .map_err(|err| format!("failed to delete attachment {}: {err}", target.display()))
    }

    fn update_session_title(&self, session_id: &str, title: &str) -> Result<SessionInfo, String> {
        let normalized = normalize_session_title(title);
        if normalized.is_empty() {
            return Err("session title cannot be empty".to_string());
        }

        if let Some(session) = self.sessions.lock().get(session_id).cloned() {
            {
                let mut meta = session.meta.lock();
                meta.title = normalized;
                meta.last_active_at = unix_timestamp();
            }
            session.persist_meta();
            return Ok(session.info());
        }

        let mut meta = load_session_meta(session_id)?;
        meta.title = normalized;
        meta.last_active_at = unix_timestamp();
        persist_session_meta(&meta)?;
        Ok(meta.to_info())
    }

    fn session_info_for_storage(&self, session_id: &str) -> Result<SessionInfo, String> {
        if let Some(session) = self.sessions.lock().get(session_id).cloned() {
            return Ok(session.info());
        }
        load_session_meta(session_id).map(|meta| meta.to_info())
    }

    fn send_chat_message(&self, session_id: &str, message: &str) -> Result<(), String> {
        let session = self.get(session_id)?;
        if !matches!(session.info().status, SessionStatus::Running) {
            return Err("session is not running".to_string());
        }
        let clean_msg = message.trim();
        if clean_msg.is_empty() {
            return Ok(());
        }

        let is_first_message = {
            let meta = session.meta.lock();
            meta.first_user_message.is_none()
        };
        let info = session.info();
        let payload = if is_first_message && (info.agent_id == "agy" || info.agent_id == "codex") {
            format!(
                "{}\n<!-- waypoint_session_id: {} -->",
                clean_msg, session_id
            )
        } else {
            clean_msg.to_string()
        };

        let normalized = payload.replace('\n', "\r");
        let injected = format!("{normalized}\r");
        session
            .writer
            .lock()
            .write_all(injected.as_bytes())
            .map_err(|err| format!("failed to write chat message to PTY: {err}"))?;
        session.append_chat_user_message(clean_msg);
        session.append_input(&format!("{payload}\n"));
        session.remember_first_user_message(clean_msg);
        session.meta.lock().last_active_at = unix_timestamp();
        session.persist_meta();
        Ok(())
    }

    fn list_chat_messages(&self, session_id: &str) -> Result<Vec<ChatMessage>, String> {
        if let Some(session) = self.sessions.lock().get(session_id).cloned() {
            session.finalize_open_assistant_message_if_idle(CHAT_STREAM_IDLE_FINALIZE_MS);
            let messages = session.chat_messages.lock().clone();
            return Ok(messages);
        }

        // Fallback for historical sessions loaded from persisted metadata:
        // synthesize a readable assistant message from the persisted replay log.
        let meta = load_session_meta(session_id)?;
        let replay_bytes = read_persisted_replay(session_id)?;
        let replay_text = String::from_utf8_lossy(&replay_bytes).to_string();
        let cleaned = clean_terminal_output(&replay_text, CHAT_MESSAGE_CONTENT_LIMIT_CHARS);
        if cleaned.trim().is_empty() {
            return Ok(Vec::new());
        }

        Ok(vec![ChatMessage {
            id: format!("replay-{session_id}"),
            role: ChatRole::Assistant,
            content: cleaned,
            pending: false,
            created_at: meta.created_at,
            updated_at: meta.last_active_at.max(meta.created_at),
        }])
    }

    fn get_session_diff_snapshot(&self, session_id: &str) -> Result<SessionDiffSnapshot, String> {
        let session = self.get(session_id)?;
        let info = session.info();
        let branch = git_command(&info.cwd, &["branch", "--show-current"])
            .unwrap_or_else(|| "unknown".to_string());
        let status = git_command(&info.cwd, &["status", "--short"])
            .unwrap_or_else(|| "git status unavailable".to_string());
        let unstaged_diff =
            git_command(&info.cwd, &["diff"]).unwrap_or_else(|| "git diff unavailable".to_string());
        let staged_diff = git_command(&info.cwd, &["diff", "--staged"])
            .unwrap_or_else(|| "git staged diff unavailable".to_string());
        Ok(SessionDiffSnapshot {
            branch: empty_fallback(&branch, "unknown").to_string(),
            status: empty_fallback(&status, "clean or unavailable").to_string(),
            unstaged_diff: empty_fallback(&unstaged_diff, "No unstaged diff.").to_string(),
            staged_diff: empty_fallback(&staged_diff, "No staged diff.").to_string(),
            captured_at: unix_timestamp(),
        })
    }

    fn resize_session(&self, session_id: &str, rows: u16, cols: u16) -> Result<(), String> {
        if rows < 5 || cols < 10 {
            return Ok(());
        }
        let rows = rows.min(MAX_PTY_ROWS);
        let cols = cols.min(MAX_PTY_COLS);
        let session = self.get(session_id)?;
        if !matches!(session.info().status, SessionStatus::Running) {
            return Ok(());
        }
        session
            .master
            .lock()
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| format!("failed to resize PTY: {err}"))?;
        Ok(())
    }

    fn kill_session(&self, session_id: &str) -> Result<(), String> {
        let session = self.get(session_id)?;
        session
            .child
            .lock()
            .kill()
            .map_err(|err| format!("failed to kill session: {err}"))?;
        session.mark_status(SessionStatus::Exited);
        cache_agy_resume_ref_on_session(&session);
        Ok(())
    }

    fn delete_session(&self, session_id: &str) -> Result<(), String> {
        let live_session = self.sessions.lock().remove(session_id);
        if let Some(session) = live_session {
            *session.deleted.lock() = true;
            let _ = session.child.lock().kill();
        }

        let dir = session_dir(session_id)?;
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!(
                "failed to delete session history {}: {err}",
                dir.display()
            )),
        }
    }

    fn forward_session(
        &self,
        source_session_id: &str,
        target_session_id: &str,
        note: Option<String>,
        handover_mode: HandoverContentMode,
        edited_prompt: Option<String>,
    ) -> Result<HandoverResult, String> {
        if source_session_id == target_session_id {
            return Err("source and target sessions must be different".to_string());
        }

        let source = self.get(source_session_id)?;
        let target = self.get(target_session_id)?;
        let source_info = source.info();
        let target_info = target.info();

        let handover = self.write_handover_for(
            &source,
            &source_info,
            &target_info,
            note,
            handover_mode,
            edited_prompt.as_deref(),
            &target_info.cwd,
        )?;

        self.record_handover_link(&source_info, &target);
        self.remember_handover(&target, &handover.prompt);

        Ok(HandoverResult {
            prompt: handover.prompt,
            source_session: source_info,
            target_session: target_info,
            mode: "existing-session".to_string(),
            handover_mode: handover.effective_mode,
            handover_path: Some(handover.main_path.display().to_string()),
            evidence_path: handover
                .evidence_path
                .map(|path| path.display().to_string()),
        })
    }

    fn continue_session(
        &self,
        app: AppHandle,
        source_session_id: &str,
        target_agent_id: &str,
        cwd: String,
        note: Option<String>,
        handover_mode: HandoverContentMode,
        edited_prompt: Option<String>,
        rows: Option<u16>,
        cols: Option<u16>,
    ) -> Result<HandoverResult, String> {
        let source = self.get(source_session_id)?;
        let source_info = source.info();
        if !matches!(source_info.status, SessionStatus::Running) {
            return Err(format!(
                "source session is not running: {}",
                source_info.title
            ));
        }

        if target_agent_id == "agy" {
            return self.continue_agy_with_initial_prompt(
                app,
                &source,
                source_info,
                cwd,
                note,
                handover_mode,
                edited_prompt.as_deref(),
                rows,
                cols,
            );
        }

        if target_agent_id == "claude-code" {
            return self.continue_claude_with_initial_prompt(
                app,
                &source,
                source_info,
                cwd,
                note,
                handover_mode,
                edited_prompt.as_deref(),
                rows,
                cols,
            );
        }

        if target_agent_id == "codex" {
            return self.continue_codex_with_initial_prompt(
                app,
                &source,
                source_info,
                cwd,
                note,
                handover_mode,
                edited_prompt.as_deref(),
                rows,
                cols,
            );
        }

        if target_agent_id == "copilot" {
            return self.continue_copilot_with_initial_prompt(
                app,
                &source,
                source_info,
                cwd,
                note,
                handover_mode,
                edited_prompt.as_deref(),
                rows,
                cols,
            );
        }

        let target_info = self.create_agent_session(
            app,
            target_agent_id,
            cwd,
            source_info.dangerous,
            source_info.none_workspace,
            rows,
            cols,
        )?;
        let target = self.get(&target_info.id)?;
        let handover = self.inject_handover(
            &source,
            &target,
            note,
            handover_mode,
            edited_prompt.as_deref(),
            true,
        )?;
        let target_info = target.info();

        Ok(HandoverResult {
            prompt: handover.prompt,
            source_session: source_info,
            target_session: target_info,
            mode: "new-session".to_string(),
            handover_mode: handover.effective_mode,
            handover_path: Some(handover.main_path.display().to_string()),
            evidence_path: handover
                .evidence_path
                .map(|path| path.display().to_string()),
        })
    }

    fn continue_claude_with_initial_prompt(
        &self,
        app: AppHandle,
        source: &Arc<PtySession>,
        source_info: SessionInfo,
        cwd: String,
        note: Option<String>,
        handover_mode: HandoverContentMode,
        edited_prompt: Option<&str>,
        rows: Option<u16>,
        cols: Option<u16>,
    ) -> Result<HandoverResult, String> {
        let definition = agent_definitions()
            .into_iter()
            .find(|definition| definition.id == "claude-code")
            .ok_or_else(|| "Claude Code preset is missing".to_string())?;
        let resolved = resolve_agent_command(&definition).ok_or_else(|| {
            "Claude Code is not available in PATH. Install it or make sure your login shell can resolve it."
                .to_string()
        })?;
        let target_id = Uuid::new_v4().to_string();
        let planned_target = SessionInfo {
            id: target_id.clone(),
            agent_id: definition.id.to_string(),
            agent_name: definition.name.to_string(),
            title: "Claude Code new session".to_string(),
            command: "claude <handover>".to_string(),
            cwd: cwd.clone(),
            status: SessionStatus::Running,
            attached: false,
            created_at: unix_timestamp(),
            last_active_at: unix_timestamp(),
            first_user_message: None,
            native_session_ref: None,
            parent_session_id: None,
            handover_root_id: None,
            dangerous: source_info.dangerous,
            none_workspace: source_info.none_workspace,
        };
        let handover = self.write_handover_for(
            source,
            &source_info,
            &planned_target,
            note.clone(),
            handover_mode,
            edited_prompt,
            &cwd,
        )?;
        let startup_prompt = handover_reference_startup_prompt(&handover.main_path, &target_id);

        let mut args = resolved.args;
        args.push(startup_prompt);
        if source_info.dangerous {
            apply_dangerous_flag(definition.id, &mut args);
        }

        let target_info = self.spawn_session_with_identity(
            app,
            definition.id,
            definition.name,
            definition.name.to_string(),
            "claude <handover>".to_string(),
            resolved.executable,
            args,
            cwd,
            rows,
            cols,
            Vec::new(),
            Some(target_id),
            None,
            None,
            None,
            None,
            Some(source_info.id.clone()),
            Some(
                source_info
                    .handover_root_id
                    .clone()
                    .unwrap_or(source_info.id.clone()),
            ),
            source_info.dangerous,
            source_info.none_workspace,
        )?;
        let target = self.get(&target_info.id)?;
        self.record_handover_link(&source_info, &target);
        let target_info = target.info();
        self.remember_handover(&target, &handover.prompt);

        Ok(HandoverResult {
            prompt: handover.prompt,
            source_session: source_info,
            target_session: target_info,
            mode: "new-session".to_string(),
            handover_mode: handover.effective_mode,
            handover_path: Some(handover.main_path.display().to_string()),
            evidence_path: handover
                .evidence_path
                .map(|path| path.display().to_string()),
        })
    }

    fn continue_agy_with_initial_prompt(
        &self,
        app: AppHandle,
        source: &Arc<PtySession>,
        source_info: SessionInfo,
        cwd: String,
        note: Option<String>,
        handover_mode: HandoverContentMode,
        edited_prompt: Option<&str>,
        rows: Option<u16>,
        cols: Option<u16>,
    ) -> Result<HandoverResult, String> {
        let definition = agent_definitions()
            .into_iter()
            .find(|definition| definition.id == "agy")
            .ok_or_else(|| "Antigravity CLI preset is missing".to_string())?;
        let resolved = resolve_agent_command(&definition).ok_or_else(|| {
            "Antigravity CLI is not available in PATH. Install it or make sure your login shell can resolve it."
                .to_string()
        })?;
        let target_id = Uuid::new_v4().to_string();
        let planned_target = SessionInfo {
            id: target_id.clone(),
            agent_id: definition.id.to_string(),
            agent_name: definition.name.to_string(),
            title: "Antigravity CLI new session".to_string(),
            command: "agy --prompt-interactive <handover>".to_string(),
            cwd: cwd.clone(),
            status: SessionStatus::Running,
            attached: false,
            created_at: unix_timestamp(),
            last_active_at: unix_timestamp(),
            first_user_message: None,
            native_session_ref: None,
            parent_session_id: None,
            handover_root_id: None,
            dangerous: source_info.dangerous,
            none_workspace: source_info.none_workspace,
        };
        let handover = self.write_handover_for(
            source,
            &source_info,
            &planned_target,
            note.clone(),
            handover_mode,
            edited_prompt,
            &cwd,
        )?;
        let startup_prompt = handover_reference_startup_prompt(&handover.main_path, &target_id);
        let mut args = resolved.args;
        if let Some(parent) = handover.main_path.parent() {
            args.push("--add-dir".to_string());
            args.push(parent.to_string_lossy().into_owned());
        }
        args.push("--prompt-interactive".to_string());
        args.push(startup_prompt);
        let target_info = self.spawn_session_with_identity(
            app,
            definition.id,
            definition.name,
            definition.name.to_string(),
            "agy --prompt-interactive <handover>".to_string(),
            resolved.executable,
            args,
            cwd,
            rows,
            cols,
            Vec::new(),
            Some(target_id),
            None,
            None,
            None,
            None,
            None,
            None,
            source_info.dangerous,
            source_info.none_workspace,
        )?;
        let target = self.get(&target_info.id)?;
        self.record_handover_link(&source_info, &target);
        let target_info = target.info();
        self.remember_handover(&target, &handover.prompt);

        Ok(HandoverResult {
            prompt: handover.prompt,
            source_session: source_info,
            target_session: target_info,
            mode: "new-session".to_string(),
            handover_mode: handover.effective_mode,
            handover_path: Some(handover.main_path.display().to_string()),
            evidence_path: handover
                .evidence_path
                .map(|path| path.display().to_string()),
        })
    }

    fn continue_codex_with_initial_prompt(
        &self,
        app: AppHandle,
        source: &Arc<PtySession>,
        source_info: SessionInfo,
        cwd: String,
        note: Option<String>,
        handover_mode: HandoverContentMode,
        edited_prompt: Option<&str>,
        rows: Option<u16>,
        cols: Option<u16>,
    ) -> Result<HandoverResult, String> {
        let definition = agent_definitions()
            .into_iter()
            .find(|definition| definition.id == "codex")
            .ok_or_else(|| "Codex CLI preset is missing".to_string())?;
        let resolved = resolve_agent_command(&definition).ok_or_else(|| {
            "Codex CLI is not available in PATH. Install it or make sure your login shell can resolve it."
                .to_string()
        })?;
        let target_id = Uuid::new_v4().to_string();
        let planned_target = SessionInfo {
            id: target_id.clone(),
            agent_id: definition.id.to_string(),
            agent_name: definition.name.to_string(),
            title: "Codex new session".to_string(),
            command: "codex --no-alt-screen <handover>".to_string(),
            cwd: cwd.clone(),
            status: SessionStatus::Running,
            attached: false,
            created_at: unix_timestamp(),
            last_active_at: unix_timestamp(),
            first_user_message: None,
            native_session_ref: None,
            parent_session_id: None,
            handover_root_id: None,
            dangerous: source_info.dangerous,
            none_workspace: source_info.none_workspace,
        };
        let handover = self.write_handover_for(
            source,
            &source_info,
            &planned_target,
            note,
            handover_mode,
            edited_prompt,
            &cwd,
        )?;
        let startup_prompt = handover_reference_startup_prompt(&handover.main_path, &target_id);

        let mut args = resolved.args;
        if let Some(parent) = handover.main_path.parent() {
            args.push("--add-dir".to_string());
            args.push(parent.to_string_lossy().into_owned());
        }
        args.push(startup_prompt);
        if source_info.dangerous {
            apply_dangerous_flag(definition.id, &mut args);
        }

        let target_info = self.spawn_session_with_identity(
            app,
            definition.id,
            definition.name,
            definition.name.to_string(),
            "codex --no-alt-screen <handover>".to_string(),
            resolved.executable,
            args,
            cwd,
            rows,
            cols,
            Vec::new(),
            Some(target_id),
            None,
            None,
            None,
            None,
            None,
            None,
            source_info.dangerous,
            source_info.none_workspace,
        )?;
        let target = self.get(&target_info.id)?;
        self.record_handover_link(&source_info, &target);
        let target_info = target.info();
        self.remember_handover(&target, &handover.prompt);

        Ok(HandoverResult {
            prompt: handover.prompt,
            source_session: source_info,
            target_session: target_info,
            mode: "new-session".to_string(),
            handover_mode: handover.effective_mode,
            handover_path: Some(handover.main_path.display().to_string()),
            evidence_path: handover
                .evidence_path
                .map(|path| path.display().to_string()),
        })
    }

    fn continue_copilot_with_initial_prompt(
        &self,
        app: AppHandle,
        source: &Arc<PtySession>,
        source_info: SessionInfo,
        cwd: String,
        note: Option<String>,
        handover_mode: HandoverContentMode,
        edited_prompt: Option<&str>,
        rows: Option<u16>,
        cols: Option<u16>,
    ) -> Result<HandoverResult, String> {
        let definition = agent_definitions()
            .into_iter()
            .find(|definition| definition.id == "copilot")
            .ok_or_else(|| "GitHub Copilot preset is missing".to_string())?;
        let resolved = resolve_agent_command(&definition).ok_or_else(|| {
            "GitHub Copilot CLI is not available in PATH. Install it or make sure your login shell can resolve it."
                .to_string()
        })?;
        let display_command = format!("{} -i <handover>", resolved.display);
        let target_id = Uuid::new_v4().to_string();
        let planned_target = SessionInfo {
            id: target_id.clone(),
            agent_id: definition.id.to_string(),
            agent_name: definition.name.to_string(),
            title: "GitHub Copilot new session".to_string(),
            command: display_command.clone(),
            cwd: cwd.clone(),
            status: SessionStatus::Running,
            attached: false,
            created_at: unix_timestamp(),
            last_active_at: unix_timestamp(),
            first_user_message: None,
            native_session_ref: None,
            parent_session_id: None,
            handover_root_id: None,
            dangerous: source_info.dangerous,
            none_workspace: source_info.none_workspace,
        };
        let handover = self.write_handover_for(
            source,
            &source_info,
            &planned_target,
            note.clone(),
            handover_mode,
            edited_prompt,
            &cwd,
        )?;
        let startup_prompt = handover_reference_startup_prompt(&handover.main_path, &target_id);

        let mut args = resolved.args;
        if let Some(parent) = handover.main_path.parent() {
            append_copilot_cli_option(&mut args, "--add-dir".to_string());
            append_copilot_cli_option(&mut args, parent.to_string_lossy().into_owned());
        }
        append_copilot_cli_option(&mut args, "-i".to_string());
        append_copilot_cli_option(&mut args, startup_prompt);

        let target_info = self.spawn_session_with_identity(
            app,
            definition.id,
            definition.name,
            definition.name.to_string(),
            display_command,
            resolved.executable,
            args,
            cwd,
            rows,
            cols,
            Vec::new(),
            Some(target_id),
            None,
            None,
            None,
            None,
            None,
            None,
            source_info.dangerous,
            source_info.none_workspace,
        )?;
        let target = self.get(&target_info.id)?;
        self.record_handover_link(&source_info, &target);
        let target_info = target.info();
        self.remember_handover(&target, &handover.prompt);

        Ok(HandoverResult {
            prompt: handover.prompt,
            source_session: source_info,
            target_session: target_info,
            mode: "new-session".to_string(),
            handover_mode: handover.effective_mode,
            handover_path: Some(handover.main_path.display().to_string()),
            evidence_path: handover
                .evidence_path
                .map(|path| path.display().to_string()),
        })
    }

    fn inject_handover(
        &self,
        source: &Arc<PtySession>,
        target: &Arc<PtySession>,
        note: Option<String>,
        handover_mode: HandoverContentMode,
        edited_prompt: Option<&str>,
        target_is_new: bool,
    ) -> Result<WrittenHandover, String> {
        let source_info = source.info();
        let target_info = target.info();
        let handover = self.write_handover_for(
            source,
            &source_info,
            &target_info,
            note,
            handover_mode,
            edited_prompt,
            &target_info.cwd,
        )?;
        let display_path = handover
            .main_path
            .strip_prefix(&target_info.cwd)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| handover.main_path.display().to_string());
        let short_instruction = format!(
            "A handover context file is referenced at {display_path}. Read only this exact file (no directory listing or glob scanning), acknowledge context loaded, then wait for my next instruction."
        );

        if target_is_new {
            thread::sleep(Duration::from_millis(handover_startup_delay_ms(
                &target_info.agent_id,
            )));
        }
        inject_with_retry(target, &short_instruction)?;
        self.record_handover_link(&source_info, target);
        self.remember_handover(target, &handover.prompt);
        target.meta.lock().last_active_at = unix_timestamp();

        Ok(handover)
    }

    fn record_handover_link(&self, source: &SessionInfo, target: &Arc<PtySession>) {
        let mut meta = target.meta.lock();
        meta.parent_session_id = Some(source.id.clone());
        meta.handover_root_id = Some(
            source
                .handover_root_id
                .clone()
                .filter(|id| !id.trim().is_empty())
                .unwrap_or_else(|| source.id.clone()),
        );
        meta.last_active_at = unix_timestamp();
        drop(meta);
        target.persist_meta();
    }

    fn remember_handover(&self, target: &Arc<PtySession>, prompt: &str) {
        *target.inherited_handover.lock() =
            format_handover_for_inheritance(prompt, HANDOVER_INHERITED_STORE_CHARS);
    }

    fn get_handover_preview(&self, source_session_id: &str) -> Result<HandoverPreview, String> {
        let source = self.get(source_session_id)?;
        Ok(self.build_handover_preview_for(&source))
    }

    #[allow(clippy::too_many_arguments)]
    fn get_handover_draft(
        &self,
        source_session_id: &str,
        target_mode: &str,
        target_session_id: Option<&str>,
        target_agent_id: Option<&str>,
        cwd: Option<&str>,
        note: Option<String>,
        requested_mode: HandoverContentMode,
    ) -> Result<HandoverDraft, String> {
        let source = self.get(source_session_id)?;
        let source_info = source.info();
        let target_info = match target_mode {
            "existing" => {
                if let Some(target_id) =
                    target_session_id.filter(|id| !id.trim().is_empty())
                {
                    self.get(target_id)?.info()
                } else {
                    planned_file_handover_target_info(&source_info)
                }
            }
            "new" => {
                let agent_id = target_agent_id
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(|| {
                        "target agent is required for new-session handover".to_string()
                    })?;
                let target_cwd = cwd
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or(&source_info.cwd);
                planned_handover_target_info(
                    agent_id,
                    target_cwd,
                    source_info.dangerous,
                    source_info.none_workspace,
                )?
            }
            other => return Err(format!("unknown handover target mode: {other}")),
        };

        Ok(
            self.build_handover_draft_for(
                &source,
                &source_info,
                &target_info,
                note,
                requested_mode,
            ),
        )
    }

    fn create_handover_file(
        &self,
        source_session_id: &str,
        note: Option<String>,
        requested_mode: HandoverContentMode,
        edited_prompt: Option<String>,
    ) -> Result<HandoverFileResult, String> {
        let source = self.get(source_session_id)?;
        let source_info = source.info();
        let target_info = planned_file_handover_target_info(&source_info);
        let handover = self.write_handover_for(
            &source,
            &source_info,
            &target_info,
            note,
            requested_mode,
            edited_prompt.as_deref(),
            &source_info.cwd,
        )?;

        Ok(HandoverFileResult {
            prompt: handover.prompt,
            source_session: source_info,
            handover_mode: handover.effective_mode,
            handover_path: handover.main_path.display().to_string(),
            evidence_path: handover
                .evidence_path
                .map(|path| path.display().to_string()),
        })
    }

    fn build_handover_preview_for(&self, source: &Arc<PtySession>) -> HandoverPreview {
        let source_info = source.info();
        let terminal_context_chars =
            build_handover_source_context(source, &source_info, HANDOVER_CONTEXT_CHARS)
                .chars()
                .count();
        let user_input_chars =
            build_handover_user_inputs(source, &source_info, HANDOVER_USER_INPUT_CHARS)
                .chars()
                .count();
        let inherited_context_chars = tail_chars(
            &source.inherited_handover.lock(),
            HANDOVER_INHERITED_CONTEXT_CHARS,
        )
        .chars()
        .count();
        let estimated_chars = terminal_context_chars + user_input_chars + inherited_context_chars;
        let is_large = estimated_chars > HANDOVER_LARGE_THRESHOLD_CHARS;

        HandoverPreview {
            estimated_chars,
            large_threshold_chars: HANDOVER_LARGE_THRESHOLD_CHARS,
            is_large,
            recommended_mode: if is_large { "compact" } else { "full" }.to_string(),
            terminal_context_chars,
            user_input_chars,
            inherited_context_chars,
        }
    }

    fn build_handover_draft_for(
        &self,
        source: &Arc<PtySession>,
        source_info: &SessionInfo,
        target_info: &SessionInfo,
        note: Option<String>,
        requested_mode: HandoverContentMode,
    ) -> HandoverDraft {
        let preview = self.build_handover_preview_for(source);
        let effective_mode = resolve_handover_mode(requested_mode, preview.is_large);
        let evidence_path = if matches!(effective_mode, EffectiveHandoverMode::Compact) {
            Some(
                "Preview only: full evidence path is assigned when handover is created."
                    .to_string(),
            )
        } else {
            None
        };
        let prompt = match effective_mode {
            EffectiveHandoverMode::Compact => self.build_compact_handover_prompt_for(
                source,
                source_info,
                target_info,
                note,
                evidence_path.as_deref(),
            ),
            EffectiveHandoverMode::Full => {
                self.build_handover_prompt_for(source, source_info, target_info, note)
            }
        };

        HandoverDraft {
            prompt,
            effective_mode: effective_mode.as_str().to_string(),
            estimated_chars: preview.estimated_chars,
            evidence_path,
        }
    }

    fn write_handover_for(
        &self,
        source: &Arc<PtySession>,
        source_info: &SessionInfo,
        target_info: &SessionInfo,
        note: Option<String>,
        requested_mode: HandoverContentMode,
        edited_prompt: Option<&str>,
        cwd: &str,
    ) -> Result<WrittenHandover, String> {
        let preview = self.build_handover_preview_for(source);
        let effective_mode = resolve_handover_mode(requested_mode, preview.is_large);
        let paths = reserve_handover_paths(cwd)?;
        let evidence_path_display = if matches!(effective_mode, EffectiveHandoverMode::Compact) {
            Some(paths.evidence_path.display().to_string())
        } else {
            None
        };
        let generated_prompt = match effective_mode {
            EffectiveHandoverMode::Compact => self.build_compact_handover_prompt_for(
                source,
                source_info,
                target_info,
                note.clone(),
                evidence_path_display.as_deref(),
            ),
            EffectiveHandoverMode::Full => {
                self.build_handover_prompt_for(source, source_info, target_info, note.clone())
            }
        };
        let prompt = edited_prompt
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or(generated_prompt);
        let evidence = if matches!(effective_mode, EffectiveHandoverMode::Compact) {
            Some(self.build_full_handover_evidence_for(source, source_info, target_info, note))
        } else {
            None
        };

        write_handover_files(&paths, &prompt, evidence.as_deref())?;

        Ok(WrittenHandover {
            prompt,
            main_path: paths.main_path,
            evidence_path: evidence.map(|_| paths.evidence_path),
            effective_mode: effective_mode.as_str().to_string(),
        })
    }

    fn resolve_and_cache_agy_conversation_id(
        &self,
        source: &Arc<PtySession>,
        source_info: &SessionInfo,
    ) {
        if source_info.agent_id != "agy" {
            return;
        }
        cache_agy_resume_ref_on_session(source);
    }

    fn build_handover_prompt_for(
        &self,
        source: &Arc<PtySession>,
        source_info: &SessionInfo,
        target_info: &SessionInfo,
        note: Option<String>,
    ) -> String {
        self.resolve_and_cache_agy_conversation_id(source, source_info);

        let recent_context =
            build_handover_source_context(source, source_info, HANDOVER_CONTEXT_CHARS);
        let recent_user_inputs =
            build_handover_user_inputs(source, source_info, HANDOVER_USER_INPUT_CHARS);
        let inherited_handover = tail_chars(
            &source.inherited_handover.lock(),
            HANDOVER_INHERITED_CONTEXT_CHARS,
        );
        let artifacts = build_agy_artifacts_context(&source_info.id);

        build_handover_prompt(
            &source_info,
            &target_info,
            note.as_deref().unwrap_or_default(),
            &inherited_handover,
            &recent_context,
            &recent_user_inputs,
            &artifacts,
        )
    }

    fn build_compact_handover_prompt_for(
        &self,
        source: &Arc<PtySession>,
        source_info: &SessionInfo,
        target_info: &SessionInfo,
        note: Option<String>,
        evidence_path: Option<&str>,
    ) -> String {
        self.resolve_and_cache_agy_conversation_id(source, source_info);

        let recent_context =
            build_handover_source_context(source, source_info, COMPACT_HANDOVER_CONTEXT_CHARS);
        let recent_user_inputs =
            build_handover_user_inputs(source, source_info, COMPACT_USER_INPUT_CHARS);
        let inherited_handover = tail_chars(
            &source.inherited_handover.lock(),
            COMPACT_HANDOVER_INHERITED_CONTEXT_CHARS,
        );
        let artifacts = build_agy_artifacts_context(&source_info.id);

        build_compact_handover_prompt(
            source_info,
            target_info,
            note.as_deref().unwrap_or_default(),
            &inherited_handover,
            &recent_context,
            &recent_user_inputs,
            evidence_path,
            &artifacts,
        )
    }

    fn build_full_handover_evidence_for(
        &self,
        source: &Arc<PtySession>,
        source_info: &SessionInfo,
        target_info: &SessionInfo,
        note: Option<String>,
    ) -> String {
        self.resolve_and_cache_agy_conversation_id(source, source_info);

        let recent_context =
            build_handover_source_context(source, source_info, HANDOVER_CONTEXT_CHARS);
        let recent_user_inputs =
            build_handover_user_inputs(source, source_info, HANDOVER_USER_INPUT_CHARS);
        let inherited_handover = tail_chars(
            &source.inherited_handover.lock(),
            HANDOVER_INHERITED_CONTEXT_CHARS,
        );
        let artifacts = build_agy_artifacts_context(&source_info.id);

        build_full_handover_evidence(
            source_info,
            target_info,
            note.as_deref().unwrap_or_default(),
            &inherited_handover,
            &recent_context,
            &recent_user_inputs,
            &artifacts,
        )
    }

    fn get(&self, session_id: &str) -> Result<Arc<PtySession>, String> {
        self.sessions
            .lock()
            .get(session_id)
            .cloned()
            .ok_or_else(|| format!("unknown session: {session_id}"))
    }
}

fn build_agy_artifacts_context(session_id: &str) -> String {
    let Some(conversation_id) = find_agy_conversation_id(session_id) else {
        return String::new();
    };
    let Some(home) = home_dir() else {
        return String::new();
    };
    let brain_dir = home
        .join(".gemini")
        .join("antigravity-cli")
        .join("brain")
        .join(&conversation_id);
    if !brain_dir.is_dir() {
        return String::new();
    }

    let mut artifact_blocks = Vec::new();
    if let Ok(entries) = fs::read_dir(brain_dir) {
        let mut sorted_entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
        sorted_entries.sort_by_key(|entry| entry.file_name());

        for entry in sorted_entries {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    if ext == "md" {
                        let filename = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        if let Ok(content) = fs::read_to_string(&path) {
                            if !content.trim().is_empty() {
                                artifact_blocks.push(format!(
                                    "### [{filename}](file://{})\n\n{}\n",
                                    path.display(),
                                    content.trim()
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    if artifact_blocks.is_empty() {
        String::new()
    } else {
        format!(
            "## Session Artifacts\n\nThe following technical proposals and artifacts were generated during this session:\n\n{}",
            artifact_blocks.join("\n---\n\n")
        )
    }
}

impl PtySession {
    fn info(&self) -> SessionInfo {
        self.meta.lock().to_info()
    }

    fn mark_status(&self, status: SessionStatus) {
        let mut meta = self.meta.lock();
        meta.status = status;
        meta.last_active_at = unix_timestamp();
        drop(meta);
        self.persist_meta();
    }

    fn persist_meta(&self) {
        if *self.deleted.lock() {
            return;
        }
        let meta = self.meta.lock().clone();
        if let Err(err) = persist_session_meta(&meta) {
            eprintln!("[waypoint] failed to persist session metadata: {err}");
        }
    }

    fn append_ring(&self, data: &str) {
        let mut ring = self.ring.lock();
        ring.push_str(data);
        if ring.chars().count() > RING_LIMIT_CHARS {
            *ring = ring
                .chars()
                .rev()
                .take(RING_LIMIT_CHARS)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
        }
    }

    fn append_input(&self, data: &str) {
        let mut input_ring = self.input_ring.lock();
        input_ring.push_str(data);
        if input_ring.chars().count() > INPUT_RING_LIMIT_CHARS {
            *input_ring = input_ring
                .chars()
                .rev()
                .take(INPUT_RING_LIMIT_CHARS)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
        }
    }

    fn append_render(&self, data: &[u8]) {
        let mut render_ring = self.render_ring.lock();
        render_ring.extend_from_slice(data);
        if render_ring.len() > RENDER_RING_LIMIT_BYTES {
            let drop_len = render_ring.len() - RENDER_RING_LIMIT_BYTES;
            render_ring.drain(0..drop_len);
        }
    }

    fn capture_user_input(&self, data: &str) {
        let mut pending = self.pending_user_input.lock();
        let submitted = extract_submitted_user_inputs(&mut pending, data);
        drop(pending);

        for candidate in submitted {
            self.append_chat_user_message(&candidate);
            self.remember_first_user_message(&candidate);
        }
    }

    fn remember_first_user_message(&self, value: &str) {
        let normalized = normalize_session_title(value);
        if normalized.is_empty() {
            return;
        }

        let mut meta = self.meta.lock();
        if meta.first_user_message.is_some() {
            return;
        }
        meta.first_user_message = Some(normalized);
        meta.last_active_at = unix_timestamp();
        drop(meta);
        self.persist_meta();
    }

    fn append_chat_user_message(&self, content: &str) {
        let mut open_assistant_index = self.open_assistant_index.lock();
        let mut messages = self.chat_messages.lock();

        if let Some(index) = *open_assistant_index {
            if let Some(message) = messages.get_mut(index) {
                message.pending = false;
                message.updated_at = unix_timestamp();
            }
            *open_assistant_index = None;
        }

        let now = unix_timestamp();
        messages.push(ChatMessage {
            id: Uuid::new_v4().to_string(),
            role: ChatRole::User,
            content: content.to_string(),
            pending: false,
            created_at: now,
            updated_at: now,
        });

        trim_chat_messages(&mut messages, &mut open_assistant_index);
    }

    fn append_chat_assistant_output(&self, chunk: &str, replace_existing: bool) {
        if chunk.is_empty() {
            return;
        }

        let mut open_assistant_index = self.open_assistant_index.lock();
        let mut messages = self.chat_messages.lock();
        let now = unix_timestamp();

        let index = match *open_assistant_index {
            Some(existing) => existing,
            None => {
                messages.push(ChatMessage {
                    id: Uuid::new_v4().to_string(),
                    role: ChatRole::Assistant,
                    content: String::new(),
                    pending: true,
                    created_at: now,
                    updated_at: now,
                });
                let created_index = messages.len() - 1;
                *open_assistant_index = Some(created_index);
                created_index
            }
        };

        if let Some(message) = messages.get_mut(index) {
            if replace_existing {
                message.content = merge_assistant_output(&message.content, chunk, true);
            } else if !message.content.ends_with(chunk) {
                message.content = merge_assistant_output(&message.content, chunk, false);
            }
            if message.content.chars().count() > CHAT_MESSAGE_CONTENT_LIMIT_CHARS {
                message.content = truncate_tail(&message.content, CHAT_MESSAGE_CONTENT_LIMIT_CHARS);
            }
            message.pending = true;
            message.updated_at = now;
            *self.last_assistant_output_at_ms.lock() = Some(unix_timestamp_ms());
        } else {
            *open_assistant_index = None;
        }

        trim_chat_messages(&mut messages, &mut open_assistant_index);
    }

    fn finalize_open_assistant_message(&self) {
        let mut open_assistant_index = self.open_assistant_index.lock();
        let mut messages = self.chat_messages.lock();
        if let Some(index) = *open_assistant_index {
            if let Some(message) = messages.get_mut(index) {
                message.pending = false;
                message.updated_at = unix_timestamp();
            }
            *open_assistant_index = None;
            *self.last_assistant_output_at_ms.lock() = None;
        }
    }

    fn finalize_open_assistant_message_if_idle(&self, idle_ms: u64) {
        let last_output = *self.last_assistant_output_at_ms.lock();
        let Some(last_output) = last_output else {
            return;
        };

        if unix_timestamp_ms().saturating_sub(last_output) < idle_ms {
            return;
        }

        self.finalize_open_assistant_message();
    }
}

fn cache_agy_resume_ref_on_session(session: &Arc<PtySession>) -> bool {
    let changed = {
        let mut meta = session.meta.lock();
        cache_agy_resume_ref_in_meta(&mut meta)
    };
    if changed {
        session.persist_meta();
    }
    changed
}

fn should_append_agy_session_marker(session: &Arc<PtySession>, data: &str) -> bool {
    if !data.contains('\r') && !data.contains('\n') {
        return false;
    }
    {
        let meta = session.meta.lock();
        if (meta.agent_id != "agy" && meta.agent_id != "codex") || meta.first_user_message.is_some() {
            return false;
        }
    }

    let mut pending = session.pending_user_input.lock().clone();
    !extract_submitted_user_inputs(&mut pending, data).is_empty()
}

fn cache_agy_resume_ref_in_meta(meta: &mut SessionMeta) -> bool {
    if meta.agent_id != "agy" {
        return false;
    }

    let existing = meta.native_session_ref.clone();
    let existing_id = existing
        .as_ref()
        .and_then(|session_ref| session_ref.id.clone())
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());
    let existing_project = existing
        .as_ref()
        .and_then(|session_ref| session_ref.project.clone())
        .map(|project| project.trim().to_string())
        .filter(|project| !project.is_empty());

    let resume_ref = resolve_agy_resume_ref(&meta.id).or_else(|| {
        existing_id.clone().map(|conversation_id| {
            let project = existing_project
                .clone()
                .or_else(|| {
                    existing
                        .as_ref()
                        .and_then(|session_ref| session_ref.resume_command.as_deref())
                        .and_then(parse_agy_resume_ref)
                        .and_then(|resume_ref| resume_ref.project)
                })
                .or_else(|| {
                    find_agy_resume_ref_in_waypoint_transcript(&meta.id)
                        .and_then(|resume_ref| resume_ref.project)
                });
            AgyResumeRef {
                conversation_id,
                project,
            }
        })
    });

    let Some(resume_ref) = resume_ref else {
        return false;
    };

    let resume_command = format_agy_resume_command("agy", &resume_ref);
    let name = existing
        .as_ref()
        .and_then(|session_ref| session_ref.name.clone());
    let discovered_at = existing
        .as_ref()
        .map(|session_ref| session_ref.discovered_at)
        .filter(|value| *value > 0)
        .unwrap_or_else(unix_timestamp);
    let next = NativeSessionRef {
        provider: "agy".to_string(),
        id: Some(resume_ref.conversation_id),
        name,
        project: resume_ref.project,
        resume_command: Some(resume_command),
        discovered_at,
    };

    let changed = existing
        .as_ref()
        .map(|current| {
            current.provider != next.provider
                || current.id != next.id
                || current.project != next.project
                || current.resume_command != next.resume_command
        })
        .unwrap_or(true);
    if changed {
        meta.native_session_ref = Some(next);
        meta.last_active_at = unix_timestamp();
    }
    changed
}

impl SessionMeta {
    fn to_info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id.clone(),
            agent_id: self.agent_id.clone(),
            agent_name: self.agent_name.clone(),
            title: self.title.clone(),
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            status: self.status.clone(),
            attached: self.attached,
            created_at: self.created_at,
            last_active_at: self.last_active_at,
            first_user_message: self.first_user_message.clone(),
            native_session_ref: self.native_session_ref.clone(),
            parent_session_id: self.parent_session_id.clone(),
            handover_root_id: self.handover_root_id.clone(),
            dangerous: self.dangerous,
            none_workspace: self.none_workspace,
        }
    }
}

fn default_cwd() -> String {
    if let Ok(path) = env::current_dir() {
        if let Some(normalized) = canonicalize_workspace_dir(&path) {
            if normalized != "/" {
                return normalized;
            }
        }
        if let Some(raw) = path.to_str() {
            if raw != "/" {
                return raw.to_string();
            }
        }
    }

    if let Ok(home) = env::var("HOME") {
        if let Some(normalized) = canonicalize_workspace_dir(Path::new(&home)) {
            return normalized;
        }
        return home;
    }

    "/".to_string()
}

fn canonicalize_workspace_dir(path: &Path) -> Option<String> {
    fs::canonicalize(path)
        .ok()
        .and_then(|resolved| resolved.to_str().map(ToOwned::to_owned))
}

fn waypoint_root_dir() -> Result<PathBuf, String> {
    let home = env::var("HOME").map_err(|err| format!("failed to resolve HOME: {err}"))?;
    // Separate dev (debug) and prod (release) storage so running `tauri dev`
    // does not share sessions/handovers with an installed DMG. Reinstalling
    // the DMG (release) won't touch the dev store, and vice versa.
    let leaf = if cfg!(debug_assertions) {
        ".waypoint-dev"
    } else {
        ".waypoint"
    };
    Ok(PathBuf::from(home).join(leaf))
}

fn waypoint_sessions_dir() -> Result<PathBuf, String> {
    Ok(waypoint_root_dir()?.join("sessions"))
}

fn session_dir(session_id: &str) -> Result<PathBuf, String> {
    Ok(waypoint_sessions_dir()?.join(session_id))
}

fn session_meta_path(session_id: &str) -> Result<PathBuf, String> {
    Ok(session_dir(session_id)?.join("meta.json"))
}

fn session_transcript_path(session_id: &str) -> Result<PathBuf, String> {
    Ok(session_dir(session_id)?.join("transcript.log"))
}

fn session_attachment_dir(cwd: &str, session_id: &str) -> Result<PathBuf, String> {
    // Attachments must live INSIDE the session's cwd so that agent CLIs with
    // project-scoped file access (e.g. Claude Code's Read tool) can read them.
    // Saving under ~/.waypoint/ made the path invisible to those tools and
    // caused "Read returned empty" errors when the user pasted an image and
    // sent the resolved path to the agent.
    let cwd_path = PathBuf::from(cwd);
    if !cwd_path.is_dir() {
        return Err(format!("workspace directory does not exist: {cwd}"));
    }
    let cwd_canonical = fs::canonicalize(&cwd_path).unwrap_or(cwd_path);
    Ok(cwd_canonical.join(".waypoint-attachments").join(session_id))
}

/// Ensure the attachment directory exists and is ignored by git so pasted
/// screenshots don't pollute the user's project status.
fn ensure_session_attachment_dir(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|err| {
        format!(
            "failed to create attachment directory {}: {err}",
            dir.display()
        )
    })?;
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        let _ = fs::write(&gitignore, "*\n");
    }
    Ok(())
}

fn normalize_attachment_mime(mime: &str) -> Result<(&'static str, &'static str), String> {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/png" => Ok(("image/png", "png")),
        "image/jpeg" | "image/jpg" => Ok(("image/jpeg", "jpg")),
        "image/webp" => Ok(("image/webp", "webp")),
        "image/gif" => Ok(("image/gif", "gif")),
        other => Err(format!("unsupported attachment type: {other}")),
    }
}

fn mime_from_attachment_path(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

fn attachment_info_from_path(
    path: &Path,
    mime_override: Option<&str>,
    include_preview: bool,
) -> Result<SessionAttachmentInfo, String> {
    let metadata = fs::metadata(path)
        .map_err(|err| format!("failed to inspect attachment {}: {err}", path.display()))?;
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| format!("invalid attachment path: {}", path.display()))?;
    let mime = mime_override
        .map(ToOwned::to_owned)
        .or_else(|| mime_from_attachment_path(path).map(ToOwned::to_owned))
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let created_at = metadata
        .created()
        .or_else(|_| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or_else(unix_timestamp);
    let preview_data_url = if include_preview && metadata.len() <= MAX_ATTACHMENT_PREVIEW_BYTES {
        fs::read(path).ok().map(|bytes| {
            format!(
                "data:{};base64,{}",
                mime,
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )
        })
    } else {
        None
    };

    Ok(SessionAttachmentInfo {
        id: filename.clone(),
        filename,
        path: path.display().to_string(),
        mime,
        size_bytes: metadata.len(),
        created_at,
        preview_data_url,
    })
}

fn list_session_attachment_infos(
    cwd: &str,
    session_id: &str,
    include_preview: bool,
) -> Result<Vec<SessionAttachmentInfo>, String> {
    let dir = session_attachment_dir(cwd, session_id)?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(&dir).map_err(|err| {
        format!(
            "failed to read attachment directory {}: {err}",
            dir.display()
        )
    })?;
    let mut attachments = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && mime_from_attachment_path(path).is_some())
        .filter_map(|path| attachment_info_from_path(&path, None, include_preview).ok())
        .collect::<Vec<_>>();
    attachments.sort_by_key(|attachment| attachment.created_at);
    Ok(attachments)
}

fn persist_session_meta(meta: &SessionMeta) -> Result<(), String> {
    let dir = session_dir(&meta.id)?;
    fs::create_dir_all(&dir).map_err(|err| {
        format!(
            "failed to create session directory {}: {err}",
            dir.display()
        )
    })?;
    let path = dir.join("meta.json");
    let payload = serde_json::to_vec_pretty(meta)
        .map_err(|err| format!("failed to encode session metadata: {err}"))?;
    fs::write(&path, payload)
        .map_err(|err| format!("failed to write session metadata {}: {err}", path.display()))
}

fn load_session_meta(session_id: &str) -> Result<SessionMeta, String> {
    let path = session_meta_path(session_id)?;
    let payload = fs::read(&path)
        .map_err(|err| format!("failed to read session metadata {}: {err}", path.display()))?;
    serde_json::from_slice(&payload)
        .map_err(|err| format!("failed to parse session metadata {}: {err}", path.display()))
}

fn load_all_session_metas() -> Vec<SessionMeta> {
    let Ok(dir) = waypoint_sessions_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("meta.json"))
        .filter_map(|path| {
            let payload = fs::read(&path).ok()?;
            serde_json::from_slice::<SessionMeta>(&payload).ok()
        })
        .collect()
}

fn open_transcript_append(path: &Path) -> Result<File, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed to create transcript directory {}: {err}",
                parent.display()
            )
        })?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| format!("failed to open transcript {}: {err}", path.display()))
}

fn read_persisted_replay(session_id: &str) -> Result<Vec<u8>, String> {
    let path = session_transcript_path(session_id)?;
    let bytes = fs::read(&path).unwrap_or_default();
    if bytes.len() <= PERSISTED_REPLAY_LIMIT_BYTES {
        return Ok(bytes);
    }
    Ok(bytes[bytes.len() - PERSISTED_REPLAY_LIMIT_BYTES..].to_vec())
}

fn agent_definitions() -> Vec<AgentDefinition> {
    vec![
        AgentDefinition {
            id: "claude-code",
            name: "Claude Code",
            description: "Anthropic Claude Code CLI",
            candidates: &[CommandCandidate {
                executable: "claude",
                args: &[],
                display: "claude",
                verify: VerifyStrategy::CommandExists,
            }],
        },
        AgentDefinition {
            id: "codex",
            name: "Codex",
            description: "OpenAI Codex CLI",
            candidates: &[CommandCandidate {
                executable: "codex",
                args: &["--no-alt-screen"],
                display: "codex --no-alt-screen",
                verify: VerifyStrategy::CommandExists,
            }],
        },
        AgentDefinition {
            id: "agy",
            name: "Antigravity CLI",
            description: "Google Antigravity CLI",
            candidates: &[CommandCandidate {
                executable: "agy",
                args: &[],
                display: "agy",
                verify: VerifyStrategy::CommandExists,
            }],
        },
        AgentDefinition {
            id: "copilot",
            name: "GitHub Copilot",
            description: "GitHub Copilot CLI",
            candidates: &[
                CommandCandidate {
                    executable: "copilot",
                    args: &[],
                    display: "copilot",
                    verify: VerifyStrategy::CommandExists,
                },
                CommandCandidate {
                    executable: "gh",
                    args: &["copilot"],
                    display: "gh copilot",
                    verify: VerifyStrategy::ShellHelp("gh copilot --help"),
                },
            ],
        },
    ]
}

fn resolve_agent_command(definition: &AgentDefinition) -> Option<ResolvedAgentCommand> {
    definition.candidates.iter().find_map(resolve_candidate)
}

fn resolve_candidate(candidate: &CommandCandidate) -> Option<ResolvedAgentCommand> {
    let executable_path = resolve_executable(candidate.executable)?;
    let verified = match candidate.verify {
        VerifyStrategy::CommandExists => true,
        VerifyStrategy::ShellHelp(command) => {
            run_login_shell_status(&format!("{} >/dev/null 2>&1", command))
        }
    };
    if !verified {
        return None;
    }

    let args = candidate
        .args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let resolved_display = std::iter::once(executable_path.clone())
        .chain(args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");

    Some(ResolvedAgentCommand {
        executable: executable_path,
        args,
        display: candidate.display.to_string(),
        resolved_display,
    })
}

fn append_copilot_cli_option(args: &mut Vec<String>, value: String) {
    if args.first().map(|arg| arg.as_str()) == Some("copilot")
        && !args.iter().any(|arg| arg == "--")
    {
        args.push("--".to_string());
    }
    args.push(value);
}

fn copilot_args_have_session_identity(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg == "--continue"
            || arg == "--resume"
            || arg.starts_with("--resume=")
            || arg == "-r"
            || arg.starts_with("-r=")
            || arg == "--session-id"
            || arg.starts_with("--session-id=")
    })
}

fn native_resume_command_for(meta: &SessionMeta) -> Result<Option<NativeResumeCommand>, String> {
    let definition = agent_definitions()
        .into_iter()
        .find(|definition| definition.id == meta.agent_id)
        .ok_or_else(|| format!("unknown agent preset: {}", meta.agent_id))?;
    let resolved = resolve_agent_command(&definition).ok_or_else(|| {
        format!(
            "{} is not available in PATH. Install it or make sure your login shell can resolve it.",
            definition.name
        )
    })?;
    let now = unix_timestamp();
    let native_id = meta
        .native_session_ref
        .as_ref()
        .and_then(|session_ref| session_ref.id.clone())
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());

    let mut resolved_native_id = native_id.clone();
    let mut resolved_project = meta
        .native_session_ref
        .as_ref()
        .and_then(|session_ref| session_ref.project.clone());

    let (args, display_command) = match meta.agent_id.as_str() {
        "claude-code" => {
            let Some(native_id) = native_id else {
                return Ok(None);
            };
            if find_claude_native_transcript_path(&meta.cwd, &native_id).is_none() {
                return Ok(None);
            }
            let mut args = resolved.args;
            // Claude Code's --resume takes an OPTIONAL value (`-r, --resume [value]`
            // in `claude --help`). With clap, optional-value flags must use the
            // `--resume=<id>` form; the space form `--resume <id>` is parsed as
            // `--resume` (open picker) plus `<id>` as a positional prompt, which
            // is why resuming a specific session used to pop the picker.
            args.push(format!("--resume={native_id}"));
            if meta.dangerous {
                apply_dangerous_flag("claude-code", &mut args);
            }
            (
                args,
                format!("{} --resume={}", resolved.display, shell_quote(&native_id)),
            )
        }
        "codex" => {
            let mut args = resolved.args;
            args.push("resume".to_string());
            if let Some(ref native_id) = native_id {
                args.push(native_id.clone());
            } else {
                args.push("--last".to_string());
            }
            if meta.dangerous {
                apply_dangerous_flag("codex", &mut args);
            }
            let display = if let Some(ref id) = native_id {
                format!("{} resume {}", resolved.display, shell_quote(id))
            } else {
                format!("{} resume --last", resolved.display)
            };
            (args, display)
        }
        "agy" => {
            let mut agy_ref = match native_id {
                Some(id) => {
                    let project = meta
                        .native_session_ref
                        .as_ref()
                        .and_then(|session_ref| session_ref.project.clone())
                        .or_else(|| {
                            meta.native_session_ref
                                .as_ref()
                                .and_then(|session_ref| session_ref.resume_command.as_deref())
                                .and_then(parse_agy_resume_ref)
                                .and_then(|resume_ref| resume_ref.project)
                        });
                    AgyResumeRef {
                        conversation_id: id,
                        project,
                    }
                }
                None => {
                    if let Some(resume_ref) = resolve_agy_resume_ref(&meta.id) {
                        resume_ref
                    } else {
                        return Ok(None);
                    }
                }
            };
            if agy_ref.project.is_none() {
                agy_ref.project = find_agy_resume_ref_in_waypoint_transcript(&meta.id)
                    .and_then(|resume_ref| resume_ref.project);
            }
            resolved_native_id = Some(agy_ref.conversation_id.clone());
            resolved_project = agy_ref.project.clone();
            let mut args = resolved.args;
            args.push(format!("--conversation={}", agy_ref.conversation_id));
            if let Some(project) = agy_ref.project.as_ref() {
                args.push(format!("--project={project}"));
            }
            let display_command = format_agy_resume_command(&resolved.display, &agy_ref);
            (args, display_command)
        }
        "copilot" => {
            let Some(native_id) = native_id else {
                return Ok(None);
            };
            let mut args = resolved.args;
            append_copilot_cli_option(&mut args, format!("--resume={native_id}"));
            (
                args,
                format!("{} --resume={}", resolved.display, shell_quote(&native_id)),
            )
        }
        _ => return Ok(None),
    };

    Ok(Some(NativeResumeCommand {
        executable: resolved.executable,
        args,
        display_command: display_command.clone(),
        native_session_ref: Some(NativeSessionRef {
            provider: meta.agent_id.clone(),
            id: resolved_native_id,
            name: meta
                .native_session_ref
                .as_ref()
                .and_then(|session_ref| session_ref.name.clone()),
            project: resolved_project,
            resume_command: Some(display_command),
            discovered_at: now,
        }),
    }))
}

fn resolve_executable(executable: &str) -> Option<String> {
    let command = format!("command -v {}", shell_quote(executable));
    let output = run_login_shell_output(&command)?;
    let path = output.lines().next()?.trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn run_login_shell_output(command: &str) -> Option<String> {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let output = Command::new(shell).arg("-lc").arg(command).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn run_login_shell_status(command: &str) -> bool {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    Command::new(shell)
        .arg("-lc")
        .arg(command)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Capture the user's full login-shell environment.
///
/// GUI-launched apps (DMG install, Finder, launchd) inherit a minimal PATH
/// (`/usr/bin:/bin:/usr/sbin:/sbin`) and miss homebrew / nvm / `~/.local/bin`.
/// On top of that, `portable-pty`'s `CommandBuilder` calls `env_clear()` when
/// spawning, so the child PTY process would otherwise have no PATH/HOME at
/// all. Agent CLIs (claude/codex) resolve their own absolute path via
/// `command -v`, but the MCP servers they spawn (`npx`, `uvx`, `node`) rely
/// on PATH lookup and fail without this.
fn login_shell_env() -> &'static [(String, String)] {
    static CACHE: OnceLock<Vec<(String, String)>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let output = Command::new(&shell).arg("-lc").arg("env").output().ok();
        let Some(output) = output else {
            return Vec::new();
        };
        let mut vars: Vec<(String, String)> = Vec::new();
        for line in output.stdout.lines().flatten() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.is_empty() || key.contains('\0') {
                continue;
            }
            vars.push((key.to_string(), value.to_string()));
        }
        vars
    })
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

struct HandoverPaths {
    main_path: PathBuf,
    evidence_path: PathBuf,
}

struct WrittenHandover {
    prompt: String,
    main_path: PathBuf,
    evidence_path: Option<PathBuf>,
    effective_mode: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EffectiveHandoverMode {
    Compact,
    Full,
}

impl EffectiveHandoverMode {
    fn as_str(self) -> &'static str {
        match self {
            EffectiveHandoverMode::Compact => "compact",
            EffectiveHandoverMode::Full => "full",
        }
    }
}

fn resolve_handover_mode(
    requested_mode: HandoverContentMode,
    is_large: bool,
) -> EffectiveHandoverMode {
    match requested_mode {
        HandoverContentMode::Recommended if is_large => EffectiveHandoverMode::Compact,
        HandoverContentMode::Recommended => EffectiveHandoverMode::Full,
        HandoverContentMode::Compact => EffectiveHandoverMode::Compact,
        HandoverContentMode::Full => EffectiveHandoverMode::Full,
    }
}

fn planned_handover_target_info(
    agent_id: &str,
    cwd: &str,
    dangerous: bool,
    none_workspace: bool,
) -> Result<SessionInfo, String> {
    let definition = agent_definitions()
        .into_iter()
        .find(|definition| definition.id == agent_id)
        .ok_or_else(|| format!("unknown agent preset: {agent_id}"))?;
    let now = unix_timestamp();
    let command = match definition.id {
        "claude-code" => "claude <handover>".to_string(),
        "agy" => "agy --prompt-interactive <handover>".to_string(),
        "codex" => "codex --no-alt-screen <handover>".to_string(),
        "copilot" => resolve_agent_command(&definition)
            .map(|resolved| format!("{} -i <handover>", resolved.display))
            .unwrap_or_else(|| "copilot -i <handover>".to_string()),
        _ => format!("{} <handover>", definition.name),
    };

    Ok(SessionInfo {
        id: "pending".to_string(),
        agent_id: definition.id.to_string(),
        agent_name: definition.name.to_string(),
        title: format!("{} new session", definition.name),
        command,
        cwd: cwd.to_string(),
        status: SessionStatus::Running,
        attached: false,
        created_at: now,
        last_active_at: now,
        first_user_message: None,
        native_session_ref: None,
        parent_session_id: None,
        handover_root_id: None,
        dangerous,
        none_workspace,
    })
}

fn planned_file_handover_target_info(source: &SessionInfo) -> SessionInfo {
    SessionInfo {
        id: "file".to_string(),
        agent_id: "manual".to_string(),
        agent_name: "Manual handover".to_string(),
        title: "Handover file".to_string(),
        command: "handover file".to_string(),
        cwd: source.cwd.clone(),
        status: SessionStatus::Running,
        attached: false,
        created_at: unix_timestamp(),
        last_active_at: unix_timestamp(),
        first_user_message: None,
        native_session_ref: None,
        parent_session_id: None,
        handover_root_id: None,
        dangerous: source.dangerous,
        none_workspace: source.none_workspace,
    }
}

fn reserve_handover_paths(cwd: &str) -> Result<HandoverPaths, String> {
    let dir = handover_workspace_dir(cwd)?;
    fs::create_dir_all(&dir).map_err(|err| {
        format!(
            "failed to create handover directory {}: {err}",
            dir.display()
        )
    })?;
    let id = Uuid::new_v4();
    Ok(HandoverPaths {
        main_path: dir.join(format!("handover-{id}.md")),
        evidence_path: dir.join(format!("handover-{id}-full-evidence.md")),
    })
}

fn write_handover_files(
    paths: &HandoverPaths,
    prompt: &str,
    evidence: Option<&str>,
) -> Result<(), String> {
    if let Some(evidence) = evidence {
        fs::write(&paths.evidence_path, evidence).map_err(|err| {
            format!(
                "failed to write handover evidence file {}: {err}",
                paths.evidence_path.display()
            )
        })?;
    }
    fs::write(&paths.main_path, prompt).map_err(|err| {
        format!(
            "failed to write handover file {}: {err}",
            paths.main_path.display()
        )
    })?;
    Ok(())
}

fn handover_workspace_dir(cwd: &str) -> Result<PathBuf, String> {
    let workspace_name = Path::new(cwd)
        .file_name()
        .map(|name| name.to_string_lossy().trim().to_string())
        .filter(|name| !name.is_empty() && name != "." && name != "..")
        .unwrap_or_else(|| "workspace".to_string());

    Ok(waypoint_root_dir()?.join(workspace_name))
}

fn handover_reference_startup_prompt(path: &Path, session_id: &str) -> String {
    format!(
        "Initialization step for this new session: read only this exact handover file now: {}. This single-file read is explicitly allowed. Do not list/search directories, do not use glob patterns, and do not read any other files during this initialization turn. After loading that single file, reply exactly: \"Context loaded. Waiting for your instruction.\" and wait for the next user message. Crucially, this constraint applies ONLY to this first startup turn; in all subsequent turns, you must fully use your normal tools, file reading, and directory search capabilities to assist the user.\n<!-- waypoint_session_id: {} -->",
        path.display(),
        session_id
    )
}

fn handover_startup_delay_ms(agent_id: &str) -> u64 {
    match agent_id {
        "codex" => CODEX_HANDOVER_STARTUP_DELAY_MS,
        _ => HANDOVER_INJECT_DELAY_MS,
    }
}

fn build_handover_prompt(
    source: &SessionInfo,
    target: &SessionInfo,
    note: &str,
    inherited_handover: &str,
    recent_context: &str,
    recent_user_inputs: &str,
    artifacts: &str,
) -> String {
    let artifacts_section = if artifacts.trim().is_empty() {
        String::new()
    } else {
        format!("{}\n\n", artifacts.trim())
    };
    let conversation_timeline =
        format_ordered_conversation_timeline(recent_context, recent_user_inputs);
    format!(
        r#"# Waypoint Handover

You are continuing work from another local agent session inside waypoint.

## Summary
Continuation from {source_agent} session "{source_title}" in `{source_cwd}`.
No semantic summary was generated; use the user note and evidence below.

## Source Session
- Agent: {source_agent}
- Title: {source_title}
- Command: {source_command}
- Workspace: {source_cwd}

## Target Session
- Agent: {target_agent}
- Title: {target_title}
- Command: {target_command}
- Workspace: {target_cwd}

## User Note
{note}

{artifacts_section}## Inherited Handover Context
{inherited_handover}

## Evidence

### Recent Conversation Timeline (ordered)
```text
{conversation_timeline}
```

## Recommended Next Steps
- Start from the user note and recent conversation.
- Inspect the workspace git state directly if it matters for the task.
- Continue or rerun the most relevant validation for the task.

## Instructions
- This file is a context snapshot from a previous agent session.
- Use it to preserve continuity with the current workspace state.
- Do not treat this file as a standing instruction to pause or re-initialize on every turn.
- Do not revert unrelated user changes.
- Preserve existing user edits.
- Ask before destructive operations.
"#,
        source_agent = source.agent_name,
        source_title = source.title,
        source_command = source.command,
        source_cwd = source.cwd,
        target_agent = target.agent_name,
        target_title = target.title,
        target_command = target.command,
        target_cwd = target.cwd,
        note = if note.trim().is_empty() {
            "No additional note."
        } else {
            note.trim()
        },
        inherited_handover = empty_fallback(inherited_handover, "No inherited handover context."),
        conversation_timeline = conversation_timeline,
    )
}

fn build_compact_handover_prompt(
    source: &SessionInfo,
    target: &SessionInfo,
    note: &str,
    inherited_handover: &str,
    recent_context: &str,
    recent_user_inputs: &str,
    evidence_path: Option<&str>,
    artifacts: &str,
) -> String {
    let artifacts_section = if artifacts.trim().is_empty() {
        String::new()
    } else {
        format!("{}\n\n", artifacts.trim())
    };
    let conversation_timeline =
        format_ordered_conversation_timeline(recent_context, recent_user_inputs);
    format!(
        r#"# Waypoint Handover

Continue from the previous local agent session.

## Summary
Continuation from {source_agent} session "{source_title}" in `{source_cwd}`.
This compact handover includes short recent evidence. Inspect workspace state directly if needed.

## Source
- Agent: {source_agent}
- Title: {source_title}
- Workspace: {source_cwd}

## Target
- Agent: {target_agent}
- Workspace: {target_cwd}

## User Note
{note}

{artifacts_section}## Inherited Handover Context
{inherited_handover}

## Full Evidence
{full_evidence}

## Evidence

### Recent Conversation Timeline (ordered)
```text
{conversation_timeline}
```

## Recommended Next Steps
- Start from the user note and recent conversation.
- Inspect the workspace git state directly if it matters for the task.
- Continue or rerun the most relevant validation for the task.

## Instructions
- This file is a context snapshot from a previous agent session.
- Use it to preserve continuity with the current workspace state.
- Do not treat this file as a standing instruction to pause or re-initialize on every turn.
- Do not revert unrelated user changes.
"#,
        source_agent = source.agent_name,
        source_title = source.title,
        source_cwd = source.cwd,
        target_agent = target.agent_name,
        target_cwd = target.cwd,
        note = if note.trim().is_empty() {
            "No additional note."
        } else {
            note.trim()
        },
        full_evidence = format_full_evidence_reference(evidence_path),
        inherited_handover = empty_fallback(inherited_handover, "No inherited handover context."),
        conversation_timeline = conversation_timeline,
    )
}

fn build_full_handover_evidence(
    source: &SessionInfo,
    target: &SessionInfo,
    note: &str,
    inherited_handover: &str,
    recent_context: &str,
    recent_user_inputs: &str,
    artifacts: &str,
) -> String {
    let artifacts_section = if artifacts.trim().is_empty() {
        String::new()
    } else {
        format!("{}\n\n", artifacts.trim())
    };
    let conversation_timeline =
        format_ordered_conversation_timeline(recent_context, recent_user_inputs);
    format!(
        r#"# Waypoint Full Handover Evidence

This file contains larger raw evidence for a compact handover. The target agent should read it only if the main handover is insufficient.

## Source Session
- Agent: {source_agent}
- Title: {source_title}
- Command: {source_command}
- Workspace: {source_cwd}

## Target Session
- Agent: {target_agent}
- Title: {target_title}
- Command: {target_command}
- Workspace: {target_cwd}

## User Note
{note}

{artifacts_section}## Inherited Handover Context
{inherited_handover}

## Recent Conversation Timeline (ordered)
```text
{conversation_timeline}
```
"#,
        source_agent = source.agent_name,
        source_title = source.title,
        source_command = source.command,
        source_cwd = source.cwd,
        target_agent = target.agent_name,
        target_title = target.title,
        target_command = target.command,
        target_cwd = target.cwd,
        note = if note.trim().is_empty() {
            "No additional note."
        } else {
            note.trim()
        },
        inherited_handover = empty_fallback(inherited_handover, "No inherited handover context."),
        conversation_timeline = conversation_timeline,
    )
}

fn format_ordered_conversation_timeline(recent_context: &str, recent_user_inputs: &str) -> String {
    let recent_context = recent_context.trim();
    if !recent_context.is_empty() {
        return recent_context.to_string();
    }

    let recent_user_inputs = recent_user_inputs.trim();
    if !recent_user_inputs.is_empty() {
        return format!(
            "Only user inputs were captured; assistant replies were not available in order.\n\n{recent_user_inputs}"
        );
    }

    "No ordered conversation captured.".to_string()
}

fn format_full_evidence_reference(evidence_path: Option<&str>) -> String {
    match evidence_path {
        Some(path) => format!(
            "A larger evidence file was preserved at `{path}`. Read only this exact file if the compact handover is insufficient; do not scan the handover directory."
        ),
        None => "No separate full evidence file was generated for this handover.".to_string(),
    }
}

fn format_handover_for_inheritance(prompt: &str, limit: usize) -> String {
    let mut parts = Vec::new();

    if let Some(earlier) = extract_first_markdown_section(
        prompt,
        &["## Inherited Handover Context", "## Prior Handover Context"],
    )
    .filter(|content| is_meaningful_inherited_context(content))
    {
        parts.push(format!(
            "### Earlier Handover Context\n{}",
            dedupe_repeated_handover_lines(&earlier)
        ));
    }

    let mut hop_parts = Vec::new();
    push_inheritance_section(
        &mut hop_parts,
        "#### Summary",
        extract_first_markdown_section(prompt, &["## Summary"]),
    );
    push_inheritance_section(
        &mut hop_parts,
        "#### Source",
        extract_first_markdown_section(prompt, &["## Source Session", "## Source"]),
    );
    push_inheritance_section(
        &mut hop_parts,
        "#### Target",
        extract_first_markdown_section(prompt, &["## Target Session", "## Target"]),
    );
    push_inheritance_section(
        &mut hop_parts,
        "#### User Note",
        extract_first_markdown_section(prompt, &["## User Note"]),
    );
    if !hop_parts.is_empty() {
        parts.push(format!(
            "### Previous Handover Hop\n{}",
            hop_parts.join("\n\n")
        ));
    }

    let mut evidence_parts = Vec::new();
    push_inheritance_section(
        &mut evidence_parts,
        "#### Recent Conversation Timeline",
        extract_first_markdown_section(
            prompt,
            &[
                "### Recent Conversation Timeline (ordered)",
                "## Recent Conversation Timeline (ordered)",
                "### Recent Source Terminal Context",
                "### Recent Source Context",
                "## Recent Source Terminal Context",
            ],
        )
        .map(|content| dedupe_repeated_handover_lines(&content)),
    );

    if !evidence_parts.is_empty() {
        parts.push(format!(
            "### Previous Handover Evidence\n{}",
            evidence_parts.join("\n\n")
        ));
    }

    let snapshot = parts.join("\n\n");
    if snapshot.trim().is_empty() {
        return "No inherited handover context.".to_string();
    }
    tail_chars(&snapshot, limit)
}

fn push_inheritance_section(parts: &mut Vec<String>, heading: &str, content: Option<String>) {
    let Some(content) = content else {
        return;
    };
    let content = content.trim();
    if content.is_empty() {
        return;
    }
    parts.push(format!("{heading}\n{content}"));
}

fn extract_first_markdown_section(markdown: &str, headings: &[&str]) -> Option<String> {
    headings
        .iter()
        .find_map(|heading| extract_markdown_section(markdown, heading))
}

fn extract_markdown_section(markdown: &str, heading: &str) -> Option<String> {
    let target = heading.trim();
    let heading_level = target.chars().take_while(|c| *c == '#').count();
    if heading_level == 0 {
        return None;
    }

    let mut capturing = false;
    let mut lines = Vec::new();
    for line in markdown.lines() {
        if !capturing {
            if line.trim() == target {
                capturing = true;
            }
            continue;
        }

        if is_markdown_heading_at_or_above(line, heading_level) {
            break;
        }
        lines.push(line);
    }

    let content = lines.join("\n").trim().to_string();
    if content.is_empty() {
        None
    } else {
        Some(content)
    }
}

fn is_markdown_heading_at_or_above(line: &str, level: usize) -> bool {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > level {
        return false;
    }
    trimmed.chars().nth(hashes) == Some(' ')
}

fn is_meaningful_inherited_context(content: &str) -> bool {
    let trimmed = content.trim();
    !trimmed.is_empty() && trimmed != "No inherited handover context."
}

fn inject_with_retry(target: &Arc<PtySession>, prompt: &str) -> Result<(), String> {
    let injection = format!("\x1b[200~{prompt}\x1b[201~\n");
    let mut last_error = None;

    for attempt in 1..=HANDOVER_INJECT_ATTEMPTS {
        if let Some(exit_status) = target
            .child
            .lock()
            .try_wait()
            .map_err(|err| format!("failed to inspect target session: {err}"))?
        {
            target.mark_status(SessionStatus::Exited);
            return Err(format!(
                "target session exited before handover could be injected: {exit_status}. Recent output:\n{}",
                tail_chars(&target.ring.lock(), 4000)
            ));
        }

        match target.writer.lock().write_all(injection.as_bytes()) {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_error = Some(err.to_string());
                if attempt < HANDOVER_INJECT_ATTEMPTS {
                    thread::sleep(Duration::from_millis(HANDOVER_INJECT_DELAY_MS));
                }
            }
        }
    }

    Err(format!(
        "failed to write handover to target session after {HANDOVER_INJECT_ATTEMPTS} attempts: {}. Recent target output:\n{}",
        last_error.unwrap_or_else(|| "unknown write error".to_string()),
        tail_chars(&target.ring.lock(), 4000)
    ))
}

fn trim_chat_messages(messages: &mut Vec<ChatMessage>, open_assistant_index: &mut Option<usize>) {
    if messages.len() <= CHAT_HISTORY_LIMIT {
        return;
    }

    let drop_count = messages.len() - CHAT_HISTORY_LIMIT;
    messages.drain(0..drop_count);

    if let Some(index) = *open_assistant_index {
        *open_assistant_index = index.checked_sub(drop_count);
    }
}

fn build_handover_source_context(
    source: &Arc<PtySession>,
    source_info: &SessionInfo,
    limit: usize,
) -> String {
    if let Some(context) = build_native_source_context(source_info, limit) {
        return context;
    }

    build_recent_source_context(source, limit)
}

fn build_handover_user_inputs(
    source: &Arc<PtySession>,
    source_info: &SessionInfo,
    limit: usize,
) -> String {
    if let Some(inputs) = build_native_user_inputs(source_info, limit) {
        return inputs;
    }

    build_recent_user_inputs(source, limit)
}

fn build_recent_source_context(source: &Arc<PtySession>, limit: usize) -> String {
    source.finalize_open_assistant_message_if_idle(CHAT_STREAM_IDLE_FINALIZE_MS);
    let messages = source.chat_messages.lock().clone();
    let conversation = format_chat_messages_for_handover(&messages, limit);
    if !conversation.trim().is_empty() {
        return conversation;
    }

    clean_terminal_output_minimal(&source.ring.lock(), limit)
}

fn build_recent_user_inputs(source: &Arc<PtySession>, limit: usize) -> String {
    let terminal_inputs = clean_terminal_input(&source.input_ring.lock(), limit);
    if !terminal_inputs.trim().is_empty() {
        return terminal_inputs;
    }

    let recent_output = clean_terminal_output_minimal(&source.ring.lock(), limit);
    extract_user_prompts_from_terminal_context(&recent_output, limit)
}

fn build_native_source_context(source_info: &SessionInfo, limit: usize) -> Option<String> {
    let messages = native_transcript_messages_for(source_info)?;
    let context = format_native_transcript_context(&messages, limit);
    if context.trim().is_empty() {
        None
    } else {
        Some(context)
    }
}

fn build_native_user_inputs(source_info: &SessionInfo, limit: usize) -> Option<String> {
    let messages = native_transcript_messages_for(source_info)?;
    let inputs = format_native_user_inputs(&messages, limit);
    if inputs.trim().is_empty() {
        None
    } else {
        Some(inputs)
    }
}

fn native_transcript_messages_for(
    source_info: &SessionInfo,
) -> Option<Vec<NativeTranscriptMessage>> {
    let kind = native_transcript_kind(source_info)?;
    let path = native_transcript_path(source_info, kind)?;
    let file = File::open(path).ok()?;
    let messages = parse_native_transcript_messages(BufReader::new(file), kind);
    if messages.is_empty() {
        None
    } else {
        Some(messages)
    }
}

#[derive(Clone, Copy)]
enum NativeTranscriptKind {
    Claude,
    Codex,
    Agy,
}

struct NativeTranscriptMessage {
    role: ChatRole,
    content: String,
}

fn native_transcript_kind(source_info: &SessionInfo) -> Option<NativeTranscriptKind> {
    match source_info.agent_id.as_str() {
        "claude-code" => Some(NativeTranscriptKind::Claude),
        "codex" => Some(NativeTranscriptKind::Codex),
        "agy" => Some(NativeTranscriptKind::Agy),
        _ => match source_info.native_session_ref.as_ref()?.provider.as_str() {
            "claude-code" | "claude" => Some(NativeTranscriptKind::Claude),
            "codex" => Some(NativeTranscriptKind::Codex),
            "agy" => Some(NativeTranscriptKind::Agy),
            _ => None,
        },
    }
}

fn native_transcript_path(
    source_info: &SessionInfo,
    kind: NativeTranscriptKind,
) -> Option<PathBuf> {
    let native_id = source_info
        .native_session_ref
        .as_ref()
        .and_then(|session_ref| session_ref.id.as_deref())
        .unwrap_or_default()
        .trim();

    match kind {
        NativeTranscriptKind::Claude => {
            if native_id.is_empty() {
                None
            } else {
                find_claude_native_transcript_path(&source_info.cwd, native_id)
            }
        }
        NativeTranscriptKind::Codex => {
            if !native_id.is_empty() {
                find_codex_native_transcript_path(native_id)
            } else {
                find_codex_session_by_marker(&source_info.id)
                    .or_else(|| find_latest_codex_native_transcript_path(&source_info.cwd, source_info.created_at))
            }
        }
        NativeTranscriptKind::Agy => {
            let agy_id = if native_id.is_empty() {
                resolve_agy_resume_ref(&source_info.id)?.conversation_id
            } else {
                native_id.to_string()
            };
            find_agy_native_transcript_path(&agy_id)
        }
    }
}

fn find_agy_conversation_id(session_id: &str) -> Option<String> {
    let home = home_dir()?;
    let brain_dir = home.join(".gemini").join("antigravity-cli").join("brain");
    if !brain_dir.is_dir() {
        return None;
    }
    let pattern = format!("waypoint_session_id: {session_id}");
    let entries = fs::read_dir(brain_dir).ok()?;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            let transcript_path = path
                .join(".system_generated")
                .join("logs")
                .join("transcript.jsonl");
            if transcript_path.is_file() {
                if let Ok(content) = fs::read_to_string(&transcript_path) {
                    if content.contains(&pattern) {
                        return path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned());
                    }
                }
            }
        }
    }
    None
}

fn resolve_agy_resume_ref(session_id: &str) -> Option<AgyResumeRef> {
    let transcript_ref = find_agy_resume_ref_in_waypoint_transcript(session_id);
    if let Some(conversation_id) = find_agy_conversation_id(session_id) {
        let project = transcript_ref
            .as_ref()
            .filter(|resume_ref| resume_ref.conversation_id == conversation_id)
            .and_then(|resume_ref| resume_ref.project.clone());
        return Some(AgyResumeRef {
            conversation_id,
            project,
        });
    }
    transcript_ref
}

fn find_agy_resume_ref_in_waypoint_transcript(session_id: &str) -> Option<AgyResumeRef> {
    let path = session_transcript_path(session_id).ok()?;
    let content = fs::read_to_string(path).ok()?;
    parse_agy_resume_ref(&content)
}

fn parse_agy_resume_ref(text: &str) -> Option<AgyResumeRef> {
    let cleaned = strip_ansi_escape_sequences(text);
    let lines = cleaned.lines().collect::<Vec<_>>();

    lines
        .iter()
        .rev()
        .filter(|line| line.contains("Resume in the same project"))
        .find_map(|line| parse_agy_resume_ref_line(line))
        .or_else(|| {
            lines
                .iter()
                .rev()
                .find_map(|line| parse_agy_resume_ref_line(line))
        })
}

fn parse_agy_resume_ref_line(line: &str) -> Option<AgyResumeRef> {
    if !line.contains("agy") || !line.contains("--conversation") {
        return None;
    }

    let tokens = line.split_whitespace().collect::<Vec<_>>();
    let mut conversation_id = None;
    let mut project = None;
    let mut index = 0;
    while index < tokens.len() {
        let token = clean_cli_token(tokens[index]);
        if token == "--conversation" {
            conversation_id = tokens
                .get(index + 1)
                .and_then(|value| clean_cli_value(value));
            index += 2;
            continue;
        }
        if token == "--project" {
            project = tokens
                .get(index + 1)
                .and_then(|value| clean_cli_value(value));
            index += 2;
            continue;
        }
        if let Some(value) = token.strip_prefix("--conversation=") {
            conversation_id = clean_cli_value(value);
        } else if let Some(value) = token.strip_prefix("--project=") {
            project = clean_cli_value(value);
        }
        index += 1;
    }

    conversation_id.map(|conversation_id| AgyResumeRef {
        conversation_id,
        project,
    })
}

fn clean_cli_token(token: &str) -> String {
    token
        .trim_matches(|ch| matches!(ch, '\'' | '"' | '`' | '(' | ')' | ',' | ';'))
        .to_string()
}

fn clean_cli_value(value: &str) -> Option<String> {
    let value = clean_cli_token(value);
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn strip_ansi_escape_sequences(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            output.push(ch);
            continue;
        }

        if chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        }
    }
    output
}

fn format_agy_resume_command(display: &str, agy_ref: &AgyResumeRef) -> String {
    let mut command = format!(
        "{} --conversation={}",
        display,
        shell_quote(&agy_ref.conversation_id)
    );
    if let Some(project) = agy_ref
        .project
        .as_ref()
        .filter(|project| !project.trim().is_empty())
    {
        command.push_str(&format!(" --project={}", shell_quote(project)));
    }
    command
}

fn find_agy_native_transcript_path(conversation_id: &str) -> Option<PathBuf> {
    let home = home_dir()?;
    let expected = home
        .join(".gemini")
        .join("antigravity-cli")
        .join("brain")
        .join(conversation_id)
        .join(".system_generated")
        .join("logs")
        .join("transcript.jsonl");
    if expected.is_file() {
        Some(expected)
    } else {
        None
    }
}

fn find_claude_native_transcript_path(cwd: &str, native_id: &str) -> Option<PathBuf> {
    let home = home_dir()?;
    let projects_dir = home.join(".claude").join("projects");
    let expected = projects_dir
        .join(claude_project_dir_name(cwd))
        .join(format!("{native_id}.jsonl"));
    if expected.is_file() {
        return Some(expected);
    }

    let mut candidates = Vec::new();
    collect_jsonl_paths(&projects_dir, 3, &mut candidates);
    candidates.into_iter().find(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(|name| name == format!("{native_id}.jsonl"))
            .unwrap_or(false)
    })
}

fn claude_project_dir_name(cwd: &str) -> String {
    let mut name = String::new();
    for c in cwd.chars() {
        if c.is_alphanumeric() {
            name.push(c);
        } else {
            name.push('-');
        }
    }
    name
}

fn append_claude_session_id(args: &mut Vec<String>, session_id: &str) {
    let insert_at = args
        .iter()
        .position(|arg| !arg.starts_with('-'))
        .unwrap_or(args.len());
    args.insert(insert_at, session_id.to_string());
    args.insert(insert_at, "--session-id".to_string());
}

fn apply_dangerous_flag(agent_id: &str, args: &mut Vec<String>) {
    let flag = match agent_id {
        "claude-code" => "--dangerously-skip-permissions",
        "codex" => "--dangerously-bypass-approvals-and-sandbox",
        _ => return,
    };
    if !args.iter().any(|arg| arg == flag) {
        args.insert(0, flag.to_string());
    }
}

fn claude_args_have_session_identity(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg == "--resume"
            || arg.starts_with("--resume=")
            || arg == "-r"
            || arg.starts_with("-r=")
            || arg == "--session-id"
            || arg.starts_with("--session-id=")
    })
}

fn find_codex_session_by_marker(session_id: &str) -> Option<PathBuf> {
    let home = home_dir()?;
    let mut candidates = Vec::new();
    collect_jsonl_paths(&home.join(".codex").join("sessions"), 6, &mut candidates);

    let pattern = format!("waypoint_session_id: {session_id}");
    candidates.into_iter().find(|path| {
        fs::read_to_string(path)
            .map(|content| content.contains(&pattern))
            .unwrap_or(false)
    })
}

fn find_codex_native_transcript_path(native_id: &str) -> Option<PathBuf> {
    let home = home_dir()?;
    let mut candidates = Vec::new();
    collect_jsonl_paths(&home.join(".codex").join("sessions"), 6, &mut candidates);
    collect_jsonl_paths(
        &home.join(".codex").join("archived_sessions"),
        3,
        &mut candidates,
    );

    if let Some(path) = candidates.iter().find(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.contains(native_id))
            .unwrap_or(false)
    }) {
        return Some(path.clone());
    }

    candidates
        .into_iter()
        .find(|path| codex_transcript_has_session_id(path, native_id))
}

fn find_latest_codex_native_transcript_path(cwd: &str, created_at: u64) -> Option<PathBuf> {
    let home = home_dir()?;
    let mut candidates = Vec::new();
    collect_jsonl_paths(&home.join(".codex").join("sessions"), 6, &mut candidates);

    let normalized_cwd = normalize_existing_path(cwd);

    // Use file birth time (stable, reflects session start) rather than modification
    // time (which grows whenever the session sends a new message).  This prevents a
    // newer concurrent session in the same workspace from stealing the transcript of
    // an older session during handover.
    candidates
        .into_iter()
        .filter(|path| codex_transcript_matches_workspace(path, cwd, normalized_cwd.as_deref()))
        .filter_map(|path| {
            // Birth time is stable; fall back to modified time if unavailable.
            let born_at = path_created_secs(&path)
                .or_else(|| path_modified_secs(&path))?;
            // Exclude transcripts that were already old (> 30 s) before this session started.
            if born_at.saturating_add(30) < created_at {
                return None;
            }
            Some((path, born_at))
        })
        .min_by_key(|(_, born_at)| {
            // Select the transcript whose birth time is closest to session creation.
            // Prefer transcripts born just *after* created_at (normal case: codex
            // writes the first event a second or two after the Waypoint session is
            // recorded).  Slightly penalise transcripts born *before* created_at to
            // avoid picking up a pre-existing session that happens to share the cwd.
            if *born_at >= created_at {
                *born_at - created_at
            } else {
                (created_at - *born_at).saturating_mul(2)
            }
        })
        .map(|(path, _)| path)
}

fn codex_transcript_has_session_id(path: &Path, native_id: &str) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    for line in BufReader::new(file).lines().map_while(Result::ok).take(50) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        if value
            .pointer("/payload/id")
            .and_then(Value::as_str)
            .map(|id| id == native_id)
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

fn codex_transcript_matches_workspace(
    path: &Path,
    cwd: &str,
    normalized_cwd: Option<&str>,
) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    for line in BufReader::new(file).lines().map_while(Result::ok).take(50) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        let Some(transcript_cwd) = value.pointer("/payload/cwd").and_then(Value::as_str) else {
            continue;
        };
        if transcript_cwd == cwd {
            return true;
        }
        if let Some(normalized_cwd) = normalized_cwd {
            if normalize_existing_path(transcript_cwd).as_deref() == Some(normalized_cwd) {
                return true;
            }
        }
    }
    false
}

fn normalize_existing_path(path: &str) -> Option<String> {
    fs::canonicalize(path)
        .ok()
        .map(|path| path.to_string_lossy().to_string())
}

fn path_modified_secs(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

/// Returns the file birth (creation) time in seconds since the Unix epoch.
/// On macOS this is the true birth time; falls back to modification time on
/// platforms that don't expose it.
fn path_created_secs(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .created()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn collect_jsonl_paths(root: &Path, max_depth: usize, paths: &mut Vec<PathBuf>) {
    if max_depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_paths(&path, max_depth - 1, paths);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            paths.push(path);
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    env::var("HOME").ok().map(PathBuf::from)
}

fn parse_native_transcript_messages<R: BufRead>(
    reader: R,
    kind: NativeTranscriptKind,
) -> Vec<NativeTranscriptMessage> {
    reader
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter_map(|value| match kind {
            NativeTranscriptKind::Claude => parse_claude_native_message(&value),
            NativeTranscriptKind::Codex => parse_codex_native_message(&value),
            NativeTranscriptKind::Agy => parse_agy_native_message(&value),
        })
        .collect()
}

fn parse_agy_native_message(value: &Value) -> Option<NativeTranscriptMessage> {
    let msg_type = value.get("type").and_then(Value::as_str)?;
    let role = match msg_type {
        "USER_INPUT" => ChatRole::User,
        "PLANNER_RESPONSE" => ChatRole::Assistant,
        _ => return None,
    };
    let content = value.get("content").and_then(Value::as_str)?;
    let content = clean_native_message_content(content, role);
    if content.trim().is_empty() || is_native_system_noise(&content) {
        return None;
    }
    Some(NativeTranscriptMessage { role, content })
}

fn parse_claude_native_message(value: &Value) -> Option<NativeTranscriptMessage> {
    let message = value.get("message")?;
    let role = match message
        .get("role")
        .and_then(Value::as_str)
        .or_else(|| value.get("type").and_then(Value::as_str))?
    {
        "user" => ChatRole::User,
        "assistant" => ChatRole::Assistant,
        _ => return None,
    };
    let raw_content = message.get("content")?;
    let content = match role {
        ChatRole::User => extract_text_parts(raw_content, &["text"]),
        ChatRole::Assistant => extract_text_parts(raw_content, &["text"]),
    };
    let content = clean_native_message_content(&content, role);
    if content.trim().is_empty() || is_native_system_noise(&content) {
        return None;
    }

    Some(NativeTranscriptMessage { role, content })
}

fn parse_codex_native_message(value: &Value) -> Option<NativeTranscriptMessage> {
    if value.get("type").and_then(Value::as_str) != Some("response_item") {
        return None;
    }
    let payload = value.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let role = match payload.get("role").and_then(Value::as_str)? {
        "user" => ChatRole::User,
        "assistant" => ChatRole::Assistant,
        _ => return None,
    };
    let content_value = payload.get("content")?;
    let allowed_types = match role {
        ChatRole::User => &["input_text", "text"][..],
        ChatRole::Assistant => &["output_text", "text"][..],
    };
    let content = extract_text_parts(content_value, allowed_types);
    let content = clean_native_message_content(&content, role);
    if content.trim().is_empty() || is_native_system_noise(&content) {
        return None;
    }

    Some(NativeTranscriptMessage { role, content })
}

fn extract_text_parts(value: &Value, allowed_types: &[&str]) -> String {
    match value {
        Value::String(text) => text.to_string(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| extract_text_part(item, allowed_types))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn extract_text_part(value: &Value, allowed_types: &[&str]) -> Option<String> {
    if let Value::String(text) = value {
        return Some(text.to_string());
    }

    let item_type = value.get("type").and_then(Value::as_str)?;
    if !allowed_types.contains(&item_type) {
        return None;
    }

    value
        .get("text")
        .or_else(|| value.get("content"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn clean_native_message_content(content: &str, _role: ChatRole) -> String {
    let cleaned = clean_handover_text_minimal(content, CHAT_MESSAGE_CONTENT_LIMIT_CHARS);
    collapse_blank_lines(&cleaned, 2).trim().to_string()
}

fn is_native_system_noise(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed.starts_with("<system-reminder>")
        || trimmed.starts_with("<local-command-stdout>")
        || trimmed.starts_with("<local-command-stderr>")
}

fn format_native_transcript_context(messages: &[NativeTranscriptMessage], limit: usize) -> String {
    let blocks = messages
        .iter()
        .map(|message| {
            let role = match message.role {
                ChatRole::User => "User",
                ChatRole::Assistant => "Assistant",
            };
            format!("{role}:\n{}", message.content)
        })
        .collect::<Vec<_>>();
    tail_chars(&blocks.join("\n\n"), limit)
}

fn format_native_user_inputs(messages: &[NativeTranscriptMessage], limit: usize) -> String {
    let inputs = messages
        .iter()
        .filter(|message| message.role == ChatRole::User)
        .map(|message| message.content.clone())
        .collect::<Vec<_>>();
    tail_chars(&inputs.join("\n\n"), limit)
}

fn format_chat_messages_for_handover(messages: &[ChatMessage], limit: usize) -> String {
    let Some(start_index) = messages
        .iter()
        .position(|message| matches!(message.role, ChatRole::User))
    else {
        return String::new();
    };

    let mut blocks = Vec::new();
    for message in messages.iter().skip(start_index) {
        let content = clean_handover_message_content(&message.content, message.role);
        if content.trim().is_empty() {
            continue;
        }

        let role = match message.role {
            ChatRole::User => "User",
            ChatRole::Assistant => "Assistant",
        };
        let pending = if message.pending { " (pending)" } else { "" };
        blocks.push(format!("{role}{pending}:\n{content}"));
    }

    tail_chars(&blocks.join("\n\n"), limit)
}

fn extract_user_prompts_from_terminal_context(context: &str, limit: usize) -> String {
    let prompts = context
        .lines()
        .filter_map(extract_user_prompt_line)
        .collect::<Vec<_>>();
    tail_chars(&prompts.join("\n"), limit)
}

fn extract_user_prompt_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let content = trimmed
        .strip_prefix('›')
        .or_else(|| trimmed.strip_prefix('❯'))
        .or_else(|| trimmed.strip_prefix("Human:"))
        .or_else(|| trimmed.strip_prefix("User:"))?
        .trim();
    if content.is_empty() || looks_like_tui_noise_line(content) {
        return None;
    }
    Some(content.to_string())
}

fn clean_handover_message_content(content: &str, _role: ChatRole) -> String {
    clean_handover_text_minimal(content, CHAT_MESSAGE_CONTENT_LIMIT_CHARS)
}

fn clean_handover_text_minimal(content: &str, limit: usize) -> String {
    let stripped = strip_orphan_ansi_fragments(&strip_ansi(content));
    let normalized = stripped.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(normalized.len());

    for ch in normalized.chars() {
        match ch {
            '\n' | '\t' => out.push(ch),
            '\x08' | '\x7f' => {
                out.pop();
            }
            c if !c.is_control() => out.push(c),
            _ => {}
        }
    }

    let collapsed = collapse_blank_lines(&out, 2).trim().to_string();
    tail_chars(&collapsed, limit)
}

fn extract_submitted_user_inputs(pending: &mut String, data: &str) -> Vec<String> {
    let mut submitted = Vec::new();
    let mut ignoring_escape = false;

    for ch in data.chars() {
        if ignoring_escape {
            if ch.is_ascii_alphabetic() || ch == '~' {
                ignoring_escape = false;
            }
            continue;
        }

        match ch {
            '\u{1b}' => {
                ignoring_escape = true;
            }
            '\r' | '\n' => {
                let candidate = pending.trim().to_string();
                pending.clear();
                if !candidate.is_empty() {
                    submitted.push(candidate);
                }
            }
            '\u{7f}' | '\u{8}' => {
                pending.pop();
            }
            '\t' => pending.push(' '),
            _ if !ch.is_control() => pending.push(ch),
            _ => {}
        }
    }

    submitted
}

fn merge_assistant_output(existing: &str, chunk: &str, replace_existing: bool) -> String {
    if existing.is_empty() {
        return chunk.to_string();
    }
    if chunk.is_empty() {
        return existing.to_string();
    }

    if existing.ends_with(chunk) || existing.contains(chunk) {
        return existing.to_string();
    }
    if chunk.contains(existing) {
        return chunk.to_string();
    }

    let overlap = longest_suffix_prefix_overlap(existing, chunk);
    if overlap > 0 {
        let suffix = chunk.chars().skip(overlap).collect::<String>();
        return format!("{existing}{suffix}");
    }

    if replace_existing && chunk.chars().count() > existing.chars().count() * 2 {
        return chunk.to_string();
    }

    format!("{existing}{chunk}")
}

fn longest_suffix_prefix_overlap(left: &str, right: &str) -> usize {
    let left_chars = left.chars().collect::<Vec<_>>();
    let right_chars = right.chars().collect::<Vec<_>>();
    let max = left_chars.len().min(right_chars.len());

    for len in (4..=max).rev() {
        if left_chars[left_chars.len() - len..] == right_chars[..len] {
            return len;
        }
    }

    0
}

fn truncate_tail(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    value
        .chars()
        .rev()
        .take(limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn normalize_session_title(value: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= 48 {
        return collapsed;
    }
    format!(
        "{}...",
        collapsed.chars().take(45).collect::<String>().trim_end()
    )
}

fn clean_chat_chunk(raw: &str) -> String {
    let stripped = strip_orphan_ansi_fragments(&strip_ansi(raw));
    // Normalize \r\n -> \n first so that normal PTY newlines don't interfere with
    // the \r-based cursor-overwrite simulation below.
    let normalized_newlines = stripped.replace("\r\n", "\n");
    let mut cleaned_lines = Vec::new();

    for raw_line in normalized_newlines.split('\n') {
        // For each \n-delimited line, simulate bare \r cursor-return overwriting:
        // collect all \r-separated segments, apply them sequentially to a buffer,
        // then use the final visible buffer state.
        let mut line_buf = String::new();
        for segment in raw_line.split('\r') {
            // Each \r resets the virtual cursor to column 0 and overwrites from there.
            let seg_clean: String = segment
                .chars()
                .filter(|c| *c == '\t' || (*c >= ' ' && *c != '\x7f'))
                .collect();
            let seg_len = seg_clean.chars().count();
            let buf_len = line_buf.chars().count();
            if seg_len >= buf_len {
                line_buf = seg_clean;
            } else if seg_len > 0 {
                // Overwrite only the first seg_len chars
                let rest: String = line_buf.chars().skip(seg_len).collect();
                line_buf = format!("{}{}", seg_clean, rest);
            }
            // seg_len == 0: bare \r with no content means no-op (cursor reset but nothing written)
        }

        let normalized = line_buf.trim_end().to_string();
        if looks_like_tui_noise_line(&normalized) {
            continue;
        }
        cleaned_lines.push(normalized);
    }

    let cleaned = collapse_blank_lines(&cleaned_lines.join("\n"), 2)
        .trim_end()
        .to_string();
    dedupe_repeated_handover_lines(&cleaned)
}

fn has_chat_repaint_hint(raw: &str) -> bool {
    // Full-screen repaint sequences
    if raw.contains("\x1b[2J")
        || raw.contains("\x1b[H")
        || raw.contains("\x1b[1;1H")
        || raw.contains("\x1b[?1049h")
        || raw.contains("\x1b[?1049l")
    {
        return true;
    }
    // Detect bare \r (not part of \r\n) — these are in-place spinner redraws.
    // We scan the raw bytes directly to distinguish \r\n (normal PTY newline)
    // from a lone \r (cursor-return overwrite used by spinner animations).
    let raw_bytes = raw.as_bytes();
    for i in 0..raw_bytes.len() {
        if raw_bytes[i] == b'\r' {
            let next = raw_bytes.get(i + 1).copied().unwrap_or(0);
            if next != b'\n' {
                return true;
            }
        }
    }
    false
}

fn looks_like_tui_noise_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }

    // Filter out simple prompt-only lines
    if trimmed == ">" || trimmed == "?" || trimmed == "$" || trimmed == "|" {
        return true;
    }

    let total_chars = trimmed.chars().count();

    // Filter very short lines (≤4 chars) that have no alphanumeric content — TUI fragments like "G|", "|>"
    if total_chars <= 4 && !trimmed.chars().any(|c| c.is_alphanumeric()) {
        return true;
    }

    // Filter out path prompt lines, including those starting with | (e.g. |~/coding/waypoint/src-tauri||>)
    // Uses specific patterns to avoid false-positives on code with | operators.
    let prompt_candidate = trimmed.trim_start_matches('|').trim_start_matches(' ');
    if (prompt_candidate.starts_with('~')
        || prompt_candidate.starts_with('/')
        || prompt_candidate.contains(":\\"))
        && (trimmed.ends_with('>')
            || trimmed.ends_with('$')
            || trimmed.contains('│')
            || trimmed.contains("||>"))
    {
        return true;
    }

    // Lines starting with a spinner symbol are Claude Code status lines (e.g. "⏺ Thinking...")
    let spinner_starts: &[char] = &['⏺', '·', '●', '○', '◯'];
    if let Some(first_char) = trimmed.chars().next() {
        if spinner_starts.contains(&first_char) {
            return true;
        }
        if ('\u{2800}'..='\u{28ff}').contains(&first_char) {
            let normalized: String = trimmed
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect();
            if normalized.contains("working") || normalized.contains("thinking") {
                return true;
            }
        }
    }

    // Detect animation-artifact lines: high density of * and + mixed with alphanumeric.
    // These come from spinner frames being concatenated via \r overwriting.
    if total_chars > 10 {
        let noise_chars = trimmed
            .chars()
            .filter(|c| *c == '*' || *c == '+' || *c == '·' || *c == '●' || *c == '⏺')
            .count();
        // If more than ~14% of chars are animation noise markers, treat as TUI artifact
        if noise_chars * 7 >= total_chars {
            return true;
        }
    }

    // Normalize string to lowercase, keep only alphanumeric for keyword matching
    let normalized: String = trimmed
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();

    let has_box_chars = trimmed.chars().any(|c| {
        matches!(
            c,
            '│' | '┃'
                | '─'
                | '━'
                | '┌'
                | '┐'
                | '└'
                | '┘'
                | '├'
                | '┤'
                | '┬'
                | '┴'
                | '┼'
                | '╭'
                | '╮'
                | '╯'
                | '╰'
                | '█'
                | '▌'
                | '▐'
                | '▄'
                | '▀'
                | '■'
                | '□'
        )
    });
    if normalized.contains("claudecode")
        && (has_box_chars
            || normalized.starts_with("claudecode")
            || normalized.contains("claudecodev"))
    {
        return true;
    }

    // Common TUI status / interactive UI keywords
    if normalized.contains("esctointerrupt")
        || normalized.contains("esctocancel")
        || normalized.contains("forshortcuts")
        || normalized.contains("swirling")
        || normalized.contains("thundering")
        || normalized.contains("releasenotes")
        || normalized.contains("welcomeback")
        || normalized.contains("apiusagebilling")
        || normalized.contains("whatsnew")
        || normalized.contains("tipsforgettingstarted")
        || normalized.contains("alternatescreen")
        || normalized.contains("ctrlv")
        || normalized.contains("pasting")
        || normalized.contains("effort")
        || normalized.contains("mcpserver")
        || normalized.contains("mcpneedsauth")
        || normalized.contains("nativeinstallationexists")
        || normalized.contains("localbinisnotinyourpath")
        || normalized.contains("sessionstartstartuphookerror")
        || normalized.contains("failedwithnonblockingstatus")
        || normalized.contains("imageinclipboard")
        || normalized.contains("ctrlvtopaste")
    {
        return true;
    }

    if normalized == "thinking"
        || normalized.contains("noodling")
        || normalized.contains("stillthinking")
        || normalized.contains("thinkingmore")
        || normalized.contains("brewedfor")
        || normalized.contains("bakedfor")
    {
        return true;
    }

    // "thinking" keyword lines that also contain animation punctuation are spinner status lines
    if normalized.contains("thinking")
        && (trimmed.contains('*')
            || trimmed.contains('+')
            || trimmed.contains('·')
            || trimmed.contains('●')
            || trimmed.contains('⏺')
            || trimmed.contains('>'))
    {
        return true;
    }

    // Box-drawing character dominated lines (TUI borders)
    let box_chars = [
        '│', '┃', '─', '━', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼', '╭', '╮', '╯', '╰', '█',
        '▌', '▐', '▄', '▀', '■', '□',
    ];
    let box_count = trimmed.chars().filter(|c| box_chars.contains(c)).count();
    let has_text = trimmed.chars().any(|c| c.is_alphanumeric());

    if box_count * 3 >= total_chars && !has_text {
        return true;
    }

    // Long separator lines
    if total_chars > 30
        && trimmed
            .chars()
            .all(|c| c == '-' || c == '=' || c == '_' || c == '.' || c == ' ' || c == '|')
    {
        return true;
    }

    false
}

fn collapse_blank_lines(input: &str, max_blank_run: usize) -> String {
    let mut out = String::new();
    let mut blank_run = 0;

    for line in input.split('\n') {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run <= max_blank_run {
                if !out.is_empty() {
                    out.push('\n');
                }
            }
            continue;
        }

        blank_run = 0;
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line.trim_end());
    }

    out
}

fn git_command(cwd: &str, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    Some(tail_chars(&stdout, 30_000))
}

fn tail_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let tail = value
        .chars()
        .rev()
        .take(limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("[truncated to last {limit} chars]\n{tail}")
}

fn empty_fallback<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value.trim()
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn unix_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

/// Clean raw PTY output for use in handover context.
///
/// Steps:
/// 1. Strip all ANSI escape sequences (CSI, OSC, etc.)
/// 2. Simulate carriage return (\r) overwriting: for each line, apply \r
///    so that text after \r replaces text from the beginning of the line.
/// 3. Remove common TUI spinner / decoration characters.
/// 4. Collapse runs of blank or near-blank lines.
/// 5. Truncate to the last `limit` characters.
fn clean_terminal_output(raw: &str, limit: usize) -> String {
    // Step 1: strip ANSI escape sequences
    let stripped = strip_orphan_ansi_fragments(&strip_ansi(raw));

    // Step 2: simulate \r overwriting per line and remove control chars
    let mut clean_lines: Vec<String> = Vec::new();
    for raw_line in stripped.split('\n') {
        let segments: Vec<&str> = raw_line.split('\r').collect();
        // The last non-empty segment after splitting by \r is what's visible
        let visible = segments
            .iter()
            .rev()
            .find(|s| !s.is_empty())
            .copied()
            .unwrap_or("");
        // Remove remaining control characters (< 0x20 except tab)
        let cleaned: String = visible
            .chars()
            .filter(|c| *c == '\t' || (*c >= ' ' && *c != '\x7f'))
            .collect();
        clean_lines.push(cleaned);
    }

    // Step 3: remove TUI spinner / decoration characters and known status lines
    let spinner_chars: &[char] = &[
        '✢', '✳', '✶', '✻', '✽', '⏺', '⠂', '⠐',
        '·', // middle dot used as separator in TUI status bars
    ];
    let clean_lines: Vec<String> = clean_lines
        .into_iter()
        .filter_map(|line| {
            let trimmed = line.trim();
            // If the line is ONLY spinner/decoration chars (possibly with spaces), skip it
            if !trimmed.is_empty()
                && trimmed
                    .chars()
                    .all(|c| spinner_chars.contains(&c) || c == ' ')
            {
                return None;
            }
            if looks_like_tui_noise_line(trimmed) {
                None
            } else {
                Some(line)
            }
        })
        .collect();

    // Step 4: collapse runs of blank lines (keep at most 1)
    let mut result = String::new();
    let mut prev_blank = false;
    for line in &clean_lines {
        let is_blank = line.trim().is_empty();
        if is_blank {
            if !prev_blank {
                result.push('\n');
            }
            prev_blank = true;
        } else {
            if !result.is_empty() && !prev_blank {
                result.push('\n');
            }
            result.push_str(line.trim_end());
            prev_blank = false;
        }
    }

    // Step 5: collapse duplicate repaint lines and truncate to last `limit` chars
    tail_chars(&dedupe_repeated_handover_lines(&result), limit)
}

fn clean_terminal_output_minimal(raw: &str, limit: usize) -> String {
    clean_handover_text_minimal(raw, limit)
}

fn clean_terminal_input(raw: &str, limit: usize) -> String {
    let stripped = strip_orphan_ansi_fragments(&strip_ansi(raw));
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();

    for ch in stripped.chars() {
        match ch {
            '\r' | '\n' => {
                let line = current.trim();
                if !line.is_empty() {
                    lines.push(line.to_string());
                }
                current.clear();
            }
            '\x08' | '\x7f' => {
                current.pop();
            }
            c if c >= ' ' && c != '\x7f' => current.push(c),
            _ => {}
        }
    }

    let trailing = current.trim();
    if !trailing.is_empty() {
        lines.push(trailing.to_string());
    }

    tail_chars(&lines.join("\n"), limit)
}

fn dedupe_repeated_handover_lines(input: &str) -> String {
    let mut seen = HashSet::new();
    let mut output = Vec::new();

    for line in input.lines() {
        let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
        let should_dedupe = normalized.chars().count() >= 24
            && !normalized.starts_with("```")
            && normalized.chars().any(|c| c.is_alphanumeric());

        if should_dedupe && !seen.insert(normalized) {
            continue;
        }
        output.push(line);
    }

    collapse_blank_lines(&output.join("\n"), 2)
}

fn strip_orphan_ansi_fragments(input: &str) -> String {
    let chars = input.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(input.len());
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '[' {
            if let Some(next) = orphan_csi_fragment_end(&chars, i + 1) {
                i = next;
                continue;
            }
        }

        if chars[i].is_ascii_digit() || chars[i] == ';' {
            if let Some(next) = orphan_sgr_fragment_end(&chars, i) {
                i = next;
                continue;
            }
            if let Some(next) = orphan_cursor_erase_fragment_end(&chars, i) {
                i = next;
                continue;
            }
        }

        output.push(chars[i]);
        i += 1;
    }

    output
}

fn orphan_csi_fragment_end(chars: &[char], start: usize) -> Option<usize> {
    let mut cursor = start;
    while cursor < chars.len() && cursor.saturating_sub(start) <= 24 {
        let ch = chars[cursor];
        if ch.is_ascii_digit() || ch == ';' || ch == '?' {
            cursor += 1;
            continue;
        }
        if matches!(ch, 'm' | 'K' | 'J' | 'H' | 'X') {
            return Some(cursor + 1);
        }
        return None;
    }
    None
}

fn orphan_sgr_fragment_end(chars: &[char], start: usize) -> Option<usize> {
    let mut cursor = start;
    let mut saw_semicolon = chars[start] == ';';
    while cursor < chars.len() && cursor.saturating_sub(start) <= 32 {
        let ch = chars[cursor];
        if ch.is_ascii_digit() {
            cursor += 1;
            continue;
        }
        if ch == ';' {
            saw_semicolon = true;
            cursor += 1;
            continue;
        }
        if ch == 'm' && saw_semicolon {
            return Some(cursor + 1);
        }
        return None;
    }
    None
}

fn orphan_cursor_erase_fragment_end(chars: &[char], start: usize) -> Option<usize> {
    let mut cursor = start;
    while cursor < chars.len() && chars[cursor].is_ascii_digit() && cursor.saturating_sub(start) < 4
    {
        cursor += 1;
    }
    if cursor > start + 1 && cursor < chars.len() && chars[cursor] == 'X' {
        Some(cursor + 1)
    } else {
        None
    }
}

fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Normal,
        Esc,
        Csi,
        Osc,
        OscEsc,
    }

    let mut state = State::Normal;

    while let Some(c) = chars.next() {
        match state {
            State::Normal => {
                if c == '\x1b' {
                    state = State::Esc;
                } else {
                    output.push(c);
                }
            }
            State::Esc => match c {
                '[' => state = State::Csi,
                ']' => state = State::Osc,
                '(' | ')' | '#' | '%' | '*' | '+' | '-' | '.' | '/' => {
                    chars.next();
                    state = State::Normal;
                }
                _ => {
                    state = State::Normal;
                }
            },
            State::Csi => {
                let b = c as u32;
                if (0x40..=0x7E).contains(&b) {
                    state = State::Normal;
                }
            }
            State::Osc => {
                if c == '\x07' {
                    state = State::Normal;
                } else if c == '\x1b' {
                    state = State::OscEsc;
                }
            }
            State::OscEsc => {
                if c == '\\' {
                    state = State::Normal;
                } else {
                    state = State::Osc;
                }
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    static HOME_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_strip_ansi() {
        assert_eq!(strip_ansi("\x1b[31mHello\x1b[0m"), "Hello");
        assert_eq!(strip_ansi("\x1b[22G·\x1b[24GAPI"), "·API");
        assert_eq!(strip_ansi("Normal Text"), "Normal Text");
        assert_eq!(strip_ansi("\x1b]0;Claude Code\x07Yes"), "Yes");
    }

    #[test]
    fn test_clean_terminal_output_strips_spinner_lines() {
        let input = "Hello\n✻\n✶\n\nWorld";
        let result = clean_terminal_output(input, 10000);
        assert!(result.contains("Hello"));
        assert!(result.contains("World"));
        assert!(!result.contains('✻'));
        assert!(!result.contains('✶'));
    }

    #[test]
    fn test_clean_terminal_output_filters_claude_tui_noise() {
        let input = "╭───Claude Code v2.1.144────────────────╮\n│ Tips for getting started │\n? for shortcuts ● high · /effort\n✻ Noodling… still thinking\nthinking\n看完代码，其实当前实现已经支持按次调用切换。\nNative installation exists but ~/.local/bin is not in your PATH. Run:\n";
        let result = clean_terminal_output(input, 10000);

        assert!(result.contains("看完代码"));
        assert!(!result.contains("Claude Code"));
        assert!(!result.contains("Tips for getting started"));
        assert!(!result.contains("shortcuts"));
        assert!(!result.contains("Noodling"));
        assert!(!result.contains("Native installation exists"));
    }

    #[test]
    fn test_clean_terminal_output_filters_repaint_artifacts() {
        let input = "### 为什么按 Agent 分类？;238;232;213m1. 工具视角\n[1m重复标题\n⣻ Working...esc to cancelGemini 3.5 Flash (High)\n最终内容";
        let result = clean_terminal_output(input, 10000);

        assert!(result.contains("### 为什么按 Agent 分类？1. 工具视角"));
        assert!(result.contains("重复标题"));
        assert!(result.contains("最终内容"));
        assert!(!result.contains("238;232;213m"));
        assert!(!result.contains("Working"));
        assert!(!result.contains("[1m"));
    }

    #[test]
    fn test_clean_terminal_output_handles_cr_overwrite() {
        // \r causes the cursor to go back to start of line, overwriting
        let input = "old text\rnew";
        let result = clean_terminal_output(input, 10000);
        // "old text" gets overwritten by "new" which starts at col 0
        assert!(result.contains("new"));
        assert!(!result.contains("old text"));
    }

    #[test]
    fn test_clean_terminal_output_collapses_blank_lines() {
        let input = "A\n\n\n\n\n\nB";
        let result = clean_terminal_output(input, 10000);
        // Should have at most one blank line between A and B
        assert!(!result.contains("\n\n\n"));
        assert!(result.contains('A'));
        assert!(result.contains('B'));
    }

    #[test]
    fn test_clean_terminal_output_truncates() {
        let input = "A".repeat(500);
        let result = clean_terminal_output(&input, 100);
        assert!(result.chars().count() <= 130); // 100 + truncation prefix
    }

    #[test]
    fn test_clean_terminal_input_preserves_user_text() {
        let input = "hi\nfoo\x7f\x7fr\n\x1b[A";
        let result = clean_terminal_input(input, 10000);
        assert!(result.contains("hi"));
        assert!(result.contains("fr"));
        assert!(!result.contains('\x1b'));
    }

    #[test]
    fn test_extract_submitted_user_inputs_handles_escape_and_multiline() {
        let mut pending = String::new();
        assert!(extract_submitted_user_inputs(&mut pending, "hel").is_empty());
        assert_eq!(pending, "hel");

        let submitted = extract_submitted_user_inputs(&mut pending, "lo\x1b[A\rnext\n");

        assert_eq!(submitted, vec!["hello".to_string(), "next".to_string()]);
        assert!(pending.is_empty());
    }

    #[test]
    fn test_merge_assistant_output_keeps_existing_when_repaint_is_suffix_fragment() {
        let existing = "要我帮你生成一份CLAUDE.md模板，或者直接改成方案3的自动推断？";
        let chunk = "份CLAUDE.md模板，或者直接改成方案3的自动推断？";

        let result = merge_assistant_output(existing, chunk, true);

        assert_eq!(result, existing);
    }

    #[test]
    fn test_merge_assistant_output_uses_overlap_for_incremental_chunks() {
        let existing = "方式1把端绑定到项目目录";
        let chunk = "项目目录语义，一次约定长期有效";

        let result = merge_assistant_output(existing, chunk, true);

        assert_eq!(result, "方式1把端绑定到项目目录语义，一次约定长期有效");
    }

    #[test]
    fn test_clean_chat_chunk_filters_completion_status() {
        let result = clean_chat_chunk("实际回答\n✻Bakedfor48s\n");

        assert_eq!(result, "实际回答");
    }

    #[test]
    fn test_compact_handover_prompt_is_structured_manifest() {
        let source = SessionInfo {
            id: "source".to_string(),
            agent_id: "codex".to_string(),
            agent_name: "Codex".to_string(),
            title: "Source Session".to_string(),
            command: "codex".to_string(),
            cwd: "/tmp/workspace".to_string(),
            status: SessionStatus::Running,
            attached: false,
            created_at: 1,
            last_active_at: 1,
            first_user_message: None,
            native_session_ref: None,
            parent_session_id: None,
            handover_root_id: None,
            dangerous: false,
            none_workspace: false,
        };
        let target = SessionInfo {
            id: "target".to_string(),
            agent_id: "claude-code".to_string(),
            agent_name: "Claude Code".to_string(),
            title: "Target Session".to_string(),
            command: "claude".to_string(),
            cwd: "/tmp/workspace".to_string(),
            status: SessionStatus::Running,
            attached: false,
            created_at: 2,
            last_active_at: 2,
            first_user_message: None,
            native_session_ref: None,
            parent_session_id: None,
            handover_root_id: None,
            dangerous: false,
            none_workspace: false,
        };

        let result = build_compact_handover_prompt(
            &source,
            &target,
            "finish P0",
            "",
            "log",
            "input",
            None,
            "",
        );

        assert!(result.contains("## Summary"));
        assert!(!result.contains("## Current State"));
        assert!(!result.contains("## Attachments"));
        assert!(!result.contains("## Changed Files"));
        assert!(result.contains("## Evidence"));
        assert!(result.contains("### Recent Conversation Timeline (ordered)"));
        assert!(!result.contains("### Recent User Inputs (best effort)"));
        assert!(result.contains("## Recommended Next Steps"));
        assert!(result.contains("finish P0"));
    }

    #[test]
    fn test_handover_inheritance_snapshot_excludes_boilerplate_and_preserves_order() {
        let prompt = r#"# Waypoint Handover

## Summary
Continuation from Codex session "A" in `/tmp/workspace`.

## Source Session
- Agent: Codex
- Title: A

## Target Session
- Agent: Claude Code
- Title: B

## User Note
Keep going.

## Current State
- Branch: main

## Changed Files

### Git Status
```text
 M src/main.rs
```

#### Diff Preview
```diff
diff --git a/src/main.rs b/src/main.rs
```

## Inherited Handover Context
older handover context

## Evidence

### Recent Source Terminal Context
```text
User:
先做 A

Assistant:
这是一段很长的重复内容用于模拟 TUI repaint 造成的重复行
这是一段很长的重复内容用于模拟 TUI repaint 造成的重复行
最后结论
```

### Recent User Inputs (best effort)
```text
先做 A
```

## Recommended Next Steps
- Should not be inherited.

## Instructions
- Should not be inherited.
"#;

        let result = format_handover_for_inheritance(prompt, 10000);

        let older = result.find("older handover context").unwrap();
        let hop = result.find("### Previous Handover Hop").unwrap();
        let evidence = result.find("### Previous Handover Evidence").unwrap();
        assert!(older < hop);
        assert!(hop < evidence);
        assert!(result.contains("Keep going."));
        assert!(result.contains("最后结论"));
        assert!(!result.contains("Recommended Next Steps"));
        assert!(!result.contains("Instructions"));
        assert!(!result.contains("diff --git"));
        assert_eq!(
            result
                .matches("这是一段很长的重复内容用于模拟 TUI repaint 造成的重复行")
                .count(),
            1
        );
    }

    #[test]
    fn test_clean_chat_chunk() {
        let input = "Hello\r\nWorld\r\n";
        let result = clean_chat_chunk(input);
        assert_eq!(result, "Hello\nWorld");

        // test carriage return overwrite in the middle of a line
        let input_cr = "loading... 50%\rloading... 100%\r\ndone\r\n";
        let result_cr = clean_chat_chunk(input_cr);
        assert_eq!(result_cr, "loading... 100%\ndone");

        // test TUI noise filtering
        let noise_input = "┌───ClaudeCodev2.1.144──────────────────────────┐\n││Tipsforgettingstarted│\n?forshortcuts●high·/effort\n* Swirling...\nesctointerrupt●high·/effort+·+***Sw*Sirwliirn*lig...*ng*..\n~/coding/waypoint/src-tauri││>\n>\n?\n";
        let noise_result = clean_chat_chunk(noise_input);
        assert_eq!(noise_result, "");
    }

    #[test]
    fn test_format_chat_messages_for_handover_prefers_conversation() {
        let messages = vec![
            ChatMessage {
                id: "startup".to_string(),
                role: ChatRole::Assistant,
                content: "Welcome back!".to_string(),
                pending: false,
                created_at: 1,
                updated_at: 1,
            },
            ChatMessage {
                id: "user".to_string(),
                role: ChatRole::User,
                content: "我这里是不是要加 CLAUDE.md 约定？".to_string(),
                pending: false,
                created_at: 2,
                updated_at: 2,
            },
            ChatMessage {
                id: "assistant".to_string(),
                role: ChatRole::Assistant,
                content:
                    "? for shortcuts ● high · /effort\n可以，建议把 tokenSetId 约定绑定到项目目录。"
                        .to_string(),
                pending: false,
                created_at: 3,
                updated_at: 3,
            },
        ];

        let result = format_chat_messages_for_handover(&messages, 10000);

        assert!(result.contains("User:\n我这里是不是要加 CLAUDE.md"));
        assert!(result.contains("tokenSetId 约定绑定到项目目录"));
        assert!(!result.contains("Welcome back"));
        assert!(result.contains("shortcuts"));
    }

    #[test]
    fn test_extract_user_prompts_from_terminal_context_handles_cli_prompts() {
        let context = "│~/coding/flowbite-mcp││\n❯  我觉得也是这样比较好，实现吧\nAssistant answer\n› 重新构建打包吧";

        let result = extract_user_prompts_from_terminal_context(context, 10000);

        assert!(result.contains("我觉得也是这样比较好"));
        assert!(result.contains("重新构建打包吧"));
        assert!(!result.contains("Assistant answer"));
    }

    #[test]
    fn test_parse_claude_native_transcript_preserves_user_followups() {
        let jsonl = r#"
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"先实现 preview"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"已处理 preview"},{"type":"tool_use","name":"Read"}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ignored tool output"}]}}
{"type":"user","message":{"role":"user","content":"追问：create 后不要再显示 handover 弹窗"}}
"#;

        let messages = parse_native_transcript_messages(
            std::io::Cursor::new(jsonl),
            NativeTranscriptKind::Claude,
        );
        let context = format_native_transcript_context(&messages, 10000);
        let inputs = format_native_user_inputs(&messages, 10000);

        assert!(context.contains("User:\n先实现 preview"));
        assert!(context.contains("Assistant:\n已处理 preview"));
        assert!(inputs.contains("先实现 preview"));
        assert!(inputs.contains("追问：create 后不要再显示"));
        assert!(!context.contains("ignored tool output"));
    }

    #[test]
    fn test_parse_claude_native_transcript_preserves_claude_code_identity_answer() {
        let jsonl = r#"
{"type":"user","message":{"role":"user","content":"你是谁"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"我是 Claude Code，Anthropic 官方的命令行 AI 编程助手。\n\n有什么我可以帮你的吗？"}]}}
"#;

        let messages = parse_native_transcript_messages(
            std::io::Cursor::new(jsonl),
            NativeTranscriptKind::Claude,
        );
        let context = format_native_transcript_context(&messages, 10000);

        assert!(context.contains("Assistant:\n我是 Claude Code"));
        assert!(context.contains("Anthropic 官方"));
        assert!(context.contains("有什么我可以帮你的吗？"));
    }

    #[test]
    fn test_parse_agy_native_transcript_preserves_messages() {
        let jsonl = r#"
{"type":"USER_INPUT","content":"Please generate a technical proposal."}
{"type":"PLANNER_RESPONSE","content":"Sure! I will create a proposal."}
{"type":"OTHER_EVENT","content":"some other stuff"}
"#;

        let messages = parse_native_transcript_messages(
            std::io::Cursor::new(jsonl),
            NativeTranscriptKind::Agy,
        );
        let context = format_native_transcript_context(&messages, 10000);
        let inputs = format_native_user_inputs(&messages, 10000);

        assert!(context.contains("User:\nPlease generate a technical proposal."));
        assert!(context.contains("Assistant:\nSure! I will create a proposal."));
        assert!(inputs.contains("Please generate a technical proposal."));
        assert!(!context.contains("some other stuff"));
    }

    #[test]
    fn test_find_agy_conversation_id() {
        let _guard = HOME_MUTEX.lock().unwrap();
        let old_home = env::var("HOME").ok();
        let root = env::temp_dir().join(format!("waypoint-test-agy-{}", Uuid::new_v4()));
        env::set_var("HOME", &root);

        let conv_id = "test-conv-123";
        let session_id = "test-session-456";
        let log_dir = root
            .join(".gemini")
            .join("antigravity-cli")
            .join("brain")
            .join(conv_id)
            .join(".system_generated")
            .join("logs");
        fs::create_dir_all(&log_dir).unwrap();

        let transcript_path = log_dir.join("transcript.jsonl");
        fs::write(
            &transcript_path,
            format!("some log lines\nwaypoint_session_id: {session_id}\nmore lines"),
        )
        .unwrap();

        let found = find_agy_conversation_id(session_id);
        assert_eq!(found, Some(conv_id.to_string()));

        // Cleanup
        if let Some(h) = old_home {
            env::set_var("HOME", h);
        } else {
            env::remove_var("HOME");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_cache_agy_resume_ref_in_meta_from_native_transcript_marker() {
        let _guard = HOME_MUTEX.lock().unwrap();
        let old_home = env::var("HOME").ok();
        let root = env::temp_dir().join(format!("waypoint-test-agy-cache-{}", Uuid::new_v4()));
        env::set_var("HOME", &root);

        let conv_id = "test-conv-cache-123";
        let session_id = "test-session-cache-456";
        let log_dir = root
            .join(".gemini")
            .join("antigravity-cli")
            .join("brain")
            .join(conv_id)
            .join(".system_generated")
            .join("logs");
        fs::create_dir_all(&log_dir).unwrap();
        fs::write(
            log_dir.join("transcript.jsonl"),
            format!("first user turn\n<!-- waypoint_session_id: {session_id} -->"),
        )
        .unwrap();

        let mut meta = SessionMeta {
            id: session_id.to_string(),
            agent_id: "agy".to_string(),
            agent_name: "agy".to_string(),
            title: "agy session".to_string(),
            command: "agy".to_string(),
            cwd: "/tmp".to_string(),
            status: SessionStatus::Exited,
            attached: false,
            created_at: 123456,
            last_active_at: 123456,
            first_user_message: Some("hi".to_string()),
            native_session_ref: None,
            parent_session_id: None,
            handover_root_id: None,
            dangerous: false,
            none_workspace: false,
        };

        assert!(cache_agy_resume_ref_in_meta(&mut meta));
        let native_ref = meta.native_session_ref.unwrap();
        assert_eq!(native_ref.provider, "agy");
        assert_eq!(native_ref.id, Some(conv_id.to_string()));
        assert_eq!(
            native_ref.resume_command,
            Some(format!("agy --conversation='{}'", conv_id))
        );

        if let Some(h) = old_home {
            env::set_var("HOME", h);
        } else {
            env::remove_var("HOME");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_resolve_agy_resume_ref_prefers_native_marker_over_terminal_resume_text() {
        let _guard = HOME_MUTEX.lock().unwrap();
        let old_home = env::var("HOME").ok();
        let root = env::temp_dir().join(format!("waypoint-test-agy-priority-{}", Uuid::new_v4()));
        env::set_var("HOME", &root);

        let session_id = "test-session-priority-456";
        let native_conv_id = "native-marker-conv";
        let log_dir = root
            .join(".gemini")
            .join("antigravity-cli")
            .join("brain")
            .join(native_conv_id)
            .join(".system_generated")
            .join("logs");
        fs::create_dir_all(&log_dir).unwrap();
        fs::write(
            log_dir.join("transcript.jsonl"),
            format!("<!-- waypoint_session_id: {session_id} -->"),
        )
        .unwrap();

        let waypoint_dir = session_dir(session_id).unwrap();
        fs::create_dir_all(&waypoint_dir).unwrap();
        fs::write(
            waypoint_dir.join("transcript.log"),
            "Resume in the same project: agy --conversation=terminal-conv --project=default-cli-project",
        )
        .unwrap();

        let found = resolve_agy_resume_ref(session_id).unwrap();
        assert_eq!(found.conversation_id, native_conv_id);
        assert_eq!(found.project, None);

        if let Some(h) = old_home {
            env::set_var("HOME", h);
        } else {
            env::remove_var("HOME");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_parse_agy_resume_ref_prefers_same_project_command() {
        let output = r#"
Something else
Resume: agy --conversation=c79caf4a-cdf9-4b20-a2fb-e6143ba1ddf9 (or -c)
Resume in the same project: agy --conversation=c79caf4a-cdf9-4b20-a2fb-e6143ba1ddf9 --project=default-cli-project
"#;

        let found = parse_agy_resume_ref(output);

        assert_eq!(
            found,
            Some(AgyResumeRef {
                conversation_id: "c79caf4a-cdf9-4b20-a2fb-e6143ba1ddf9".to_string(),
                project: Some("default-cli-project".to_string()),
            })
        );
    }

    #[test]
    fn test_build_agy_artifacts_context() {
        let _guard = HOME_MUTEX.lock().unwrap();
        let old_home = env::var("HOME").ok();
        let root = env::temp_dir().join(format!("waypoint-test-agy-artifacts-{}", Uuid::new_v4()));
        env::set_var("HOME", &root);

        let conv_id = "test-conv-123";
        let session_id = "test-session-456";
        let brain_dir = root
            .join(".gemini")
            .join("antigravity-cli")
            .join("brain")
            .join(conv_id);

        let log_dir = brain_dir.join(".system_generated").join("logs");
        fs::create_dir_all(&log_dir).unwrap();

        // 1. Write the transcript file containing the session ID tracking comment
        let transcript_path = log_dir.join("transcript.jsonl");
        fs::write(
            &transcript_path,
            format!("waypoint_session_id: {session_id}"),
        )
        .unwrap();

        // 2. Write some markdown artifacts
        let art1 = brain_dir.join("proposal.md");
        fs::write(&art1, "# Technical Proposal\nProposal content.").unwrap();

        let art2 = brain_dir.join("design.md");
        fs::write(&art2, "# Design Details\nDesign content.").unwrap();

        // 3. Call build_agy_artifacts_context
        let result = build_agy_artifacts_context(session_id);

        assert!(result.contains("## Session Artifacts"));
        assert!(result.contains("### [proposal.md]"));
        assert!(result.contains("# Technical Proposal"));
        assert!(result.contains("### [design.md]"));
        assert!(result.contains("# Design Details"));

        // Cleanup
        if let Some(h) = old_home {
            env::set_var("HOME", h);
        } else {
            env::remove_var("HOME");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_native_resume_command_for_agy_resolves_dynamically() {
        let _guard = HOME_MUTEX.lock().unwrap();
        let old_home = env::var("HOME").ok();
        let root = env::temp_dir().join(format!("waypoint-test-agy-resume-{}", Uuid::new_v4()));
        env::set_var("HOME", &root);

        let conv_id = "test-conv-999";
        let session_id = "test-session-888";
        let log_dir = root
            .join(".gemini")
            .join("antigravity-cli")
            .join("brain")
            .join(conv_id)
            .join(".system_generated")
            .join("logs");
        fs::create_dir_all(&log_dir).unwrap();

        let transcript_path = log_dir.join("transcript.jsonl");
        fs::write(
            &transcript_path,
            format!("waypoint_session_id: {session_id}"),
        )
        .unwrap();

        let meta = SessionMeta {
            id: session_id.to_string(),
            agent_id: "agy".to_string(),
            agent_name: "agy".to_string(),
            title: "agy session".to_string(),
            command: "agy".to_string(),
            cwd: "/tmp".to_string(),
            status: SessionStatus::Exited,
            attached: false,
            created_at: 123456,
            last_active_at: 123456,
            first_user_message: None,
            native_session_ref: None,
            parent_session_id: None,
            handover_root_id: None,
            dangerous: false,
            none_workspace: false,
        };

        let result = native_resume_command_for(&meta).unwrap();
        assert!(result.is_some());
        let resume_cmd = result.unwrap();
        assert!(resume_cmd.executable.ends_with("agy"));
        assert!(resume_cmd
            .args
            .contains(&format!("--conversation={conv_id}")));

        let ns_ref = resume_cmd.native_session_ref.unwrap();
        assert_eq!(ns_ref.id, Some(conv_id.to_string()));

        // Cleanup
        if let Some(h) = old_home {
            env::set_var("HOME", h);
        } else {
            env::remove_var("HOME");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_claude_project_dir_name_matches_native_storage() {
        assert_eq!(
            claude_project_dir_name("/Users/liuzhe.x/coding/mcp-deck"),
            "-Users-liuzhe-x-coding-mcp-deck"
        );
    }

    #[test]
    fn test_codex_transcript_matches_workspace_by_session_meta() {
        let root = env::temp_dir().join(format!("waypoint-test-{}", Uuid::new_v4()));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let transcript = root.join("codex.jsonl");
        std::fs::write(
            &transcript,
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"cwd\":\"{}\"}}}}\n",
                workspace.display()
            ),
        )
        .unwrap();

        let normalized = normalize_existing_path(workspace.to_str().unwrap());
        assert!(codex_transcript_matches_workspace(
            &transcript,
            workspace.to_str().unwrap(),
            normalized.as_deref()
        ));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_append_claude_session_id_precedes_initial_prompt() {
        let mut args = vec!["Read the handover file".to_string()];

        append_claude_session_id(&mut args, "acc81906-1dbd-4c13-b910-4c903c4feea6");

        assert_eq!(
            args,
            vec![
                "--session-id",
                "acc81906-1dbd-4c13-b910-4c903c4feea6",
                "Read the handover file"
            ]
        );
    }

    #[test]
    fn test_codex_dangerous_flag_does_not_split_add_dir_value() {
        let mut args = vec![
            "--no-alt-screen".to_string(),
            "--add-dir".to_string(),
            "/Users/example/.waypoint/workspace".to_string(),
            "Read the handover file".to_string(),
        ];

        apply_dangerous_flag("codex", &mut args);

        assert_eq!(
            args,
            vec![
                "--dangerously-bypass-approvals-and-sandbox",
                "--no-alt-screen",
                "--add-dir",
                "/Users/example/.waypoint/workspace",
                "Read the handover file",
            ]
        );
    }

    #[test]
    fn test_parse_codex_native_transcript_preserves_user_messages() {
        let jsonl = r#"
{"type":"session_meta","payload":{"id":"native-1","cwd":"/tmp/workspace"}}
{"type":"event_msg","msg":"user_message","user_message":"duplicated event should be ignored"}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"用户第一问"}]}}
{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"hidden"}]}}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"助手回答"}]}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"用户追问"}]}}
"#;

        let messages = parse_native_transcript_messages(
            std::io::Cursor::new(jsonl),
            NativeTranscriptKind::Codex,
        );
        let context = format_native_transcript_context(&messages, 10000);
        let inputs = format_native_user_inputs(&messages, 10000);

        assert!(context.contains("User:\n用户第一问"));
        assert!(context.contains("Assistant:\n助手回答"));
        assert!(inputs.contains("用户追问"));
        assert!(!context.contains("duplicated event"));
        assert!(!context.contains("hidden"));
    }
}

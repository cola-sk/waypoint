use std::{
    collections::HashMap,
    env, fs,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::Engine;
use parking_lot::Mutex;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
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
const COMPACT_GIT_STATUS_CHARS: usize = 4_000;
const HANDOVER_INHERITED_CONTEXT_CHARS: usize = 12_000;
const COMPACT_HANDOVER_INHERITED_CONTEXT_CHARS: usize = 6_000;
const HANDOVER_INHERITED_STORE_CHARS: usize = 24_000;
const GIT_OUTPUT_LIMIT_CHARS: usize = 30_000;
const HANDOVER_INJECT_ATTEMPTS: usize = 8;
const HANDOVER_INJECT_DELAY_MS: u64 = 350;
const CODEX_HANDOVER_STARTUP_DELAY_MS: u64 = 1_800;
const MAX_PTY_ROWS: u16 = 240;
const MAX_PTY_COLS: u16 = 600;

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
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSessionRef {
    provider: String,
    id: Option<String>,
    name: Option<String>,
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
}

#[derive(Clone, Serialize)]
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
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<SessionInfo, String> {
    state
        .manager
        .create_agent_session(app, &agent_id, cwd, rows, cols)
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
pub fn forward_session(
    state: State<'_, AppState>,
    source_session_id: String,
    target_session_id: String,
    note: Option<String>,
) -> Result<HandoverResult, String> {
    state
        .manager
        .forward_session(&source_session_id, &target_session_id, note)
}

#[tauri::command]
pub fn continue_session(
    state: State<'_, AppState>,
    app: AppHandle,
    source_session_id: String,
    target_agent_id: String,
    cwd: String,
    note: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<HandoverResult, String> {
    state.manager.continue_session(
        app,
        &source_session_id,
        &target_agent_id,
        cwd,
        note,
        rows,
        cols,
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
        self.spawn_session(
            app,
            definition.id,
            definition.name,
            definition.name.to_string(),
            resolved.display,
            resolved.executable,
            resolved.args,
            cwd,
            rows,
            cols,
            Vec::new(),
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
    ) -> Result<SessionInfo, String> {
        let cwd_path = PathBuf::from(&cwd);
        if !cwd_path.is_dir() {
            return Err(format!("workspace directory does not exist: {cwd}"));
        }

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
            cwd,
            status: SessionStatus::Running,
            attached: false,
            created_at: created_at_override.unwrap_or(now),
            last_active_at: now,
            first_user_message,
            native_session_ref,
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

        let meta = load_session_meta(session_id)?;
        if !PathBuf::from(&meta.cwd).is_dir() {
            return Err(format!("workspace directory does not exist: {}", meta.cwd));
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
        )
    }

    fn write_session(&self, session_id: &str, data: String) -> Result<(), String> {
        let session = self.get(session_id)?;
        if !matches!(session.info().status, SessionStatus::Running) {
            return Err("session is not running".to_string());
        }
        session.append_input(&data);
        session.capture_user_input(&data);
        session
            .writer
            .lock()
            .write_all(data.as_bytes())
            .map_err(|err| format!("failed to write to PTY: {err}"))?;
        session.meta.lock().last_active_at = unix_timestamp();
        session.persist_meta();
        Ok(())
    }

    fn send_chat_message(&self, session_id: &str, message: &str) -> Result<(), String> {
        let session = self.get(session_id)?;
        if !matches!(session.info().status, SessionStatus::Running) {
            return Err("session is not running".to_string());
        }
        let payload = message.trim();
        if payload.is_empty() {
            return Ok(());
        }
        let normalized = payload.replace('\n', "\r");
        let injected = format!("{normalized}\r");
        session
            .writer
            .lock()
            .write_all(injected.as_bytes())
            .map_err(|err| format!("failed to write chat message to PTY: {err}"))?;
        session.append_chat_user_message(payload);
        session.append_input(&format!("{payload}\n"));
        session.remember_first_user_message(payload);
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
    ) -> Result<HandoverResult, String> {
        if source_session_id == target_session_id {
            return Err("source and target sessions must be different".to_string());
        }

        let source = self.get(source_session_id)?;
        let target = self.get(target_session_id)?;
        let source_info = source.info();
        let target_info = target.info();
        if !matches!(source_info.status, SessionStatus::Running) {
            return Err(format!(
                "source session is not running: {}",
                source_info.title
            ));
        }
        if !matches!(target_info.status, SessionStatus::Running) {
            return Err(format!(
                "target session is not running: {}",
                target_info.title
            ));
        }

        let prompt = self.inject_handover(&source, &target, note, false)?;

        Ok(HandoverResult {
            prompt,
            source_session: source_info,
            target_session: target_info,
            mode: "existing-session".to_string(),
        })
    }

    fn continue_session(
        &self,
        app: AppHandle,
        source_session_id: &str,
        target_agent_id: &str,
        cwd: String,
        note: Option<String>,
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

        if target_agent_id == "gemini" {
            return self.continue_gemini_with_initial_prompt(
                app,
                &source,
                source_info,
                cwd,
                note,
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
                rows,
                cols,
            );
        }

        let target_info = self.create_agent_session(app, target_agent_id, cwd, rows, cols)?;
        let target = self.get(&target_info.id)?;
        let prompt = self.inject_handover(&source, &target, note, true)?;

        Ok(HandoverResult {
            prompt,
            source_session: source_info,
            target_session: target_info,
            mode: "new-session".to_string(),
        })
    }

    fn continue_claude_with_initial_prompt(
        &self,
        app: AppHandle,
        source: &Arc<PtySession>,
        source_info: SessionInfo,
        cwd: String,
        note: Option<String>,
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
        let planned_target = SessionInfo {
            id: "pending".to_string(),
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
        };
        let prompt = self.build_compact_handover_prompt_for(
            source,
            &source_info,
            &planned_target,
            note.clone(),
        );

        let handover_path = write_handover_file(&cwd, &prompt)?;
        let startup_prompt = handover_reference_startup_prompt(&handover_path);

        let mut args = resolved.args;
        args.push(startup_prompt);

        let target_info = self.spawn_session(
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
        )?;
        let target = self.get(&target_info.id)?;
        self.remember_handover(&target, &prompt);

        Ok(HandoverResult {
            prompt,
            source_session: source_info,
            target_session: target_info,
            mode: "new-session".to_string(),
        })
    }

    fn continue_gemini_with_initial_prompt(
        &self,
        app: AppHandle,
        source: &Arc<PtySession>,
        source_info: SessionInfo,
        cwd: String,
        note: Option<String>,
        rows: Option<u16>,
        cols: Option<u16>,
    ) -> Result<HandoverResult, String> {
        let definition = agent_definitions()
            .into_iter()
            .find(|definition| definition.id == "gemini")
            .ok_or_else(|| "Gemini CLI preset is missing".to_string())?;
        let resolved = resolve_agent_command(&definition).ok_or_else(|| {
            "Gemini CLI is not available in PATH. Install it or make sure your login shell can resolve it."
                .to_string()
        })?;
        let planned_target = SessionInfo {
            id: "pending".to_string(),
            agent_id: definition.id.to_string(),
            agent_name: definition.name.to_string(),
            title: "Gemini CLI new session".to_string(),
            command: "gemini --prompt-interactive <handover>".to_string(),
            cwd: cwd.clone(),
            status: SessionStatus::Running,
            attached: false,
            created_at: unix_timestamp(),
            last_active_at: unix_timestamp(),
            first_user_message: None,
            native_session_ref: None,
        };
        let prompt = self.build_compact_handover_prompt_for(
            source,
            &source_info,
            &planned_target,
            note.clone(),
        );
        let handover_path = write_handover_file(&cwd, &prompt)?;
        let startup_prompt = handover_reference_startup_prompt(&handover_path);
        let mut args = resolved.args;
        if let Some(parent) = handover_path.parent() {
            args.push("--include-directories".to_string());
            args.push(parent.to_string_lossy().into_owned());
        }
        args.push("--prompt-interactive".to_string());
        args.push(startup_prompt);
        let target_info = self.spawn_session(
            app,
            definition.id,
            definition.name,
            definition.name.to_string(),
            "gemini --prompt-interactive <handover>".to_string(),
            resolved.executable,
            args,
            cwd,
            rows,
            cols,
            Vec::new(),
        )?;
        let target = self.get(&target_info.id)?;
        self.remember_handover(&target, &prompt);

        Ok(HandoverResult {
            prompt,
            source_session: source_info,
            target_session: target_info,
            mode: "new-session".to_string(),
        })
    }

    fn continue_codex_with_initial_prompt(
        &self,
        app: AppHandle,
        source: &Arc<PtySession>,
        source_info: SessionInfo,
        cwd: String,
        note: Option<String>,
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
        let planned_target = SessionInfo {
            id: "pending".to_string(),
            agent_id: definition.id.to_string(),
            agent_name: definition.name.to_string(),
            title: "Codex new session".to_string(),
            command: "codex --no-alt-screen --disable terminal_resize_reflow <handover>"
                .to_string(),
            cwd: cwd.clone(),
            status: SessionStatus::Running,
            attached: false,
            created_at: unix_timestamp(),
            last_active_at: unix_timestamp(),
            first_user_message: None,
            native_session_ref: None,
        };
        let prompt =
            self.build_compact_handover_prompt_for(source, &source_info, &planned_target, note);

        let handover_path = write_handover_file(&cwd, &prompt)?;
        let startup_prompt = handover_reference_startup_prompt(&handover_path);

        let mut args = resolved.args;
        args.push(startup_prompt);

        let target_info = self.spawn_session(
            app,
            definition.id,
            definition.name,
            definition.name.to_string(),
            "codex --no-alt-screen --disable terminal_resize_reflow <handover>".to_string(),
            resolved.executable,
            args,
            cwd,
            rows,
            cols,
            Vec::new(),
        )?;
        let target = self.get(&target_info.id)?;
        self.remember_handover(&target, &prompt);

        Ok(HandoverResult {
            prompt,
            source_session: source_info,
            target_session: target_info,
            mode: "new-session".to_string(),
        })
    }

    fn continue_copilot_with_initial_prompt(
        &self,
        app: AppHandle,
        source: &Arc<PtySession>,
        source_info: SessionInfo,
        cwd: String,
        note: Option<String>,
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
        let planned_target = SessionInfo {
            id: "pending".to_string(),
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
        };
        let prompt = self.build_compact_handover_prompt_for(
            source,
            &source_info,
            &planned_target,
            note.clone(),
        );

        let handover_path = write_handover_file(&cwd, &prompt)?;
        let startup_prompt = handover_reference_startup_prompt(&handover_path);

        let mut args = resolved.args;
        if let Some(parent) = handover_path.parent() {
            append_copilot_cli_option(&mut args, "--add-dir".to_string());
            append_copilot_cli_option(&mut args, parent.to_string_lossy().into_owned());
        }
        append_copilot_cli_option(&mut args, "-i".to_string());
        append_copilot_cli_option(&mut args, startup_prompt);

        let target_info = self.spawn_session(
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
        )?;
        let target = self.get(&target_info.id)?;
        self.remember_handover(&target, &prompt);

        Ok(HandoverResult {
            prompt,
            source_session: source_info,
            target_session: target_info,
            mode: "new-session".to_string(),
        })
    }

    fn inject_handover(
        &self,
        source: &Arc<PtySession>,
        target: &Arc<PtySession>,
        note: Option<String>,
        target_is_new: bool,
    ) -> Result<String, String> {
        let source_info = source.info();
        let target_info = target.info();
        let prompt = self.build_handover_prompt_for(source, &source_info, &target_info, note);

        let handover_path = write_handover_file(&target_info.cwd, &prompt)?;
        let display_path = handover_path
            .strip_prefix(&target_info.cwd)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| handover_path.display().to_string());
        let short_instruction = format!(
            "A handover context file is referenced at {display_path}. Read only this exact file (no directory listing or glob scanning), acknowledge context loaded, then wait for my next instruction."
        );

        if target_is_new {
            thread::sleep(Duration::from_millis(handover_startup_delay_ms(
                &target_info.agent_id,
            )));
        }
        inject_with_retry(target, &short_instruction)?;
        self.remember_handover(target, &prompt);
        target.meta.lock().last_active_at = unix_timestamp();

        Ok(prompt)
    }

    fn remember_handover(&self, target: &Arc<PtySession>, prompt: &str) {
        *target.inherited_handover.lock() = tail_chars(prompt, HANDOVER_INHERITED_STORE_CHARS);
    }

    fn build_handover_prompt_for(
        &self,
        source: &Arc<PtySession>,
        source_info: &SessionInfo,
        target_info: &SessionInfo,
        note: Option<String>,
    ) -> String {
        let recent_context = clean_terminal_output(&source.ring.lock(), HANDOVER_CONTEXT_CHARS);
        let recent_user_inputs =
            clean_terminal_input(&source.input_ring.lock(), HANDOVER_USER_INPUT_CHARS);
        let inherited_handover = tail_chars(
            &source.inherited_handover.lock(),
            HANDOVER_INHERITED_CONTEXT_CHARS,
        );
        let git_status = git_command(&source_info.cwd, &["status", "--short"])
            .unwrap_or_else(|| "git status unavailable".to_string());
        let git_branch = git_command(&source_info.cwd, &["branch", "--show-current"])
            .unwrap_or_else(|| "unknown".to_string());
        let git_diff = git_command(&source_info.cwd, &["diff"])
            .unwrap_or_else(|| "git diff unavailable".to_string());
        let staged_diff = git_command(&source_info.cwd, &["diff", "--staged"])
            .unwrap_or_else(|| "git staged diff unavailable".to_string());
        build_handover_prompt(
            &source_info,
            &target_info,
            note.as_deref().unwrap_or_default(),
            &git_branch,
            &git_status,
            &git_diff,
            &staged_diff,
            &inherited_handover,
            &recent_context,
            &recent_user_inputs,
        )
    }

    fn build_compact_handover_prompt_for(
        &self,
        source: &Arc<PtySession>,
        source_info: &SessionInfo,
        target_info: &SessionInfo,
        note: Option<String>,
    ) -> String {
        let recent_context =
            clean_terminal_output(&source.ring.lock(), COMPACT_HANDOVER_CONTEXT_CHARS);
        let recent_user_inputs =
            clean_terminal_input(&source.input_ring.lock(), COMPACT_USER_INPUT_CHARS);
        let inherited_handover = tail_chars(
            &source.inherited_handover.lock(),
            COMPACT_HANDOVER_INHERITED_CONTEXT_CHARS,
        );
        let git_status = git_command(&source_info.cwd, &["status", "--short"])
            .unwrap_or_else(|| "git status unavailable".to_string());
        build_compact_handover_prompt(
            source_info,
            target_info,
            note.as_deref().unwrap_or_default(),
            &tail_chars(&git_status, COMPACT_GIT_STATUS_CHARS),
            &inherited_handover,
            &recent_context,
            &recent_user_inputs,
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
                    drop(pending);
                    self.remember_first_user_message(&candidate);
                    return;
                }
                '\u{7f}' | '\u{8}' => {
                    pending.pop();
                }
                '\t' => pending.push(' '),
                _ if !ch.is_control() => pending.push(ch),
                _ => {}
            }
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
                message.content = chunk.to_string();
            } else if !message.content.ends_with(chunk) {
                message.content.push_str(chunk);
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
        }
    }
}

fn default_cwd() -> String {
    env::current_dir()
        .ok()
        .and_then(|path| path.to_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| env::var("HOME").unwrap_or_else(|_| "/".to_string()))
}

fn waypoint_sessions_dir() -> Result<PathBuf, String> {
    let home = env::var("HOME").map_err(|err| format!("failed to resolve HOME: {err}"))?;
    Ok(PathBuf::from(home).join(".waypoint").join("sessions"))
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
                args: &["--no-alt-screen", "--disable", "terminal_resize_reflow"],
                display: "codex --no-alt-screen --disable terminal_resize_reflow",
                verify: VerifyStrategy::CommandExists,
            }],
        },
        AgentDefinition {
            id: "gemini",
            name: "Gemini CLI",
            description: "Google Gemini CLI",
            candidates: &[CommandCandidate {
                executable: "gemini",
                args: &[],
                display: "gemini",
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
        .and_then(|session_ref| session_ref.id.clone());

    let (args, display_command) = match meta.agent_id.as_str() {
        "claude-code" => {
            let mut args = resolved.args;
            if let Some(native_id) = native_id {
                args.push("--resume".to_string());
                args.push(native_id.clone());
                (
                    args,
                    format!("{} --resume {}", resolved.display, shell_quote(&native_id)),
                )
            } else {
                args.push("--continue".to_string());
                (args, format!("{} --continue", resolved.display))
            }
        }
        "codex" => {
            let mut args = resolved.args;
            args.push("resume".to_string());
            if let Some(native_id) = native_id {
                args.push(native_id.clone());
                (
                    args,
                    format!("{} resume {}", resolved.display, shell_quote(&native_id)),
                )
            } else {
                args.push("--last".to_string());
                (args, format!("{} resume --last", resolved.display))
            }
        }
        "gemini" => {
            let mut args = resolved.args;
            args.push("--resume".to_string());
            if let Some(native_id) = native_id {
                args.push(native_id.clone());
                (
                    args,
                    format!("{} --resume {}", resolved.display, shell_quote(&native_id)),
                )
            } else {
                (args, format!("{} --resume", resolved.display))
            }
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
            id: meta
                .native_session_ref
                .as_ref()
                .and_then(|session_ref| session_ref.id.clone()),
            name: meta
                .native_session_ref
                .as_ref()
                .and_then(|session_ref| session_ref.name.clone()),
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

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn write_handover_file(cwd: &str, prompt: &str) -> Result<PathBuf, String> {
    let dir = handover_workspace_dir(cwd)?;
    fs::create_dir_all(&dir).map_err(|err| {
        format!(
            "failed to create handover directory {}: {err}",
            dir.display()
        )
    })?;
    let path = dir.join(format!("handover-{}.md", Uuid::new_v4()));
    fs::write(&path, prompt)
        .map_err(|err| format!("failed to write handover file {}: {err}", path.display()))?;
    Ok(path)
}

fn handover_workspace_dir(cwd: &str) -> Result<PathBuf, String> {
    let home = env::var("HOME").map_err(|err| format!("failed to resolve HOME: {err}"))?;
    let workspace_name = Path::new(cwd)
        .file_name()
        .map(|name| name.to_string_lossy().trim().to_string())
        .filter(|name| !name.is_empty() && name != "." && name != "..")
        .unwrap_or_else(|| "workspace".to_string());

    Ok(PathBuf::from(home).join(".waypoint").join(workspace_name))
}

fn handover_reference_startup_prompt(path: &Path) -> String {
    format!(
        "Initialization step for this new session: read only this exact handover file now: {}. This single-file read is explicitly allowed. Do not list/search directories, do not use glob patterns, and do not read any other files during this initialization turn. After loading that single file, reply exactly: \"Context loaded. Waiting for your instruction.\" and wait for the next user message. Crucially, this constraint applies ONLY to this first startup turn; in all subsequent turns, you must fully use your normal tools, file reading, and directory search capabilities to assist the user.",
        path.display()
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
    git_branch: &str,
    git_status: &str,
    git_diff: &str,
    staged_diff: &str,
    inherited_handover: &str,
    recent_context: &str,
    recent_user_inputs: &str,
) -> String {
    format!(
        r#"# Handover

You are continuing work from another local agent session inside waypoint.

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

## Git Context
- Branch: {git_branch}

### Status
```text
{git_status}
```

### Unstaged Diff
```diff
{git_diff}
```

### Staged Diff
```diff
{staged_diff}
```

## Inherited Handover Context
{inherited_handover}

## Recent Source Terminal Context
```text
{recent_context}
```

## Recent User Inputs (best effort)
```text
{recent_user_inputs}
```

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
        git_branch = empty_fallback(git_branch, "unknown"),
        git_status = empty_fallback(git_status, "clean or unavailable"),
        git_diff = empty_fallback(git_diff, "No unstaged diff."),
        staged_diff = empty_fallback(staged_diff, "No staged diff."),
        inherited_handover = empty_fallback(inherited_handover, "No inherited handover context."),
        recent_context = empty_fallback(recent_context, "No recent terminal context captured."),
        recent_user_inputs = empty_fallback(recent_user_inputs, "No recent user input captured."),
    )
}

fn build_compact_handover_prompt(
    source: &SessionInfo,
    target: &SessionInfo,
    note: &str,
    git_status: &str,
    inherited_handover: &str,
    recent_context: &str,
    recent_user_inputs: &str,
) -> String {
    format!(
        r#"# Waypoint Handover

Continue from the previous local agent session.

## Source
- Agent: {source_agent}
- Title: {source_title}
- Workspace: {source_cwd}

## Target
- Agent: {target_agent}
- Workspace: {target_cwd}

## User Note
{note}

## Current Git Status
```text
{git_status}
```

## Inherited Handover Context
{inherited_handover}

## Recent Source Context
```text
{recent_context}
```

## Recent User Inputs (best effort)
```text
{recent_user_inputs}
```

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
        git_status = empty_fallback(git_status, "clean or unavailable"),
        inherited_handover = empty_fallback(inherited_handover, "No inherited handover context."),
        recent_context = empty_fallback(recent_context, "No recent terminal context captured."),
        recent_user_inputs = empty_fallback(recent_user_inputs, "No recent user input captured."),
    )
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
    let stripped = strip_ansi(raw);
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

    collapse_blank_lines(&cleaned_lines.join("\n"), 2)
        .trim_end()
        .to_string()
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

    // Common TUI status / interactive UI keywords
    if normalized.contains("esctointerrupt")
        || normalized.contains("forshortcuts")
        || normalized.contains("swirling")
        || normalized.contains("thundering")
        || normalized.contains("releasenotes")
        || normalized.contains("welcomeback")
        || normalized.contains("claudecode")
        || normalized.contains("apiusagebilling")
        || normalized.contains("whatsnew")
        || normalized.contains("tipsforgettingstarted")
        || normalized.contains("alternatescreen")
        || normalized.contains("ctrlv")
        || normalized.contains("pasting")
        || normalized.contains("effort")
        || normalized.contains("mcpserver")
        || normalized.contains("mcpneedsauth")
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
    Some(tail_chars(&stdout, GIT_OUTPUT_LIMIT_CHARS))
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
    let stripped = strip_ansi(raw);

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

    // Step 3: remove TUI spinner / decoration characters
    let spinner_chars: &[char] = &[
        '✢', '✳', '✶', '✻', '✽', '⏺', '⠂', '⠐',
        '·', // middle dot used as separator in TUI status bars
    ];
    let clean_lines: Vec<String> = clean_lines
        .into_iter()
        .map(|line| {
            let trimmed = line.trim();
            // If the line is ONLY spinner/decoration chars (possibly with spaces), skip it
            if !trimmed.is_empty()
                && trimmed
                    .chars()
                    .all(|c| spinner_chars.contains(&c) || c == ' ')
            {
                String::new()
            } else {
                line
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

    // Step 5: truncate to last `limit` chars
    tail_chars(&result, limit)
}

fn clean_terminal_input(raw: &str, limit: usize) -> String {
    let stripped = strip_ansi(raw);
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
}

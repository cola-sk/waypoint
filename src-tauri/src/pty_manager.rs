use std::{
    collections::HashMap,
    env,
    io::{Read, Write},
    path::PathBuf,
    process::Command,
    sync::Arc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

const RING_LIMIT_CHARS: usize = 200_000;
const HANDOVER_CONTEXT_CHARS: usize = 40_000;
const GIT_OUTPUT_LIMIT_CHARS: usize = 60_000;
const HANDOVER_INJECT_ATTEMPTS: usize = 8;
const HANDOVER_INJECT_DELAY_MS: u64 = 350;
const CODEX_HANDOVER_STARTUP_DELAY_MS: u64 = 1_800;

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
    ring: Mutex<String>,
}

#[derive(Clone)]
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
}

#[derive(Clone, Serialize)]
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

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PtyDataEvent {
    session_id: String,
    data: String,
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
pub fn create_shell_session(
    state: State<'_, AppState>,
    app: AppHandle,
    title: Option<String>,
    cwd: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<SessionInfo, String> {
    state.manager.create_shell_session(app, title, cwd, rows, cols)
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
    if agent_id == "shell" {
        return state
            .manager
            .create_shell_session(app, Some("Shell".to_string()), Some(cwd), rows, cols);
    }
    state
        .manager
        .create_agent_session(app, &agent_id, cwd, rows, cols)
}

#[tauri::command]
pub fn list_agent_presets() -> Vec<AgentPresetInfo> {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let mut presets = agent_definitions()
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
        .collect::<Vec<_>>();
    presets.push(AgentPresetInfo {
        id: "shell".to_string(),
        name: "Shell".to_string(),
        description: "System login shell".to_string(),
        available: true,
        command: shell.clone(),
        resolved_command: Some(shell),
    });
    presets
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

impl SessionManager {
    fn create_shell_session(
        &self,
        app: AppHandle,
        title: Option<String>,
        cwd: Option<String>,
        rows: Option<u16>,
        cols: Option<u16>,
    ) -> Result<SessionInfo, String> {
        let command = env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let cwd = cwd.unwrap_or_else(default_cwd);
        let mut args = Vec::new();
        if command.ends_with("zsh") || command.ends_with("bash") {
            args.push("-l".to_string());
        }
        self.spawn_session(
            app,
            "shell",
            "Shell",
            title.unwrap_or_else(|| "Shell".to_string()),
            command.clone(),
            command,
            args,
            cwd,
            rows,
            cols,
        )
    }

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
    ) -> Result<SessionInfo, String> {
        let cwd_path = PathBuf::from(&cwd);
        if !cwd_path.is_dir() {
            return Err(format!("workspace directory does not exist: {cwd}"));
        }

        let id = Uuid::new_v4().to_string();
        let now = unix_timestamp();
        let session_title = format!("{title} {}", &id[..8]);

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: rows.unwrap_or(30),
                cols: cols.unwrap_or(100),
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
            created_at: now,
            last_active_at: now,
        };

        let session = Arc::new(PtySession {
            meta: Mutex::new(meta),
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
            ring: Mutex::new(String::new()),
        });

        self.sessions.lock().insert(id.clone(), session.clone());

        let reader_session = session.clone();
        let reader_id = id.clone();
        let reader_app = app.clone();
        thread::spawn(move || {
            let mut buf = [0_u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
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
                        reader_session.append_ring(&data);
                        let _ = reader_app.emit(
                            "pty:data",
                            PtyDataEvent {
                                session_id: reader_id.clone(),
                                data,
                            },
                        );
                    }
                    Err(err) => {
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
        let mut sessions = self
            .sessions
            .lock()
            .values()
            .map(|session| session.info())
            .collect::<Vec<_>>();
        sessions.sort_by_key(|session| session.created_at);
        sessions
    }

    fn attach_session(&self, session_id: &str) -> Result<SessionSnapshot, String> {
        let session = self.get(session_id)?;
        {
            let mut meta = session.meta.lock();
            meta.attached = true;
            meta.last_active_at = unix_timestamp();
        }
        let replay = session.ring.lock().clone();
        Ok(SessionSnapshot {
            session: session.info(),
            replay,
        })
    }

    fn detach_session(&self, session_id: &str) -> Result<(), String> {
        let session = self.get(session_id)?;
        let mut meta = session.meta.lock();
        meta.attached = false;
        meta.last_active_at = unix_timestamp();
        Ok(())
    }

    fn write_session(&self, session_id: &str, data: String) -> Result<(), String> {
        let session = self.get(session_id)?;
        session
            .writer
            .lock()
            .write_all(data.as_bytes())
            .map_err(|err| format!("failed to write to PTY: {err}"))?;
        session.meta.lock().last_active_at = unix_timestamp();
        Ok(())
    }

    fn resize_session(&self, session_id: &str, rows: u16, cols: u16) -> Result<(), String> {
        let session = self.get(session_id)?;
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
            return Err(format!("source session is not running: {}", source_info.title));
        }
        if !matches!(target_info.status, SessionStatus::Running) {
            return Err(format!("target session is not running: {}", target_info.title));
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
            return Err(format!("source session is not running: {}", source_info.title));
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

        let target_info = if target_agent_id == "shell" {
            self.create_shell_session(
                app,
                Some("Shell Continue".to_string()),
                Some(cwd),
                rows,
                cols,
            )?
        } else {
            self.create_agent_session(app, target_agent_id, cwd, rows, cols)?
        };
        let target = self.get(&target_info.id)?;
        let prompt = self.inject_handover(&source, &target, note, true)?;

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
        };
        let prompt = self.build_handover_prompt_for(source, &source_info, &planned_target, note);
        let mut args = resolved.args;
        args.push("--prompt-interactive".to_string());
        args.push(prompt.clone());
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
        )?;

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

        if target_is_new {
            thread::sleep(Duration::from_millis(handover_startup_delay_ms(
                &target_info.agent_id,
            )));
        }
        inject_with_retry(target, &prompt)?;
        target.meta.lock().last_active_at = unix_timestamp();

        Ok(prompt)
    }

    fn build_handover_prompt_for(
        &self,
        source: &Arc<PtySession>,
        source_info: &SessionInfo,
        target_info: &SessionInfo,
        note: Option<String>,
    ) -> String {
        let recent_context = tail_chars(&source.ring.lock(), HANDOVER_CONTEXT_CHARS);
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
            &recent_context,
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
        }
    }
}

fn default_cwd() -> String {
    env::current_dir()
        .ok()
        .and_then(|path| path.to_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| env::var("HOME").unwrap_or_else(|_| "/".to_string()))
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
        VerifyStrategy::ShellHelp(command) => run_login_shell_status(&format!(
            "{} >/dev/null 2>&1",
            command
        )),
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
    recent_context: &str,
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

## Recent Source Terminal Context
```text
{recent_context}
```

## Instructions
- Continue from the current workspace state.
- Do not revert unrelated user changes.
- Preserve existing user edits.
- Ask before destructive operations.
- Start by briefly acknowledging what you understand, then continue the next useful step.
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
        recent_context = empty_fallback(recent_context, "No recent terminal context captured."),
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

fn git_command(cwd: &str, args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).current_dir(cwd).output().ok()?;
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

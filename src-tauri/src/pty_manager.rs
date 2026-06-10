use std::{
    collections::HashMap,
    env,
    io::{Read, Write},
    path::PathBuf,
    sync::Arc,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

const RING_LIMIT_CHARS: usize = 200_000;

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
    Starting,
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

impl SessionManager {
    fn create_shell_session(
        &self,
        app: AppHandle,
        title: Option<String>,
        cwd: Option<String>,
        rows: Option<u16>,
        cols: Option<u16>,
    ) -> Result<SessionInfo, String> {
        let id = Uuid::new_v4().to_string();
        let command = env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let cwd = cwd.unwrap_or_else(default_cwd);
        let now = unix_timestamp();
        let session_title = title.unwrap_or_else(|| format!("Shell {}", &id[..8]));

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: rows.unwrap_or(30),
                cols: cols.unwrap_or(100),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| format!("failed to open PTY: {err}"))?;

        let mut cmd = CommandBuilder::new(&command);
        if command.ends_with("zsh") || command.ends_with("bash") {
            cmd.arg("-l");
        }
        cmd.cwd(PathBuf::from(&cwd));

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|err| format!("failed to spawn shell: {err}"))?;
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
            title: session_title,
            command: command.clone(),
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
        thread::spawn(move || {
            let mut buf = [0_u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        reader_session.mark_status(SessionStatus::Exited);
                        let _ = app.emit(
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
                        let _ = app.emit(
                            "pty:data",
                            PtyDataEvent {
                                session_id: reader_id.clone(),
                                data,
                            },
                        );
                    }
                    Err(err) => {
                        reader_session.mark_status(SessionStatus::Error);
                        let _ = app.emit(
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
        Ok(SessionSnapshot {
            session: session.info(),
            replay: session.ring.lock().clone(),
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

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}


#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod zmodem;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Mutex;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{Manager, State};

struct PtySession {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    // the child itself is owned by the waiter thread (see pty_spawn); we keep a
    // killer handle so pty_kill still works
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    // native ZMODEM engine — sees every byte from/to this pty
    zm: zmodem::Zm,
}

#[derive(Clone, Serialize)]
struct ZmOffer {
    id: String,
    name: String,
    size: u64,
}

#[derive(Clone, Serialize)]
struct ZmProgress {
    id: String,
    name: String,
    done: u64,
    total: u64,
    dir: String,
}

#[derive(Clone, Serialize)]
struct ZmNote {
    id: String,
    text: String,
}

// Apply the engine's Event list: writes go to the pty, terminal-bound bytes and
// UI notifications are collected into `emits` (emitted AFTER the lock is freed).
enum Emit {
    Data(Vec<u8>),
    Offer { name: String, size: u64 },
    SendReady,
    Progress { name: String, done: u64, total: u64, dir: String },
    Done { name: String },
    Error(String),
    Finished,
}

fn apply_zm_events(sess: &mut PtySession, events: Vec<zmodem::Event>, emits: &mut Vec<Emit>) {
    for ev in events {
        match ev {
            zmodem::Event::Write(w) => {
                let _ = sess.writer.write_all(&w);
                let _ = sess.writer.flush();
            }
            zmodem::Event::Forward(d) => emits.push(Emit::Data(d)),
            zmodem::Event::RecvOffer { name, size } => emits.push(Emit::Offer { name, size }),
            zmodem::Event::SendReady => emits.push(Emit::SendReady),
            zmodem::Event::Progress { name, done, total, dir } => {
                emits.push(Emit::Progress { name, done, total, dir: dir.to_string() })
            }
            zmodem::Event::Done { name, .. } => emits.push(Emit::Done { name }),
            zmodem::Event::Error(e) => emits.push(Emit::Error(e)),
            zmodem::Event::Finished => emits.push(Emit::Finished),
        }
    }
    // Drive the send pump until write buffers are handed off.
    while sess.zm.needs_pump() {
        let mut more = Vec::new();
        let cont = sess.zm.pump(&mut more);
        apply_pump(sess, more, emits);
        if !cont {
            break;
        }
    }
}

fn apply_pump(sess: &mut PtySession, events: Vec<zmodem::Event>, emits: &mut Vec<Emit>) {
    for ev in events {
        match ev {
            zmodem::Event::Write(w) => {
                let _ = sess.writer.write_all(&w);
                let _ = sess.writer.flush();
            }
            zmodem::Event::Progress { name, done, total, dir } => {
                emits.push(Emit::Progress { name, done, total, dir: dir.to_string() })
            }
            zmodem::Event::Done { name, .. } => emits.push(Emit::Done { name }),
            zmodem::Event::Error(e) => emits.push(Emit::Error(e)),
            zmodem::Event::Finished => emits.push(Emit::Finished),
            _ => {}
        }
    }
}

fn flush_emits(app: &tauri::AppHandle, id: &str, emits: Vec<Emit>) {
    for e in emits {
        match e {
            Emit::Data(d) => {
                let _ = app.emit_all("pty://data", PtyData { id: id.to_string(), data: d });
            }
            Emit::Offer { name, size } => {
                let _ = app.emit_all("zmodem://offer", ZmOffer { id: id.to_string(), name, size });
            }
            Emit::SendReady => {
                let _ = app.emit_all("zmodem://send-ready", PtyExit { id: id.to_string() });
            }
            Emit::Progress { name, done, total, dir } => {
                let _ = app.emit_all("zmodem://progress", ZmProgress { id: id.to_string(), name, done, total, dir });
            }
            Emit::Done { name } => {
                let _ = app.emit_all("zmodem://done", ZmNote { id: id.to_string(), text: name });
            }
            Emit::Error(text) => {
                let _ = app.emit_all("zmodem://error", ZmNote { id: id.to_string(), text });
            }
            Emit::Finished => {
                let _ = app.emit_all("zmodem://finished", PtyExit { id: id.to_string() });
            }
        }
    }
}

// Arc so async commands can move a clone into spawn_blocking: every PTY
// command runs off the main event loop, because a blocking write into a full
// pty buffer (zmodem uploads!) would otherwise freeze the whole UI.
#[derive(Default, Clone)]
struct PtyState(std::sync::Arc<Mutex<HashMap<String, PtySession>>>);

type PtyMap = std::sync::Arc<Mutex<HashMap<String, PtySession>>>;

#[derive(Clone, Serialize)]
struct PtyData {
    id: String,
    data: Vec<u8>,
}

#[derive(Clone, Serialize)]
struct PtyExit {
    id: String,
}

fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
    }
}

#[tauri::command]
async fn pty_spawn(
    id: String,
    cols: u16,
    rows: u16,
    program: Option<String>,
    args: Option<Vec<String>>,
    app: tauri::AppHandle,
    state: State<'_, PtyState>,
) -> Result<(), String> {
    let st = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        pty_spawn_blocking(id, cols, rows, program, args, app, st)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn pty_spawn_blocking(
    id: String,
    cols: u16,
    rows: u16,
    program: Option<String>,
    args: Option<Vec<String>>,
    app: tauri::AppHandle,
    st: PtyMap,
) -> Result<(), String> {
    {
        let map = st.lock().unwrap();
        if map.contains_key(&id) {
            return Ok(());
        }
    }

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;

    // program set (e.g. "ssh") → run it directly; otherwise the default shell.
    let mut cmd = match program {
        Some(p) if !p.is_empty() => {
            let mut c = CommandBuilder::new(p);
            if let Some(a) = args {
                for x in a {
                    c.arg(x);
                }
            }
            c
        }
        _ => CommandBuilder::new(default_shell()),
    };
    if !cfg!(windows) {
        cmd.env("TERM", "xterm-256color");
    }
    if let Some(home) = dirs_home() {
        cmd.cwd(home);
    }

    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);
    let killer = child.clone_killer();

    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

    let app2 = app.clone();
    let id2 = id.clone();
    let st_reader = st.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    // Feed the ZMODEM engine; it forwards ordinary bytes to the
                    // terminal and handles rz/sz handshakes/data itself. If the
                    // session isn't in the map yet (race at spawn), fall back to
                    // forwarding raw.
                    let mut emits: Vec<Emit> = Vec::new();
                    let mut handled = false;
                    if let Ok(mut map) = st_reader.lock() {
                        if let Some(sess) = map.get_mut(&id2) {
                            let mut events = Vec::new();
                            sess.zm.feed(&buf[..n], &mut events);
                            apply_zm_events(sess, events, &mut emits);
                            handled = true;
                        }
                    }
                    if handled {
                        flush_emits(&app2, &id2, emits);
                    } else {
                        let _ = app2.emit_all("pty://data", PtyData { id: id2.clone(), data: buf[..n].to_vec() });
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Exit detection lives here, NOT on the reader hitting EOF: on Windows the
    // ConPTY master stays readable after the child exits, so the reader would
    // block forever and "pty://exit" would never fire — which is what stopped
    // the disconnect notice (and Enter-to-reconnect) from ever appearing.
    let app3 = app.clone();
    let id3 = id.clone();
    std::thread::spawn(move || {
        let _ = child.wait();
        let _ = app3.emit_all("pty://exit", PtyExit { id: id3 });
    });

    let mut map = st.lock().unwrap();
    map.insert(
        id,
        PtySession {
            writer,
            master: pair.master,
            killer,
            zm: zmodem::Zm::new(),
        },
    );
    Ok(())
}

// ── ZMODEM commands (frontend → engine) ──

#[tauri::command]
async fn zmodem_accept(
    id: String,
    path: String,
    app: tauri::AppHandle,
    state: State<'_, PtyState>,
) -> Result<(), String> {
    let st = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut emits = Vec::new();
        if let Ok(mut map) = st.lock() {
            if let Some(sess) = map.get_mut(&id) {
                let mut events = Vec::new();
                sess.zm.accept_receive(std::path::PathBuf::from(path), &mut events);
                apply_zm_events(sess, events, &mut emits);
            }
        }
        flush_emits(&app, &id, emits);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn zmodem_send(
    id: String,
    paths: Vec<String>,
    app: tauri::AppHandle,
    state: State<'_, PtyState>,
) -> Result<(), String> {
    let st = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut emits = Vec::new();
        if let Ok(mut map) = st.lock() {
            if let Some(sess) = map.get_mut(&id) {
                let mut events = Vec::new();
                let pbs = paths.into_iter().map(std::path::PathBuf::from).collect();
                sess.zm.start_send(pbs, &mut events);
                apply_zm_events(sess, events, &mut emits);
            }
        }
        flush_emits(&app, &id, emits);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn zmodem_cancel(
    id: String,
    app: tauri::AppHandle,
    state: State<'_, PtyState>,
) -> Result<(), String> {
    let st = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut emits = Vec::new();
        if let Ok(mut map) = st.lock() {
            if let Some(sess) = map.get_mut(&id) {
                let mut events = Vec::new();
                sess.zm.cancel(&mut events);
                apply_zm_events(sess, events, &mut emits);
            }
        }
        flush_emits(&app, &id, emits);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn pty_write(id: String, data: String, state: State<'_, PtyState>) -> Result<(), String> {
    let st = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut map = st.lock().unwrap();
        if let Some(s) = map.get_mut(&id) {
            s.writer
                .write_all(data.as_bytes())
                .map_err(|e| e.to_string())?;
            s.writer.flush().map_err(|e| e.to_string())?;
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn pty_write_bytes(
    id: String,
    data: Vec<u8>,
    state: State<'_, PtyState>,
) -> Result<(), String> {
    let st = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut map = st.lock().unwrap();
        if let Some(s) = map.get_mut(&id) {
            s.writer.write_all(&data).map_err(|e| e.to_string())?;
            s.writer.flush().map_err(|e| e.to_string())?;
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn pty_resize(
    id: String,
    cols: u16,
    rows: u16,
    state: State<'_, PtyState>,
) -> Result<(), String> {
    let st = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let map = st.lock().unwrap();
        if let Some(s) = map.get(&id) {
            s.master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn pty_kill(id: String, state: State<'_, PtyState>) -> Result<(), String> {
    let st = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut map = st.lock().unwrap();
        if let Some(mut s) = map.remove(&id) {
            let _ = s.killer.kill();
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

// Read an arbitrary local file (used to upload a dropped file over zmodem).
// Async for the same reason as run_command: large files must not block the UI.
#[tauri::command]
async fn read_file(path: String) -> Result<Vec<u8>, String> {
    tauri::async_runtime::spawn_blocking(move || std::fs::read(&path).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}

// Write a file received over zmodem (sz) to the path the user picked.
#[tauri::command]
async fn write_file(path: String, data: Vec<u8>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        std::fs::write(&path, &data).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn exe_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

// Settings live next to the executable so a portable/green copy carries its own
// config. If that directory isn't writable (installed under Program Files), fall
// back to the user profile instead of failing.
fn default_cfg_path() -> std::path::PathBuf {
    if let Some(d) = exe_dir() {
        let probe = d.join(".wireline_write_test");
        if std::fs::write(&probe, b"").is_ok() {
            let _ = std::fs::remove_file(&probe);
            return d.join("wireline.config.json");
        }
    }
    dirs_home()
        .map(|h| h.join(".wireline").join("wireline.config.json"))
        .unwrap_or_else(|| std::path::PathBuf::from("wireline.config.json"))
}

fn resolve_cfg(path: Option<String>) -> std::path::PathBuf {
    match path {
        Some(p) if !p.trim().is_empty() => std::path::PathBuf::from(p),
        _ => default_cfg_path(),
    }
}

#[tauri::command]
fn config_default_path() -> String {
    default_cfg_path().to_string_lossy().into_owned()
}

#[tauri::command]
fn config_read(path: Option<String>) -> Result<String, String> {
    let p = resolve_cfg(path);
    Ok(std::fs::read_to_string(&p).unwrap_or_default())
}

#[tauri::command]
fn config_write(path: Option<String>, data: String) -> Result<String, String> {
    let p = resolve_cfg(path);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&p, data).map_err(|e| e.to_string())?;
    Ok(p.to_string_lossy().into_owned())
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
}

#[derive(Clone, Serialize)]
struct CmdOut {
    code: i32,
    stdout: String,
    stderr: String,
}

// GUI apps on macOS/Linux inherit a minimal PATH (no /usr/local/bin etc.), so a
// bare program name like `opencode` won't resolve when launched from Finder.
// Search PATH plus the usual install locations and return an absolute path.
#[cfg(not(windows))]
fn resolve_program(program: &str) -> String {
    if program.contains('/') {
        return program.to_string();
    }
    let mut dirs: Vec<std::path::PathBuf> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .collect();
    for d in ["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin", "/bin"] {
        dirs.push(std::path::PathBuf::from(d));
    }
    if let Some(home) = dirs_home() {
        for d in [
            ".opencode/bin",
            ".local/bin",
            ".bun/bin",
            ".npm-global/bin",
            ".cargo/bin",
        ] {
            dirs.push(home.join(d));
        }
    }
    for d in dirs {
        let p = d.join(program);
        if p.is_file() {
            return p.to_string_lossy().into_owned();
        }
    }
    program.to_string()
}

// Run a process to completion, feeding `input` on stdin (so we avoid shell
// quoting), returning its output. Used to drive the installed `opencode` CLI.
//
// MUST be async: a sync #[tauri::command] executes on the main event loop, and
// blocking there for the whole opencode run froze the entire UI. The wait now
// happens on a worker thread while the window keeps painting and responding.
#[tauri::command]
async fn run_command(
    program: String,
    args: Vec<String>,
    input: Option<String>,
    cwd: Option<String>,
) -> Result<CmdOut, String> {
    tauri::async_runtime::spawn_blocking(move || run_command_blocking(program, args, input, cwd))
        .await
        .map_err(|e| e.to_string())?
}

fn run_command_blocking(
    program: String,
    args: Vec<String>,
    input: Option<String>,
    cwd: Option<String>,
) -> Result<CmdOut, String> {
    use std::process::{Command, Stdio};
    let mut cmd = if cfg!(windows) {
        // npm-global CLIs are .cmd shims on Windows → run through cmd.exe
        let mut c = Command::new("cmd");
        c.arg("/C").arg(&program);
        c.args(&args);
        c
    } else {
        #[cfg(not(windows))]
        let resolved = resolve_program(&program);
        #[cfg(windows)]
        let resolved = program.clone();
        let mut c = Command::new(&resolved);
        c.args(&args);
        // make sure the child (and its own subprocesses, e.g. node) can find
        // tools in the usual install dirs even under the Finder-launched PATH
        let base = std::env::var("PATH").unwrap_or_default();
        let mut extra = String::from("/usr/local/bin:/opt/homebrew/bin");
        if let Some(home) = dirs_home() {
            for d in [".opencode/bin", ".local/bin", ".bun/bin", ".npm-global/bin"] {
                extra.push(':');
                extra.push_str(&home.join(d).to_string_lossy());
            }
        }
        c.env(
            "PATH",
            if base.is_empty() {
                extra
            } else {
                format!("{}:{}", base, extra)
            },
        );
        c
    };
    match cwd {
        Some(d) if !d.is_empty() => {
            cmd.current_dir(d);
        }
        _ => {
            if let Some(home) = dirs_home() {
                cmd.current_dir(home);
            }
        }
    }
    // Windows: don't flash a console window for the child (CREATE_NO_WINDOW).
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    if let Some(inp) = input {
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(inp.as_bytes());
        }
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    Ok(CmdOut {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    })
}

fn main() {
    tauri::Builder::default()
        .manage(PtyState::default())
        .invoke_handler(tauri::generate_handler![
            pty_spawn, pty_write, pty_write_bytes, pty_resize, pty_kill, run_command, read_file,
            write_file, config_default_path, config_read, config_write,
            zmodem_accept, zmodem_send, zmodem_cancel
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

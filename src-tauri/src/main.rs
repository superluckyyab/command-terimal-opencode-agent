#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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
}

#[derive(Default)]
struct PtyState(Mutex<HashMap<String, PtySession>>);

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
fn pty_spawn(
    id: String,
    cols: u16,
    rows: u16,
    program: Option<String>,
    args: Option<Vec<String>>,
    app: tauri::AppHandle,
    state: State<PtyState>,
) -> Result<(), String> {
    {
        let map = state.0.lock().unwrap();
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
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = app2.emit_all(
                        "pty://data",
                        PtyData {
                            id: id2.clone(),
                            data: buf[..n].to_vec(),
                        },
                    );
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

    let mut map = state.0.lock().unwrap();
    map.insert(
        id,
        PtySession {
            writer,
            master: pair.master,
            killer,
        },
    );
    Ok(())
}

#[tauri::command]
fn pty_write(id: String, data: String, state: State<PtyState>) -> Result<(), String> {
    let mut map = state.0.lock().unwrap();
    if let Some(s) = map.get_mut(&id) {
        s.writer
            .write_all(data.as_bytes())
            .map_err(|e| e.to_string())?;
        s.writer.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn pty_write_bytes(id: String, data: Vec<u8>, state: State<PtyState>) -> Result<(), String> {
    let mut map = state.0.lock().unwrap();
    if let Some(s) = map.get_mut(&id) {
        s.writer.write_all(&data).map_err(|e| e.to_string())?;
        s.writer.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn pty_resize(id: String, cols: u16, rows: u16, state: State<PtyState>) -> Result<(), String> {
    let map = state.0.lock().unwrap();
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
}

#[tauri::command]
fn pty_kill(id: String, state: State<PtyState>) -> Result<(), String> {
    let mut map = state.0.lock().unwrap();
    if let Some(mut s) = map.remove(&id) {
        let _ = s.killer.kill();
    }
    Ok(())
}

// Read an arbitrary local file (used to upload a dropped file over zmodem).
#[tauri::command]
fn read_file(path: String) -> Result<Vec<u8>, String> {
    std::fs::read(&path).map_err(|e| e.to_string())
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
#[tauri::command]
fn run_command(
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
            pty_spawn, pty_write, pty_write_bytes, pty_resize, pty_kill, run_command, read_file
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

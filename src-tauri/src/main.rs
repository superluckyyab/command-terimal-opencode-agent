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
    child: Box<dyn portable_pty::Child + Send + Sync>,
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

    let mut cmd = CommandBuilder::new(default_shell());
    if !cfg!(windows) {
        cmd.env("TERM", "xterm-256color");
    }
    if let Some(home) = dirs_home() {
        cmd.cwd(home);
    }

    let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);

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
        let _ = app2.emit_all("pty://exit", PtyExit { id: id2.clone() });
    });

    let mut map = state.0.lock().unwrap();
    map.insert(
        id,
        PtySession {
            writer,
            master: pair.master,
            child,
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
        let _ = s.child.kill();
    }
    Ok(())
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
}

fn main() {
    tauri::Builder::default()
        .manage(PtyState::default())
        .invoke_handler(tauri::generate_handler![
            pty_spawn, pty_write, pty_resize, pty_kill
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

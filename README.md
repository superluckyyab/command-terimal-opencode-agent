# Wireline — terminal manager with an AI Operator

A WindTerm-style terminal workbench with a built-in AI Operator. The UI lives in
`Wireline App.dc.html` (session tree, terminal tabs, split view, SFTP panel,
command history/palette, timed sender, one-key login, quick commands, settings,
MCP/skills/workflows, timers, dark theme). It is packaged as a native Windows
desktop app with Tauri, following the same setup as `../web-agent`.

## Real terminal

Inside the desktop app the terminal is a **real shell** — `xterm.js` (the same
engine Tabby uses) on the frontend, driven by a native PTY on the Rust side
(`portable-pty`, which uses Windows ConPTY). It spawns the platform default
shell (`cmd.exe` on Windows, `$SHELL` on Unix). In a plain browser (no Tauri)
the app falls back to the scripted demo terminal, so the design preview keeps
working.

### SSH

Real SSH works by running the OS `ssh` client inside the PTY (Windows 10/11 ship
OpenSSH; on Unix it uses your `ssh`). The terminal follows the active tab:

- Open a **New Session → SSH** with a real **host / port / user** (and pick a
  credential if you like) → the terminal runs `ssh [-p port] user@host` and you
  get a real, interactive session (password / host-key prompts happen right in
  the terminal; key auth uses your `~/.ssh`).
- The built-in demo sessions (`web-01`, …) have no real address, so they open a
  local shell instead — create your own session to actually connect.

Ceiling (future work): one live PTY at a time — switching tabs reconnects, so
background sessions don't persist yet. Serial/Telnet transports aren't wired.

## AI Operator

- **No key configured** → the Operator runs the built-in scripted playbooks
  (the demo experience).
- **API base URL + key set** (Settings → *AI Operator settings*) → the Operator
  talks to a real **OpenAI-compatible** endpoint (`{base}/chat/completions`):
  OpenAI, DeepSeek, Qwen, a local Ollama/LM Studio, etc. The key is stored only
  in `localStorage` on the machine.

## Build a Windows green (portable) build via GitHub Actions

Push a tag and the workflow builds on `windows-latest`:

```bash
git tag v1.0.0
git push origin v1.0.0
```

`.github/workflows/release.yml` produces and attaches to the GitHub Release:

- `Wireline_*_x64-setup.exe` — NSIS installer
- `Wireline_portable_*_x64.zip` — **green build**: unzip and run `Wireline.exe`,
  no installation needed

You can also trigger it manually (**Actions → Build Windows → Run workflow**);
manual runs upload the `.exe` as a build artifact instead of a release.

## Local build

```bash
npm install
npm run build          # assembles dist/ (offline, no CDN)
npm run tauri build    # needs Rust + the Tauri prerequisites
```

`build.js` inlines the app's `<x-dc>` markup into `dist/index.html` and wires in
locally-vendored React (`vendor/`) plus `support.js` and the design-system CSS,
so the packaged app runs with no network.

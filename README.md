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

- Open a **New Session → SSH** and enter the **host / port / username** →
  the terminal runs `ssh [-p port] user@host`. The password and first-connect
  host-key confirmation are entered right in the terminal; key auth uses your
  `~/.ssh`. There is no separate credential store — the real `ssh` client
  handles auth.
- The app starts with no sessions; the terminal shows a local shell until you
  create or quick-connect to one.

Ceiling (future work): one live PTY at a time — switching tabs reconnects, so
background sessions don't persist yet. Serial/Telnet transports aren't wired.

### Credentials

New Session takes **username + password**, or a saved **OneKey** (name / user /
password, stored in `localStorage`, managed via **Tool → OneKeys…**, or tick
"Save these credentials as a OneKey"). A stored password is sent automatically
when ssh prompts for it; leave it blank to type it in the terminal.

### Terminal extras

- **Sender** (timed data sending) writes to the live PTY, so lines actually
  reach the shell/host.
- **IP quick-color**: IPv4 addresses in normal output are highlighted (WindTerm
  style). On by default (`ipColor`).
- **Drag & drop** an OS file onto the window → its path is pasted into the
  terminal (useful for local shells; for uploads see below).
- **AI Operator** reads the current terminal output as context, and can run
  commands in the real terminal — approved playbook steps execute in the PTY,
  and a model reply with a fenced command shows a **▶ Run in terminal** button.

### rz / sz (zmodem)

Zmodem transfers work in the terminal via `zmodem.js`: all PTY output is fed
through a `Zmodem.Sentry`, and a Rust binary channel (`pty_write_bytes`) sends
transfer bytes back.

- **`sz <file>`** on the remote → the app saves the file(s) to disk.
- **`rz`** on the remote → a file picker opens; the chosen local file(s) are
  uploaded.

Note: downloads use the WebView download path and uploads use a native file
picker — both are WebView2 features on Windows.

## AI Operator

Pick the engine from the model dropdown in the Operator panel:

- **`opencode`** (default) → drives the **`opencode` CLI installed on the
  machine** (`opencode run --auto`). The current terminal output and recent chat
  are piped to it as context, and a command in its reply gets a **▶ Run in
  terminal** button — so opencode reads and writes your terminal. Requires
  `opencode` installed and authenticated (`npm i -g opencode-ai`,
  `opencode auth login`). `--auto` auto-approves opencode's own tool use.
- **An OpenAI-compatible model** → set API base URL + key in Settings; the
  Operator calls `{base}/chat/completions` (OpenAI, DeepSeek, Qwen, local
  Ollama/LM Studio…). Key stored only in `localStorage`.
- **Otherwise** → built-in scripted playbooks (the demo experience).

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

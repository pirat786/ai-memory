# AI Memory (`aimem` + Tauri Desktop)

**English** · [Русский](README.ru.md)

Unified local-first memory for AI coding agents (**Antigravity**, **Claude Code**, **Codex**, **Cursor**, **Gemini CLI**) without MCP — powered by a shared SQLite + FTS5 database, a **Tauri 2** desktop app (window + system tray + auto-updater + background sync), and cross-machine synchronization via **Google Drive (`drive.appdata`)**.

## Features

- **No MCP required:** Local agents interact via the fast `aimem` CLI (`aimem context`, `aimem add`, `aimem search`).
- **Smart project detection:** Automatically identifies projects by normalized `git remote origin` (matching HTTPS and SSH remotes identically), so the same repo on different PCs shares the same memory.
- **Full-text search (SQLite FTS5):** Instant search across architectural decisions, bug fixes, notes, and global preferences.
- **Tauri 2 Desktop App (`ai-memory.exe`):** Native window, system tray, automatic `PATH` & agent rules setup on launch, background sync every 5 minutes, and one-click auto-updates from GitHub Releases.
- **Universal Importer:** Imports existing memories from `~/.claude`, `~/.gemini`, `~/.codex`, `~/.cursor`, and project `README.md` / `CLAUDE.md` / `AGENTS.md` / `docs/ARCHITECTURE.md` files.
- **Google Drive Sync (`drive.appdata`):** Last-write-wins merge per memory UUID (`updated_at` and soft-delete tombstones).

## Project Structure

- `crates/aimem-core` — shared core: SQLite + FTS5, Git project detection, Google Drive sync, rule installer, universal importer.
- `crates/aimem-cli` — fast `aimem` CLI for AI agents and terminal use.
- `app/` — Tauri 2 desktop application (`ai-memory`) with system tray and auto-updater.
- `ui/` — desktop frontend (`index.html`).
- `docs/` — GitHub Pages landing page, Privacy Policy, and Terms of Service.

## Build from Source

```powershell
cargo test --workspace
cargo build --release --workspace
```

Built binaries:
- `target/release/aimem.exe` — CLI for AI agents.
- `target/release/ai-memory.exe` — desktop app with system tray.

## Quick Start

1. Launch `ai-memory.exe` — it automatically adds `aimem.exe` to your User `PATH`, configures global rules for Claude Code / Antigravity / Codex, and imports existing local memories on first run.
2. Click **Connect Google** in the app (or run `aimem auth`) to enable cross-PC sync via Google Drive.
3. Use `aimem` from any terminal or let your AI agents use it automatically:
   ```powershell
   aimem context
   aimem add "Use SQLite WAL + FTS5" --kind arch --tags "sqlite,db" --agent antigravity
   aimem search "FTS5"
   ```

## Automated Releases (GitHub Actions)

The workflow in [.github/workflows/windows-release.yml](.github/workflows/windows-release.yml) builds, signs, and publishes the NSIS installer, `aimem.exe`, and `latest.json` updater manifest whenever `app/tauri.conf.json` version changes on `main`.

Required GitHub repository settings (**Settings → Secrets and variables → Actions**):
- Variables: `AIMEM_GOOGLE_CLIENT_ID`, `TAURI_SIGNING_PUBLIC_KEY`
- Secrets: `AIMEM_GOOGLE_CLIENT_SECRET`, `TAURI_SIGNING_PRIVATE_KEY` (and optional `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`)

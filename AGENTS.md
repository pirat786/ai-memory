# AGENTS.md — AI Memory (`aimem`)

## Назначение
Единая локальная память для ИИ-агентов (Antigravity, Claude Code, Codex, Cursor, Gemini CLI) на базе SQLite + FTS5, CLI `aimem`, десктопного приложения Tauri 2 (`ai-memory`) и синхронизации через Google Drive (`drive.appdata`).

## Структура
- `crates/aimem-core` — ядро: SQLite + FTS5, определение проекта по Git remote, Google Drive sync, установщик правил (`rules.rs`), импортёр (`importer.rs`).
- `crates/aimem-cli` — CLI `aimem`.
- `app/` — десктопное приложение на Tauri 2 (трей, автообновление, фоновая синхронизация).
- `ui/` — фронтенд интерфейса (`index.html`).
- `docs/` — GitHub Pages, Privacy Policy, Terms of Service.
- `secrets/` — локальные OAuth-секреты (в `.gitignore`, **не коммитить**).

## Команды сборки и проверки
```powershell
cargo test --workspace
cargo build --release --workspace
```

## Правила
- `rules.rs` обновляет только блок `<!-- AI-MEMORY-START --> ... <!-- AI-MEMORY-END -->`, сохраняя остальные пользовательские правила в глобальных файлах.
- Не коммитить `secrets/` и любые OAuth-токены.

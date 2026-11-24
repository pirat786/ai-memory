# AI Memory (`aimem` + Tauri Desktop)

[English](README.md) · **Русский**

Единая локальная память для ИИ-агентов (**Antigravity**, **Claude Code**, **Codex**, **Cursor**, **Gemini CLI**) без MCP — с общим хранилищем SQLite + FTS5, десктопным приложением на **Tauri 2** (окно + трей + автообновление + фоновый синк) и синхронизацией между компьютерами через **Google Drive (`drive.appdata`)**.

## Возможности

- **Без MCP:** агенты работают через быстрый CLI (`aimem context`, `aimem add`, `aimem search`).
- **Умная привязка к проекту:** проект определяется по нормализованному `git remote origin` (одинаково для HTTPS и SSH), поэтому на разных ПК у одного репозитория общая память.
- **Полнотекстовый поиск (SQLite FTS5):** мгновенный поиск по решениям, архитектуре, багам и правилам.
- **Десктопное приложение (Tauri 2):** нативное окно, системный трей, автонастройка `PATH` и правил агентов при запуске, фоновый синк каждые 5 минут и автообновление из GitHub Releases.
- **Универсальный импортёр:** переносит существующую память из `~/.claude`, `~/.gemini`, `~/.codex`, `~/.cursor` и документации проектов (`README.md`, `CLAUDE.md`, `AGENTS.md`, `docs/ARCHITECTURE.md`).
- **Синхронизация через Google Drive (`drive.appdata`):** слияние по UUID и `updated_at` с поддержкой мягкого удаления.

## Структура проекта

- `crates/aimem-core` — общее ядро: SQLite + FTS5, определение проекта по Git, Google Drive sync, установщик правил, импортёр.
- `crates/aimem-cli` — быстрый CLI `aimem.exe` для агентов и терминала.
- `app/` — десктопное приложение на Tauri 2 (`ai-memory.exe`) с треем и автообновлением.
- `ui/` — интерфейс приложения (`index.html`).
- `docs/` — сайт для GitHub Pages, Privacy Policy и Terms of Service.

## Сборка

```powershell
cargo test --workspace
cargo build --release --workspace
```

Готовые бинарники:
- `target/release/aimem.exe` — CLI для агентов.
- `target/release/ai-memory.exe` — десктопное приложение с треем.

## Быстрый старт

1. Запусти `ai-memory.exe` — приложение само добавит `aimem.exe` в `PATH`, настроит правила для Claude Code / Antigravity / Codex и импортирует существующую память при первом запуске.
2. Нажми **«Подключить Google»** (или выполни `aimem auth`) для синхронизации между компьютерами.
3. Использование из агентов / терминала:
   ```powershell
   aimem context
   aimem add "Используем SQLite WAL + FTS5" --kind arch --tags "sqlite,db" --agent antigravity
   aimem search "FTS5"
   ```

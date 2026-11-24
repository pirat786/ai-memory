# AI Memory (`aimem` + Tauri Desktop)

Единая локальная память для ИИ-агентов (**Antigravity**, **Claude Code**, **Cursor**, **Gemini CLI**) без MCP — с общим хранилищем SQLite + FTS5, десктопным приложением на **Tauri 2** (окно + трей + автосинк) и синхронизацией между компьютерами через **Google Drive (`drive.appdata`)**.

## Структура проекта

- `crates/aimem-core` — общее ядро: SQLite + FTS5, определение проекта по Git remote, Google Drive sync, установщик правил.
- `crates/aimem-cli` — быстрый CLI `aimem.exe` для агентов и терминала.
- `app/` — десктопное приложение на Tauri 2 (`ai-memory.exe`) с треем и фоновой синхронизацией.
- `ui/` — интерфейс приложения (`index.html`).

## Сборка

```powershell
cargo test --workspace
cargo build --release --workspace
```

Готовые бинарники:
- `target/release/aimem.exe` — CLI (добавьте в `PATH` для агентов).
- `target/release/ai-memory.exe` — десктопное приложение с треем.

## Быстрый старт

```powershell
# 1. Подключить правила для Claude Code и Antigravity
aimem install-rules

# 2. Запустить десктопное приложение (трей + автосинк)
.\target\release\ai-memory.exe

# 3. Использование из агентов / терминала
aimem context
aimem add "Используем SQLite WAL + FTS5" --kind arch --tags "sqlite,db" --agent antigravity
aimem search "FTS5"
```

## Настройка Google Drive Sync

1. В Google Cloud Console включите **Google Drive API** и создайте OAuth-клиент типа **Desktop app** со scope `https://www.googleapis.com/auth/drive.appdata`.
2. Укажите `Client ID` (и `Client Secret`, если требуется) в окне **«Настройки Google и агентов»** внутри `AI Memory` и нажмите **«Подключить Google»** (или через CLI: `aimem auth --client-id ...`).
3. Приложение в трее автоматически синхронизируется каждые 5 минут (или по кнопке в UI / трее / `aimem sync`).

# ai-memory (`aimem`)

Единая локальная память для ИИ-агентов (**Antigravity**, **Claude Code**, **Cursor**, **Gemini CLI**) без MCP — с общим хранилищем SQLite + FTS5, встроенным Web UI и синхронизацией между компьютерами через **Google Drive (`drive.appdata`)**.

## Возможности

- **Без MCP:** агенты работают через быстрый CLI (`aimem context`, `aimem add`, `aimem search`).
- **Умная привязка к проекту:** проект определяется по `git remote origin` (нормализует HTTPS и SSH к единому ID), поэтому `C:\RABOTA\proj` на одном ПК и `D:\Code\proj` на другом делят общую память.
- **Полнотекстовый поиск (SQLite FTS5):** мгновенный поиск по решениям, архитектуре, багам и правилам.
- **Встроенный Web UI (`aimem ui`):** просмотр проектов, поиск, добавление и удаление записей, настройка Google Sync.
- **Синхронизация через Google Drive (`drive.appdata`):** слияние изменений по UUID и `updated_at` (поддерживается мягкое удаление).
- **Автоподключение агентов (`aimem install-rules`):** добавляет инструкции в `~/.claude/CLAUDE.md`, `~/.gemini/antigravity/AGENTS.md` и `~/.gemini/GEMINI.md`.

## Сборка

```powershell
cargo test
cargo build --release
```

Готовый бинарник: `target/release/aimem.exe` (добавьте его в `PATH`).

## Быстрый старт

```powershell
# 1. Подключить правила для Claude Code и Antigravity
aimem install-rules

# 2. Добавить глобальное предпочтение
aimem add "Всегда писать лаконичные тесты" --global --kind pref

# 3. В папке любого проекта посмотреть контекст или добавить факт
aimem context
aimem add "Используем SQLite WAL + FTS5" --kind arch --tags "sqlite,db" --agent antigravity

# 4. Открыть Web UI
aimem ui
```

## Настройка Google Drive Sync

1. В Google Cloud Console включите **Google Drive API** и создайте OAuth-клиент типа **Desktop app** со scope `https://www.googleapis.com/auth/drive.appdata`.
2. Укажите `Client ID` (и `Client Secret`, если нужен) в Web UI (`Настройки Google и агентов`) или через команду:
   ```powershell
   aimem auth --client-id "ВАШ_CLIENT_ID" --client-secret "ВАШ_SECRET"
   ```
3. Запускайте синхронизацию через кнопку в UI или командой:
   ```powershell
   aimem sync
   ```

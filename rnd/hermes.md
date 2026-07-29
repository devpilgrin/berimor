# Hermes Agent: архитектура агента

> Источники: официальная документация https://hermes-agent.nousresearch.com/docs, репозиторий NousResearch/hermes-agent, встроенный скилл `hermes-agent`.

## Общее позиционирование

Hermes Agent — открытая среда LLM-агентов от Nous Research. Работает в терминале, десктоп-приложении, веб-панели, IDE (через ACP) и на 20+ платформах обмена сообщениями. Поддерживает 20+ LLM-провайдеров (OpenRouter, Anthropic, OpenAI, Google, DeepSeek, xAI, локальные модели и др.). Относится к классу autonomous coding agents (Claude Code, Codex, OpenClaw).

## Отличительные черты

- **Самосовершенствование через навыки (Skills).** Агент сохраняет проверенные процедуры в `~/.hermes/skills/`; они автоматически загружаются в будущих сессиях.
- **Постоянная память.** Пользовательские предпочтения, факты об окружении и извлечённые уроки сохраняются через плагины памяти (встроенная, Honcho, Mem0 и др.).
- **Мультиплатформенный шлюз.** Один агент доступен в Telegram, Discord, Slack, WhatsApp, iMessage, Signal, Matrix, Teams, Email и т.д. с полным доступом к инструментам, а не только чатом.
- **Независимость от поставщика.** Модель и провайдер можно менять в ходе сессии; пулы учётных данных автоматически чередуют ключи.
- **Профили.** Изолированные экземпляры Hermes с собственными конфигами, сессиями, навыками и памятью (`~/.hermes/profiles/<name>/`).
- **Расширяемость.** Плагины, MCP-серверы, кастомные инструменты, вебхуки, cron, полная экосистема Python.

## Ключевые пути и конфигурация

```
~/.hermes/config.yaml        # главная конфигурация
~/.hermes/.env               # ключи API и секреты
~/.hermes/skills/            # установленные навыки
~/.hermes/sessions/          # индекс маршрутизации, JSONL-транскрипты, дампы
~/.hermes/state.db           # SQLite + FTS5 — каноническое хранилище сессий
~/.hermes/logs/              # логи шлюза и ошибок
~/.hermes/auth.json          # OAuth-токены и credential pools
~/.hermes/hermes-agent/      # исходный код (при git-установке)
```

Профили используют `~/.hermes/profiles/<name>/` с таким же макетом.

## Разделы конфигурации (`config.yaml`)

| Раздел | Назначение |
|--------|------------|
| `model` | default, provider, base_url, api_key, context_length |
| `agent` | max_turns (90), tool_use_enforcement |
| `terminal` | backend, cwd, timeout (180) |
| `compression` | enabled, threshold, target_ratio |
| `display` | skin, interface (cli/tui), tool_progress, show_reasoning, show_cost, language |
| `stt` | провайдеры распознавания речи |
| `tts` | провайдеры синтеза речи |
| `memory` | memory_enabled, user_profile_enabled, provider |
| `security` | tirith_enabled, website_blocklist |
| `delegation` | модель/провайдер для субагентов, max_iterations (50), reasoning_effort |
| `checkpoints` | filesystem snapshots |
| `curator` | жизненный цикл навыков, consolidation |

## CLI (краткий справочник)

- `hermes` — интерактивный чат.
- `hermes chat -q "..."` — one-shot.
- `hermes setup`, `hermes model`, `hermes doctor` — настройка и диагностика.
- `hermes tools` — включение/выключение наборов инструментов.
- `hermes skills list/install/inspect/update` — управление навыками.
- `hermes mcp add/list/test` — MCP-серверы.
- `hermes gateway run/install/start/stop` — шлюз платформ.
- `hermes sessions list/browse/export` — сессии.
- `hermes cron create/edit/pause/resume/remove` — cron-задания.
- `hermes webhook subscribe/list/remove/test` — вебхуки.
- `hermes profile create/use/delete` — профили.
- `hermes auth add/list/remove` — пулы учётных данных.
- `hermes desktop/gui`, `hermes dashboard`, `hermes proxy` — другие поверхности.

## Toolsets (наборы инструментов)

Основные: `web`, `search`, `browser`, `terminal`, `file`, `code_execution`, `vision`, `image_gen`, `video`, `x_search`, `tts`, `skills`, `memory`, `session_search`, `delegation`, `cronjob`, `clarify`, `messaging`, `todo`, `kanban`, `debugging`, `safe`, `spotify`, `homeassistant`, `discord`, `rl` и др. Включение/отключение вступает в силу после `/reset` нового сеанса.

## Файлы контекста проекта

Hermes вводит инструкции уровня проекта, читая файлы из рабочего каталога. Первое совпадение побеждает.

| Файл | Приоритет | Режим | Назначение |
|------|-----------|-------|------------|
| `.hermes.md` / `HERMES.md` | до корня git | иерархические правила проекта, специфичные для Hermes |
| `AGENTS.md` / `agents.md` | только cwd | переносимые инструкции, работают в Claude Code, Codex, OpenCode |
| `CLAUDE.md` / `claude.md` | только cwd | то же, со вкусом Claude |
| `.cursorrules` / `.cursor/rules/*.mdc` | только cwd | переход с Cursor |

`SOUL.md` (в `$HERMES_HOME`) загружается всегда и задаёт личность агента, не правила проекта.

Каждый контекстный файл ограничен 20 000 символов; длинные файлы усекаются head + tail с маркером `[...truncated...]`. Правила проходят сканер шаблонов угроз (prompt injection) перед попаданием в системную подсказку.

## Архитектура агентного цикла

- **Loop:** пользовательский запрос → выбор инструментов → вызов LLM → выполнение инструментов → обновление контекста → следующий ход. Максимум `agent.max_turns` (по умолчанию 90).
- **Context:** SQLite state.db + FTS5 для session_search; JSONL-транскрипты; сжатие контекста при приближении к лимиту (`compression.*`).
- **Model/provider independence:** вызовы LLM абстрагированы; пулы credentials автоматически переключают ключи при исчерпании или ошибках.
- **Safety:** детектор опасных команд (`--yolo` для обхода), redaction секретов в выводе инструментов, `approvals.mode` (smart/manual/off), allowlist shell-hooks, website blocklist.

## Фоновые системы

### Делегирование (`delegate_task`)

- Создаёт субагентов с изолированным контекстом и терминалом.
- Одиночный (`goal`), пакетный (`tasks` до 3 параллельно по умолчанию), фоновый (`background=true`).
- Роли: `leaf` (не может делегировать дальше) и `orchestrator` (может спавнить воркеров, ограничен `delegation.max_spawn_depth`).
- Фоновый дочерний процесс не переживает завершение родителя; для долгих задач — `cronjob` или `terminal(background=True)`.

### Cron

- Планировщик `cron/jobs.py` + `cron/scheduler.py`.
- Расписания: длительность (`30m`, `2h`), `every monday 9am`, cron 5-полей, ISO timestamp.
- Поддерживает skills, model/provider override, script, `context_from` (цепочка задач), `workdir`, мультиплатформенную доставку.
- 3-минутный hard deadline, `.tick.lock` предотвращает дублирование, доставка оформляется header/footer, не зеркалирование в шлюзовой сеанс.

### Куратор навыков

- Фоновое обслуживание навыков, созданных агентом (`created_by: "agent"`).
- Отслеживает использование, помечает устаревшие, архивирует, делает backup tar.gz.
- Никогда не удаляет; закреплённые навыки исключены из автоматического прохода.
- LLM-консолидация отключена по умолчанию (`curator.consolidate: false`).

### Канбан (Kanban)

- SQLite-плата для совместной работы нескольких профилей/агентов.
- Профили-оркестраторы видят полный набор `kanban_*` инструментов; воркеры — ограниченный.
- Диспетчер по умолчанию работает в шлюзе, спавнит назначенные профили, блокирует задачу после `failure_limit` неудач.
- Жёсткая граница доски (`HERMES_KANBAN_BOARD`), мягкое пространство имён арендатора для изоляции рабочей директории и ключа памяти.

## Поверхности (surfaces)

- **CLI/TUI:** `hermes` (Ink TUI), `hermes chat -q`.
- **Desktop:** Electron-приложение для macOS/Linux/Windows.
- **Web Dashboard:** `hermes dashboard` — админка, конфигуратор каналов, MCP, cron, вебхуков, профилей, встроенный чат.
- **Gateway:** 20+ мессенджеров; большинство адаптеров — плагины `plugins/platforms/`.
- **Proxy:** OpenAI-совместимый локальный прокси `hermes proxy` для Aider, Cline, Continue и т.д.
- **ACP server:** интеграция с VS Code/Zed/JetBrains.

## Провайдеры и модели

Поддерживаются OpenRouter, Anthropic, Nous Portal (OAuth), Codex (OpenAI OAuth), GitHub Copilot, Google Gemini, DeepSeek, xAI/Grok, Hugging Face, Z.AI/GLM, MiniMax, Kimi/Moonshot, Alibaba/DashScope, Xiaomi MiMo, KiloCode, OpenCode Zen/Go, Qwen OAuth и кастомные endpoints.

## Безопасность и конфиденциальность

- **Redaction секретов** (`security.redact_secrets`) включена по умолчанию; сканирует stdout терминала, `read_file`, веб-контент, сводки субагентов.
- **PII redaction** (`privacy.redact_pii`) — опционально, для шлюза.
- **Approvals:** `smart` (по умолчанию) — вспомогательный LLM оценивает деструктивные команды; `manual`; `off`/`--yolo`.
- **Shell-hooks allowlist** — явное разрешение опасных shell-интеграций.
- **Website blocklist** — запрет обращения к определённым доменам.
- **Терминальный бэкенд:** локальный, Docker, SSH, модальный; timeout 180 сек по умолчанию.

## Типичные питфоллы

- `/reset` нужен после изменения toolset или навыков.
- `/restart` шлюза или выход/вход в CLI после изменения конфигурации.
- GitHub Copilot требует OAuth-фlow устройства, не работает `gh auth login`.
- Gateway на WSL2 требует `systemd=true` в `/etc/wsl.conf`; иначе падает при закрытии WSL.
- Шлюз на SSH-сервере требует `loginctl enable-linger $USER`.
- Вспомогательные задачи (vision, compression, session_search) с `provider: auto` требуют `OPENROUTER_API_KEY` или `GOOGLE_API_KEY`, либо явную настройку.

## Ссылки

- Документация: https://hermes-agent.nousresearch.com/docs/
- Репозиторий: https://github.com/NousResearch/hermes-agent
- Конфигурация: https://hermes-agent.nousresearch.com/docs/user-guide/configuration
- Провайдеры: https://hermes-agent.nousresearch.com/docs/integrations/providers
- MCP: https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp
- Cron: https://hermes-agent.nousresearch.com/docs/user-guide/features/cron
- Memory: https://hermes-agent.nousresearch.com/docs/user-guide/features/memory
- Messaging: https://hermes-agent.nousresearch.com/docs/user-guide/messaging/

# Сравнение архитектур: Hermes Agent vs Claude Code

> Сводка на основе официальной документации и собранных источников.

## TL;DR

- **Claude Code** — узко сфокусированный coding-agent, тесно интегрированный с экосистемой Anthropic; контекстное окно, checkpoints, permission modes, skills, subagents, MCP.
- **Hermes Agent** — универсальная открытая агентная платформа от Nous Research: тот же кодинг + 20+ мессенджеров, cron, kanban, профили, память, пулы провайдеров, фоновые системы, open-source расширяемость.

## Позиционирование

| Аспект | Claude Code | Hermes Agent |
|--------|-------------|--------------|
| Разработчик | Anthropic | Nous Research |
| Лицензия / код | Проприетарный; клиент недоступен | Open-source (Python) |
| Основной фокус | Разработка ПО и работа с кодовой базой | Универсальный агент: код, исследования, автоматизация, мессенджеры |
| Поверхности | Терминал, Web, Desktop, IDE, Chrome, Mobile | CLI, TUI, Desktop, Web Dashboard, Gateway (20+ платформ), IDE, Proxy |

## Модель и провайдеры

| Аспект | Claude Code | Hermes Agent |
|--------|-------------|--------------|
| Модели по умолчанию | Claude (Anthropic) | Настраиваемый default; Kimi, DeepSeek, Claude, Gemini, Grok, OpenAI, локальные и др. |
| Провайдеры | Ограничены экосистемой Anthropic | 20+ провайдеров, OAuth-пулы, автопереключение ключей |
| Смена модели в сессии | Ограничена | Поддерживается |
| Пул учётных данных | Нет | Есть (`hermes auth`) |

## Контекст и управление им

| Аспект | Claude Code | Hermes Agent |
|--------|-------------|--------------|
| Постоянный проектный контекст | `CLAUDE.md` | `.hermes.md`, `AGENTS.md`, `CLAUDE.md`, `.cursorrules` |
| Персональная память | Auto memory | Плагины памяти (builtin, Honcho, Mem0) |
| Автокомпакция | Да, встроенная + `/compact` | Да, сжатие контекста (`compression.*`) |
| Просмотр контекста | `/context` | `session_search`, логи, JSONL-транскрипты |
| Изолированные субагенты | Subagents, Agent teams | `delegate_task` (single, batch, background, orchestrator) |
| Skill-загрузка | Описания сразу, полное содержимое — по требованию | Skills загружаются по правилам; может быть always-on или on-demand |
| Контекстный лимит | Управляется внутри модели | `agent.max_turns` (90 по умолчанию) + `context_length` |

## Расширяемость и инструменты

| Аспект | Claude Code | Hermes Agent |
|--------|-------------|--------------|
| Встроенные инструменты | Файлы, поиск, терминал, веб | 20+ toolsets: web, browser, terminal, file, code_exec, vision, image_gen, video, x_search, messaging, kanban, cronjob и др. |
| MCP | Поддержка MCP-серверов | `hermes mcp add/serve/test`, встроенный MCP-клиент + Hermes как MCP-сервер |
| Skills | Markdown-файлы с инструкциями и workflows | Markdown-файлы с YAML frontmatter; можно публиковать в hub |
| Hooks | Lifecycle hooks | Вебхуки + cron + shell hooks |
| Плагины / Marketplace | Плагины и маркетплейс | Плагины Python; `hermes plugins` |
| Code intelligence | LSP (Language Server) | IDE-интеграция через ACP; LSP не выделен как отдельный extension |
| Language Server | Да, встроенный extension | Зависит от инструментов/плагинов |

## Безопасность и разрешения

| Аспект | Claude Code | Hermes Agent |
|--------|-------------|--------------|
| Режимы разрешений | Manual, Accept edits, Plan, Auto | `approvals.mode`: smart, manual, off; `--yolo` |
| Контрольные точки файлов | Checkpoints (snapshots перед правками) | `--checkpoints` / `/rollback` (filesystem snapshots) |
| Redaction секретов | Да, по умолчанию | `security.redact_secrets` включена по умолчанию |
| Redaction PII | `privacy.redact_pii` | Отдельный флаг `privacy.redact_pii` |
| Website blocklist | Не упомянут | `security.website_blocklist` |
| Shell-hooks allowlist | Не упомянут | `~/.hermes/shell-hooks-allowlist.json` |
| Детектор опасных команд | Встроен в режим Auto | `tirith` + вспомогательный LLM |

## Фоновые и долгоживущие системы

| Аспект | Claude Code | Hermes Agent |
|--------|-------------|--------------|
| Cron / планировщик | Нет | Встроенный cron (`hermes cron`) |
| Kanban / доска задач | Нет | Встроенный kanban (`hermes kanban`) |
| Куратор навыков | Нет | Встроенный (`hermes curator`) |
| Фоновые субагенты | Agent teams (координация сессий) | `delegate_task(background=true)` + cron |
| Gateway мессенджеров | Нет | 20+ платформ |
| Proxy / API-совместимость | Нет | `hermes proxy` — OpenAI-совместимый endpoint |

## Профили и изоляция

| Аспект | Claude Code | Hermes Agent |
|--------|-------------|--------------|
| Профили | Пользователь / проект / организация | Полноценные профили (`~/.hermes/profiles/...`) с отдельными конфигами, сессиями, навыками, памятью |
| Изоляция рабочих директорий | В рамках одного процесса | `-w` worktree mode; профили; арендаторы в kanban |
| Мультипользовательность | Организационные политики | Профили + канбан-арендаторы |

## Архитектурные паттерны

Оба агента реализуют одинаковые фундаментальные паттерны:

- **Augmented LLM** — базовая модель + инструменты + retrieval + memory.
- **Workflows** — prompt chaining, routing, parallelization, orchestrator-workers, evaluator-optimizer.
- **Autonomous agents** — цикл рассуждение → действие → наблюдение.
- **MCP** — стандарт подключения внешних инструментов и сервисов.
- **Subagents / isolation** — изолированные контексты для сложных или длительных задач.

Claude Code явно формализует эти паттерны в статье "Building effective agents" и предлагает начинать с workflows, а не с autonomous agents.

## Когда что выбирать

### Claude Code лучше, если:
- Основная работа — разработка в существующей кодовой базе.
- Нужна тесная интеграция с Claude, VS Code, JetBrains.
- Важны встроенные checkpoints, permission modes, code intelligence.
- Команда уже использует экосистему Anthropic.

### Hermes Agent лучше, если:
- Нужна универсальная платформа для кода, исследований, автоматизации, мессенджеров.
- Требуется свободный выбор модели/провайдера и их пулы.
- Нужны фоновые задачи (cron), kanban-доска, куратор навыков, gateway.
- Важна open-source расширяемость и самостоятельный хостинг.
- Требуется работа в Telegram/Discord/Slack/WhatsApp и т.д.

## Практические выводы для проекта Jarvis

1. **Детерминированный код вместо слепого доверия модели.** Оба агента подтверждают, что ключевые решения (permission, checkpoints, scheduling, routing) должны быть жёстко закодированы, а не отданы на откуп слабым моделям.
2. **Skills / процедурная память — критично.** Hermes и Claude Code converged на идее: повторяемые процедуры сохранять как markdown-навыки, а не в истории диалога.
3. **Context isolation.** Subagents / delegate_task — обязательный паттерн для долгих задач и параллельной работы.
4. **MCP как стандарт интеграции.** Вместо собственных адаптеров под каждый сервис — реализовать MCP-сервер/клиент.
5. **Safety first.** Checkpoints + redaction + approval modes должны быть включены по умолчанию, а не опциональны.
6. **Профили и изоляция.** Пользовательский профиль + проектный контекст + рабочая директория — минимальная модель многозадачности.

## Ссылки

- Hermes: `hermes.md` в этой папке
- Claude Code: `claude_code.md` в этой папке
- MCP: `frameworks.md` в этой папке
- Anthropic patterns: https://www.anthropic.com/research/building-effective-agents
- Hermes docs: https://hermes-agent.nousresearch.com/docs/
- Claude Code docs: https://docs.anthropic.com/en/docs/agents-and-tools/claude-code/overview

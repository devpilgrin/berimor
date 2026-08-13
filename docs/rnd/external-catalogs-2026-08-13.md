# Внешние каталоги скиллов/субагентов/плагинов: анализ и кандидаты для berimor (2026-08-13)

Задача: проанализировать общедоступные каталоги Claude Code и Hermes,
отобрать переиспользуемое для каталогов berimor (skills/agents/plugins).
Ограничение заказчика: НЕ извлекать навыки, связанные с личной
информацией — все источники ниже публичные, личные профильные навыки
Hermes-профиля в выборку не входят.

## Источники (проверены живым доступом 2026-08-13)

| Источник | Что это | Состав |
|---|---|---|
| `github.com/anthropics/skills` | Официальные скиллы Anthropic (Claude Code) | 17 скиллов (skills/): algorithmic-art, brand-guidelines, canvas-design, claude-api, doc-coauthoring, docx, frontend-design, internal-comms, mcp-builder, pdf, pptx, skill-creator, slack-gif-creator, theme-factory, web-artifacts-builder, webapp-testing, xlsx |
| `github.com/NousResearch/hermes-agent` (skills/) | Официальный каталог навыков Hermes | Категории: software-development, devops, github, research, productivity, creative, media, mlops, note-taking, smart-home, social-media, email, apple, autonomous-ai-agents |
| `github.com/wshobson/agents` | Крупнейшая публичная коллекция субагентов Claude Code | 100+ определений агентов (.md с frontmatter: name, description, tools, model) |
| `github.com/modelcontextprotocol/servers` (src/) | Эталонные MCP-серверы | everything (тестовый), fetch, filesystem, git, memory, sequentialthinking, time |

## Совместимость форматов (факты по коду berimor)

- **Скиллы**: berimor читает SKILL.md с frontmatter `name/description/triggers/tools` —
  тот же формат, что у Claude Code и Hermes. Извлечение = копия + адаптация:
  триггеры (рус+англ), поле `tools` — потолок, переписывается с имён Claude
  (Read/Write/Bash…) на имена berimor (`files.read`, `terminal.exec`…).
- **Субагенты**: berimor — agent.yaml (`name/description/model_tier/tools/
  max_turns/max_wall_seconds/allow_spawn`); Claude-субагент — .md с
  frontmatter. Конверсия механическая: frontmatter → yaml, тело → system-роль,
  `allow_spawn: false` по умолчанию (fail-closed), потолок инструментов —
  маппингом как у скиллов.
- **Плагины**: Claude-плагины — пакеты скиллов/команд/агентов, с плагинами
  berimor (изолированный процесс + ACL-манифест + sigstore) НЕ совместимы.
  Мост в экосистему — MCP: berimor имеет MCP-клиент (ADR-0023), поэтому
  «каталог плагинов» для berimor = курируемый набор `[[mcp_servers]]`.

## Кандидаты: скиллы (из anthropics/skills)

| Скилл | Зачем berimor | Примечание |
|---|---|---|
| mcp-builder | Создание MCP-серверов — прямая тема (MCP-клиент есть) | tools→files.*/terminal.exec |
| skill-creator | Создание новых скиллов каталога | соответствует berimor-формату |
| docx, pptx, xlsx, pdf | Документы — частые задачи автоматизации | скрипты на python-зависимостях машины; тело-рецепт переносится, scripts — с проверкой |
| frontend-design | Веб-интерфейсы | рецепт, без внешних dep |
| doc-coauthoring | Соавторство документов | рецепт |
| internal-comms | Статус-отчёты/комм-пакеты | рецепт |
| webapp-testing | QA через Playwright | требует playwright MCP — связать с [[mcp_servers]] |
| brand-guidelines, canvas-design, algorithmic-art, theme-factory | Креатив/оформление | ниша, но без dep |

**Исключены сознательно**: slack-gif-creator (специфика Slack, не в
ландшафте), web-artifacts-builder (формат artifacts claude.ai, у berimor
нет среды artifacts), claude-api (привязка к Anthropic SDK — у berimor
мультипровайдерность; держать в очереди, адаптировать позже).

## Кандидаты: скиллы (из NousResearch/hermes-agent/skills)

Публичные категории, полезные как ПАКЕТЫ для каталога berimor:
- software-development (ревью, отладка, TDD, планирование) — ядро
  код-агента;
- github (PR/issues/review через gh) — gh CLI доступен;
- devops, research (arxiv, lookup) — рабочие рецепты.

ВАЖНО: брать из публичного репозитория, не из локального профиля —
локальные копии могли быть персонализированы (ограничение заказчика).

## Кандидаты: субагенты (из wshobson/agents, конверсия в agent.yaml)

Первый эшелон (универсальные, без специфики вендора):
- code-reviewer, security-auditor, debugger, backend-architect,
  frontend-developer, devops-troubleshooter, postgres-pro,
  rust-engineer, test-automator, performance-engineer.

Маппинг полей: `tools: [Read, Grep, Bash]` → `[files.read, files.search,
terminal.exec]` и т.п.; `max_turns: 20`, `max_wall_seconds: 600`,
`allow_spawn: false` — дефолты каталога.

## Кандидаты: «плагины» = MCP-серверы (готовые [[mcp_servers]])

Эталонные (modelcontextprotocol/servers): filesystem, fetch, git,
memory, time, sequentialthinking (everything — только для тестов).
Проверенные сообществом (установка npm/npx): github (github-mcp-server),
playwright, postgres, sqlite, brave-search (нужен API-ключ).
Дать курируемый список с готовыми блоками конфига + напоминание: сервер
встаёт в общий диспетчер ПОСЛЕ встроенных, через тот же гейт.

## Предлагаемый план извлечения (по команде)

1. Скиллы-эшелон-1 (mcp-builder, skill-creator, docx, xlsx, pptx, pdf,
   frontend-design, doc-coauthoring) → `fixtures/golden/skills/` НЕТ —
   в каталог-поставку (решить: отдельный репозиторий berimor-catalog или
   skills/ в основном) + адаптация триггеров и потолков.
2. Субагенты-эшелон-1 (10 шт) → конвертер md→agent.yaml (скрипт) +
   ручная вычитка потолков.
3. MCP-каталог → `docs/` страница «Рекомендованные MCP-серверы» с
   готовыми блоками [[mcp_servers]].

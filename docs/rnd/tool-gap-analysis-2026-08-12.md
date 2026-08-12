# Инвентаризация инструментов: berimor vs Claude Code vs Hermes (2026-08-12)

Задача: базовый набор встроенных инструментов berimor мал — сверить с
эталонами и составить список кандидатов на встройку.

## Источники фактов (проверено, не по памяти)

- **berimor:** `crates/berimor-cli/src/builtin_dispatch.rs` — реестр встроенных.
- **Claude Code v2.1.220:** бинарник `/opt/claude-code/bin/claude` на машине —
  имена инструментов подтверждены grep по нему (MultiEdit, TodoWrite,
  BashOutput, KillShell, ExitPlanMode, WebSearch, NotebookEdit,
  AskUserQuestion, SlashCommand — все найдены).
- **Hermes:** рантайм-набор инструментов агента Hermes (Nous Research),
  зафиксирован по живому рантайму + docs.hermes-agent.nousresearch.com.
- **Свой план:** `docs/arch/ideal-agent-architecture.md` §3.9 — целевые
  встроенные наборы проекта: терминал, файлы, VCS, web (с сетевым гейтом),
  снапшоты, память, навыки, поиск по сессиям, список задач, доска,
  планировщик.

## 1. Что есть у berimor сейчас

| Инструмент | mutates | Примечание |
|---|---|---|
| files.read | false | кап размера |
| files.write | true | целиком, нет точечной правки |
| files.list | false | список каталога |
| terminal.exec | true | одноразовый, кап вывода, таймаут; нет фона |
| http.fetch | false | GET одного URL, сетевой гейт, без редиректов |
| agents.run | — | обёртка диспетчера (вложенный цикл) |

Плюс сильные стороны вне сравнения: codeact (QuickJS в WASM-песочнице —
аналога нет ни у Claude Code, ни у Hermes execute_code в плане изоляции),
MCP клиент+сервер (T1/T2), плагины-процессы с ACL и sigstore.

## 2. Эталоны

**Claude Code (база):** Read, Write, Edit, MultiEdit, NotebookEdit, Glob,
Grep, LS, Bash, BashOutput, KillShell, WebFetch, WebSearch, TodoWrite,
Task (субагент), ExitPlanMode, AskUserQuestion, SlashCommand, Skill, mcp__*.

**Hermes (база):** terminal (exec + background с notify), process
(poll/kill/log), read_file, write_file, patch (точечная правка),
search_files (grep + glob), execute_code (Python с доступом к инструментам),
browser_* (полный браузер: navigate/click/type/snapshot/console/vision),
todo, memory (долговременная), session_search (FTS по прошлым сессиям),
skills_list/view/manage, delegate_task (субагенты), cronjob (расписания),
clarify (вопрос пользователю), vision_analyze, text_to_speech.

## 3. Пробелы berimor (gap-анализ)

| Возможность | Claude Code | Hermes | berimor |
|---|---|---|---|
| Точечная правка файла | Edit/MultiEdit | patch | **нет** |
| Поиск: grep по содержимому + glob | Grep/Glob/LS | search_files | **нет** (только через terminal.exec — мутирующий, будит гейт) |
| VCS (git) | через Bash | через terminal | **нет** (§3.9 требует) |
| Поиск в web | WebSearch | web_search | **нет** (http.fetch — только точный URL) |
| Список задач сессии | TodoWrite | todo | **нет** |
| Фоновые процессы | BashOutput/KillShell | terminal background + process | **нет** |
| Вопрос пользователю из цикла | AskUserQuestion | clarify | **нет** (human_gate — только в процессах) |
| Память как инструмент | auto-memory файлы | memory | подсистема, не инструмент |
| Поиск по прошлым сессиям | — | session_search | **нет** (§3.9 требует) |
| Расписания агентом | — | cronjob | CLI-команды (оператор), не инструмент |
| Браузер | — | browser_* | — (сознательно: ниша MCP) |
| Песочница для кода модели | — | execute_code | **codeact (сильнее обоих)** |

## 4. Список кандидатов на встройку (приоритизированный)

### Волна A — база код-агента (без новых тяжёлых зависимостей)

1. **files.edit** — точечная замена old→new по уникальному якорю (аналог
   Claude Edit / Hermes patch). Без неё модель переписывает файлы целиком:
   дорого по токенам и хрупко. `mutates=true`, общий гейт.
2. **files.search** — два режима: `content` (regex по содержимому) и
   `files` (glob по имени), пагинация, капы вывода. Главный выигрыш:
   read-only поиск перестаёт идти через мутирующий terminal.exec и
   дёргать гейт подтверждения. Движок: crate `ignore` (движок ripgrep)
   или собственный обход + regex (уже в workspace).
3. **vcs.git** — git read-only операции (status/diff/log/show) через
   вызов системного git (не libgit2 — без новой зависимости), mutates=false;
   мутации (add/commit) — НЕ в этой волне (свой гейт-кейс).
4. **web.search** — поисковый запрос → список результатов (заголовок/URL/
   сниппет). Реализация: DuckDuckGo lite/html endpoint через инфраструктуру
   http.fetch + разбор (без API-ключа), сетевой гейт общий. mutates=false.

### Волна B — продуктивность цикла

5. **todo** — список задач сессии (read/write), состояние в App/процессе,
   срез — в контекстный слой шага (аналог TodoWrite / Hermes todo).
   mutates=false (не трогает ФС/сеть).
6. **terminal.exec: фоновый режим** — `background=true` → id запуска;
   новые инструменты `terminal.output` (чтение буфера) и `terminal.kill`.
   Аналог BashOutput/KillShell / Hermes process. mutates=true.
7. **human.ask** — вопрос пользователю из свободного цикла с ответом
   строкой (аналог AskUserQuestion / clarify): мапится на существующий
   канал подтверждений TUI (ConfirmRequest → модал с полем ввода).
   mutates=false. В процессах — уже покрыто human_gate, инструмент для
   agentic-цикла.

### Волна C — по собственному §3.9

8. **memory.search / memory.save** — инструментальный доступ к семантической
   памяти (сейчас запись — только кодом post-Finished). save — за opt-in
   флагом конфига (запись — доверенная граница, как extraction).
9. **session.search** — FTS по лентам прошлых сессий (`~/.config/berimor/
   sessions/*.jsonl`) и журналу — требование §3.9 «поиск по сессиям».
10. **snapshots** — снапшот файла перед мутацией (§3.9 «снапшоты»):
    скорее подсистема capability (авто-снапшот перед files.write), чем
    вызываемый инструмент — отдельное проектирование.

### Сознательно НЕ встраивать (обоснование)

- **Полный браузер** (Hermes browser_*) — тяжёлая зависимость (chromium/
  CDP); домен MCP (playwright-mcp и аналоги), уже подключаемо без кода.
- **NotebookEdit** — Jupyter-специфика; ниша MCP.
- **vision/TTS/email/cron-инструменты** — ниши плагинов/MCP; планировщик
  остаётся операторским уровнем (schedule/daemon CLI) по модели доверия.
- **execute_code (python)** — превзойдён codeact: QuickJS в WASM с топливом,
  WASI без прав — изоляция строже, чем у эталонов.

## 5. Предложение по постановке в ROADMAP

Волна A (п.1–4) — единый milestone «Базовые инструменты код-агента»:
4 инструмента, все в рамках существующих крейтов berimor-cli/berimor-capability,
зависимость-кандидат — только `ignore` (или собственный обход).
Каждый инструмент: builtin_policies + has_tool + dispatch + golden-кейсы
(deny/allowed) + e2e через реальный бинарник + запись в tools_catalog.

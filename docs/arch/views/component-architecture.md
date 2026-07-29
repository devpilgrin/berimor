# Компонентная архитектура — C4 Component

> C4, уровень 3: внутренние модули ядра. См. `ideal-agent-architecture.md` §3, `process-engine.md` §6.

## 1. Диаграмма компонентов ядра

```mermaid
flowchart TB
    subgraph core["Ядро (Rust)"]
        PE["Process Engine"]
        AL["Agent Loop"]
        CE["Context Engine\n(маршрутизатор → сборщик → оценщик)"]
        MED["Mediation\n(parse → schema → policy → commit)"]
        CAP["Capability\n(deny-статика, jail, сетевой гейт, ACL)"]
        TR["Tool Runtime"]
        MP["Model Pool\n(классы, селектор, деградация)"]
        ASCH["Actors & Scheduler"]
        MEMORY["Memory\n(4 слоя)"]
    end
    EXEC["Executors\n(ToolOnly / StructuredLLM / CodeAct / AgentStep)"]
    PLUGINS["Плагины / внешние MCP-серверы"]
    LLM_LOCAL["Локальный инференс"]
    LLM_REMOTE["Удалённые провайдеры"]
    STORE[("SQLite")]

    PE -->|build step, state| CE
    PE -->|dispatch| EXEC
    EXEC -->|сырой вывод| MED
    MED -->|патч состояния| PE
    PE -->|проверка перед мутацией| CAP
    CAP -->|allow/deny/confirm| PE
    CE -->|запрос слоёв| MEMORY
    MEMORY -->|read/write| STORE
    PE -->|read/write состояние и события| STORE
    EXEC -->|вызов инструмента| TR
    TR -->|IPC + ACL| PLUGINS
    EXEC -->|structured/codeact запрос| MP
    MP --> LLM_LOCAL
    MP --> LLM_REMOTE
    ASCH -->|инстанс = задача актора| PE
    AL -->|результат через| MED
```

## 2. Таблица интерфейсов (сокращённая)

Полная версия — `process-engine.md` §6.

| Компонент | Интерфейс | Потребители |
|---|---|---|
| Context Engine | `build(step, state) → context` | Process Engine, Agent Loop |
| Executors | `run(step, ctx) → raw_output` | Process Engine |
| Mediation | `commit(step, output) → patch \| retry \| escalate` | Process Engine, Agent Loop, фоновые работники памяти |
| Capability | `check(call) → allow \| deny \| confirm` | Process Engine (перед мутирующим вызовом), Executors (внутри CodeAct на каждый вызов стаба) |
| Model Pool | `select(tier, step) → provider` | StructuredLLM, CodeAct |
| Memory | `write(candidate) → committed \| conflict`, `read(query) → layer-results` | Context Engine (чтение), фоновые работники (запись) |
| Actors & Scheduler | `assign(task) → actor`, `schedule(cron) → task` | внешний интейк, Process Engine |

## 3. Правила связей (инвариантны к реализации)

- Executors никогда не пишут в состояние напрямую — только через Mediation.
- Capability проверяется независимо от результата Mediation: валидный контракт не освобождает от deny-проверки.
- Context Engine — единственный путь чтения памяти в структурированных шагах; у модели нет инструмента «сама поищи в памяти» (см. `memory-model.md` §3).
- Actors & Scheduler не вызывают Executors напрямую — только инстанцируют Process Engine.

## 4. Связанные документы

`process-engine.md`; `mediation.md`; `executors.md`; `memory-model.md`; `security-model.md` §2 (L2–L3).

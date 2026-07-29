# Архитектура данных

> См. `memory-model.md`, `process-engine.md` §3, `stack.md` §3.

## 1. Модель данных (ER)

Единый файл SQLite — см. ADR-0021. Диаграмма — логическая схема, не DDL.

```mermaid
erDiagram
    PROCESS_INSTANCE ||--o{ EVENT : appends
    PROCESS_INSTANCE ||--o{ SNAPSHOT : checkpoints
    PROCESS_INSTANCE }o--|| PROCESS_VERSION : "pinned to (ADR-0012)"
    ACTOR ||--|| MEMORY_PROFILE : owns
    MEMORY_PROFILE ||--o{ SEMANTIC_FACT : contains
    MEMORY_PROFILE ||--o{ EPISODE : contains
    SEMANTIC_FACT }o--|| FACT_SOURCE : "attributed to"
    ENTITY_NODE }o--|| ENTITY_TYPE : "conforms to"
    ENTITY_NODE ||--o{ ENTITY_EDGE : "source of"
    ENTITY_EDGE }o--|| EDGE_TYPE : "conforms to"
    TRUST_LIST_ENTRY ||--o{ PLUGIN_MANIFEST : authorizes

    PROCESS_INSTANCE {
        string id PK
        string process_name
        int version
        string status
    }
    EVENT {
        int seq PK
        string process_instance_id FK
        string kind
        blob payload
        datetime ts
    }
    SNAPSHOT {
        string process_instance_id FK
        int seq
        blob state_blob
    }
    MEMORY_PROFILE {
        string id PK
        string kind "user | tenant | actor"
    }
    SEMANTIC_FACT {
        string id PK
        string subject
        string predicate
        string object
        float confidence
        string source_trust "trusted | untrusted"
    }
    ENTITY_NODE {
        string id PK
        string type FK
        json fields
    }
    ENTITY_EDGE {
        string from_id FK
        string to_id FK
        string relation_type FK
    }
    TRUST_LIST_ENTRY {
        string repo PK
        string allowed_ref
        string signer_identity
        string capability_ceiling
    }
```

## 2. Поток данных: вывод модели → состояние → память

```mermaid
flowchart LR
    modelout["Вывод модели"] --> parse["Parse"]
    parse --> schema["Schema"]
    schema --> policy["Policy"]
    policy --> commit["Commit"]
    commit --> state[("Состояние процесса\n(рабочая память)")]
    commit --> episodic[("Эпизодическая память\nсобытие навсегда")]
    commit -.->|кандидат факта| dedup["Дедупликация\n(точное → близкое → конфликт)"]
    dedup --> semantic[("Семантическая память")]
    semantic --> contextbuilder["Context Engine: сборщик"]
    episodic --> contextbuilder
    contextbuilder --> promptin["Подсказка следующему шагу"]
```

## 3. Жизненный цикл и хранение по слоям

| Слой | Где живёт | Когда сворачивается/устаревает | Кто читает |
|---|---|---|---|
| Рабочая | таблица состояния текущего инстанса | сворачивается суммаризацией при приближении к бюджету, оригинал остаётся в эпизодической | Process Engine, Context Engine |
| Эпизодическая | `EVENT`, append-only, WAL | не удаляется; уплотнение по политике хранения (не удаление) | поиск по сессиям (FTS5) |
| Семантическая | `SEMANTIC_FACT` + векторный индекс `sqlite-vec` | устаревание по неиспользованию → ревью человеком | Context Engine (гибридный поиск) |
| Процедурная | файлы навыков на диске + событие ревизии в `EVENT` | версионируется, меняется только через подтверждение | Context Engine (описания всегда; тело — по требованию) |
| Граф сущностей (опционально) | `ENTITY_NODE`/`ENTITY_EDGE` | согласованность типов — контракт Mediation, конфликт рёбер — событие человеку | запросы прецедентов по профилю процесса |

## 4. Границы данных, требующие маскировки

Аргументы/вывод инструментов, мост к хранилищу секретов, тексты подтверждений, запись в `EVENT`/`SEMANTIC_FACT` — везде проходит маскировщик до записи (см. `security-model.md` §2, слой L5). Ни одна из таблиц выше не хранит значение секрета, только алиас.

## 5. Связанные документы

`memory-model.md`; `process-engine.md` §3; `mediation.md` §4; `stack.md` §3; ADR-0005, ADR-0016, ADR-0021.

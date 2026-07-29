# State View — жизненные циклы ключевых сущностей

> См. `process-engine.md` §4–5, `deployment.md` §5, `ideal-agent-architecture.md` §3.10.

## 1. Инстанс процесса

```mermaid
stateDiagram-v2
    [*] --> Running: instantiate
    Running --> Running: шаг применён (patch + event)
    Running --> WaitingHuman: human_gate
    WaitingHuman --> Running: подтверждено
    WaitingHuman --> Failed: таймаут по политике
    WaitingHuman --> DefaultBranch: таймаут → ветка по умолчанию
    DefaultBranch --> Running
    Running --> Failed: превышен max_steps/timeout/token_budget/cost_budget
    Running --> Completed: последний шаг выполнен
    Failed --> [*]
    Completed --> [*]

    note right of Running
        Восстановление после сбоя:
        новое состояние = свёртка событий
        от точки последнего снапшота
    end note
```

## 2. Шаг с моделью (Mediation)

```mermaid
stateDiagram-v2
    [*] --> Parsing
    Parsing --> SchemaCheck: разобрано
    Parsing --> Retry: не разобрано
    Retry --> Parsing: попытка < 2
    Retry --> Escalate: попытка = 2

    SchemaCheck --> PolicyCheck: валидно
    SchemaCheck --> Retry: невалидно

    PolicyCheck --> Committed: политика соблюдена
    PolicyCheck --> Escalate: нарушение (без повтора)

    Committed --> [*]
    Escalate --> [*]
```

## 3. Само-обновление (см. также `runtime-view.md` §4)

```mermaid
stateDiagram-v2
    [*] --> CheckVersion
    CheckVersion --> Done: версия не старше локальной
    CheckVersion --> MajorGate: обнаружено обновление
    MajorGate --> HumanReview: мажорный бамп версии
    MajorGate --> ResolveArtifact: минор/патч
    HumanReview --> ResolveArtifact: подтверждено
    HumanReview --> [*]: отклонено
    ResolveArtifact --> Download
    Download --> Verify
    Verify --> FailUpdate: подпись/чек-сумма неверны
    Verify --> CheckpointCurrent: ok
    CheckpointCurrent --> Swap
    Swap --> SmokeTest
    SmokeTest --> Rollback: провал
    SmokeTest --> CommitUpdate: ok
    Rollback --> [*]
    CommitUpdate --> [*]
    FailUpdate --> [*]
    Done --> [*]
```

## 4. Класс модели в реестре Model Pool

```mermaid
stateDiagram-v2
    [*] --> Registered: ручная регистрация (паспорт провайдера)
    Registered --> Evaluated: офлайн-оценка на золотом наборе
    Evaluated --> Registered: метрики в норме
    Evaluated --> Degraded: рост отказов валидации → автособытие
    Degraded --> PendingUpgrade: метрики улучшились
    PendingUpgrade --> Evaluated: подтверждено человеком (I2)
    PendingUpgrade --> Degraded: не подтверждено
```

## 5. Связанные документы

`process-engine.md` §4–5; `mediation.md` §2, §5; `deployment.md` §5; `ideal-agent-architecture.md` §3.10; ADR-0010, ADR-0011, ADR-0012, ADR-0019.

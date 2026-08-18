# Runtime View — сценарии взаимодействия во времени

> Диаграммы последовательности для ключевых сценариев. Структурные представления (system/component) показывают связи; здесь — как именно они срабатывают по шагам. См. `ideal-agent-architecture.md` §4, `mediation.md` §2, `deployment.md` §5.
> Интерактивные версии: жизненный цикл задачи — [`diagrams/task-lifecycle.html`](diagrams/task-lifecycle.html); шаг с моделью через Mediation — [`diagrams/mediation-sequence.html`](diagrams/mediation-sequence.html).

## 1. Жизненный цикл задачи (структурированный процесс)

```mermaid
sequenceDiagram
    participant U as Поверхность
    participant R as Маршрутизатор (код)
    participant PE as Process Engine
    participant CTX as Context Engine
    participant EX as Executor
    participant MED as Mediation
    participant CAP as Capability
    participant ST as Состояние/Журнал

    U->>R: новая задача
    R->>PE: инстанцировать процесс (код-правило)
    loop по шагам графа
        PE->>CTX: build(step, state)
        CTX-->>PE: контекст (слои по бюджету класса модели)
        PE->>EX: run(step, ctx)
        EX-->>PE: сырой вывод
        PE->>MED: commit(step, output)
        MED-->>PE: патч | повтор (до 2) | эскалация
        PE->>CAP: проверка перед мутирующим вызовом
        CAP-->>PE: allow | deny | confirm
        PE->>ST: apply(patch) + emit step.applied
        ST-->>PE: snapshot (если шаг мутирует)
    end
    PE-->>U: результат (только публикуемые поля контракта)
```

## 2. Mediation: повтор и эскалация

```mermaid
sequenceDiagram
    participant EX as Executor
    participant MED as Mediation
    participant M as Модель
    participant H as human_gate

    EX->>MED: сырой вывод модели
    MED->>MED: parse
    alt разбор не удался
        MED->>M: повтор с подсказкой формата (до 2)
        M-->>MED: новый вывод
    end
    MED->>MED: schema
    alt ошибка схемы
        MED->>M: повтор с текстом ошибки валидации (до 2)
        M-->>MED: новый вывод
    end
    MED->>MED: policy
    alt нарушение политики
        MED->>H: эскалация без повтора
        H-->>MED: решение человека
    end
    MED->>EX: commit: патч состояния
```

## 3. CodeAct: генерация и изолированное исполнение

```mermaid
sequenceDiagram
    participant PE as Process Engine
    participant M as Модель
    participant SA as Статический анализ
    participant WT as Wasmtime (WASM)
    participant TR as Tool Runtime (стабы)
    participant MED as Mediation

    PE->>M: срез состояния + сигнатуры инструментов
    M-->>PE: программа (текст)
    PE->>SA: белый список идентификаторов
    alt запрещённая конструкция
        SA-->>PE: отказ, без запуска
    end
    PE->>WT: компиляция в WASM + запуск (лимит топлива/памяти)
    loop вызовы инструментов из программы
        WT->>TR: host-функция (стаб)
        TR->>TR: capability-гейт на каждый вызов
        TR-->>WT: результат
    end
    WT-->>PE: {логи, результат}
    PE->>MED: commit(результат по контракту шага)
```

## 4. Само-обновление агента

```mermaid
sequenceDiagram
    participant B as Bootstrap
    participant PE as agent-self-update (процесс)
    participant GH as GitHub Releases
    participant H as human_gate
    participant FS as Файловая система

    B->>PE: инстанцировать agent-self-update
    PE->>PE: check_version
    alt новая версия мажорная
        PE->>H: подтверждение обновления
        H-->>PE: подтверждено / отклонено
    end
    PE->>GH: resolve_artifact + download
    GH-->>PE: артефакт + подпись
    PE->>PE: verify (подпись против доверенного списка)
    alt верификация не прошла
        PE-->>B: fail_update (без повтора)
    else успех
        PE->>FS: checkpoint_current
        PE->>FS: swap (атомарная замена)
        PE->>PE: smoke_test на новом бинарнике
        alt smoke_test провален
            PE->>FS: rollback из чекпоинта
        else успех
            PE->>PE: commit_update
        end
    end
```

## 5. Установка плагина из доверенного репозитория

```mermaid
sequenceDiagram
    participant U as Пользователь
    participant PE as install-plugin (процесс)
    participant TL as Доверенный список
    participant GH as GitHub
    participant H as human_gate
    participant ACL as Реестр ACL плагинов

    U->>PE: запрос установки плагина
    PE->>TL: репозиторий уже в списке?
    alt новый репозиторий
        PE->>H: подтверждение нового источника
        H-->>PE: подтверждено → добавление в TL (событие)
    end
    PE->>GH: скачать релиз + подпись
    PE->>PE: verify по signer_identity из TL
    alt манифест запрашивает ACL шире capability_ceiling
        PE->>H: подтверждение расширения прав
    end
    PE->>ACL: регистрация манифеста (события, доступ к секретам)
    PE-->>U: плагин установлен, изолированный процесс запущен
```

## 6. Связанные документы

`ideal-agent-architecture.md` §4; `mediation.md` §2, §5; `executors.md` §4; `deployment.md` §5–6.

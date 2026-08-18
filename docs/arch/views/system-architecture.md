# Системная архитектура — Context, Container, Deployment

> C4, уровни 1–2, плюс диаграмма развёртывания. См. `ideal-agent-architecture.md` §2, `deployment.md`, `stack.md`.
> Интерактивная версия компонентной карты с привязкой к крейтам: [`diagrams/component-map.html`](diagrams/component-map.html).

## 1. Контекст (C4 Level 1)

Кто и что взаимодействует с системой снаружи.

```mermaid
flowchart TB
    User["Пользователь"]
    Owner["Администратор установки"]
    Agent(["Агентная система"])
    GH["GitHub\n(Releases, доверенные репозитории плагинов)"]
    NPMReg["npm-реестр\n(bootstrap-пакет)"]
    RemoteModels["Удалённые провайдеры моделей"]
    MCPExtServers["Внешние MCP-серверы инструментов"]
    MCPExtClients["Внешние MCP-клиенты"]

    User -->|запросы, подтверждения human_gate| Agent
    Owner -->|доверенный список, режим подтверждений, каналы обновления| Agent
    Agent -->|скачивание/верификация артефактов, установка плагинов| GH
    Agent -->|установка/обновление bootstrap| NPMReg
    Agent -->|структурированные и codeact-запросы| RemoteModels
    Agent -->|клиент по MCP| MCPExtServers
    MCPExtClients -->|сервер по MCP| Agent
```

## 2. Контейнеры (C4 Level 2)

Развёртываемые единицы внутри системы — см. `deployment.md` §2, `stack.md` §10.

```mermaid
flowchart TB
    subgraph host["Машина пользователя"]
        bootstrap["Bootstrap\n(npm, TypeScript, минимум зависимостей)"]
        core["Ядро агента\n(Rust, статический бинарник:\nProcess Engine, Mediation,\nContext Engine, Capability,\nActors/Scheduler, Model Pool)"]
        db[("SQLite\nсобытия · снапшоты · память · граф")]
        sandbox["CodeAct-песочница\n(Wasmtime + WASM)"]
        localmodel["Локальный инференс\n(llama.cpp, GGUF)"]
        plugin1["Плагин\n(изолированный процесс)"]
        plugin2["Плагин\n(изолированный процесс)"]
    end

    bootstrap -->|скачивает, верифицирует подпись,\nатомарно заменяет| core
    core -->|read/write| db
    core -->|компилирует и исполняет\nпрограмму шага| sandbox
    core -->|встроенный вызов| localmodel
    core <-->|MCP по IPC, ACL-гейт на каждый вызов| plugin1
    core <-->|MCP по IPC, ACL-гейт на каждый вызов| plugin2
```

## 3. Развёртывание (Deployment view)

Физическая топология поставки и обновления — см. `deployment.md` §5–7.

```mermaid
flowchart LR
    subgraph dev["CI (GitHub Actions)"]
        build["Кросс-платформенная сборка\n(cargo + cargo-zigbuild)"]
        sign["Подпись\n(sigstore/cosign, keyless OIDC)"]
    end
    subgraph reg["Реестры распространения"]
        ghrel["GitHub Releases\n(платформенные артефакты + подписи)"]
        npmreg["npm-реестр\n(bootstrap + provenance)"]
    end
    subgraph userhost["Машина пользователя"]
        bootstrap2["Bootstrap"]
        agentcore["Ядро агента"]
    end

    build --> sign --> ghrel
    build --> sign --> npmreg
    npmreg -->|npm install| bootstrap2
    bootstrap2 -->|определяет ОС/архитектуру,\nскачивает нужный артефакт| ghrel
    ghrel -->|верифицированный бинарник| agentcore
```

## 4. Таблица контейнеров

| Контейнер | Технология | Ответственность | Специфика |
|---|---|---|---|
| Bootstrap | TypeScript, npm | определение платформы, оркестрация обновления | минимум зависимостей (ADR-0025) |
| Ядро агента | Rust, статический бинарник | Process Engine, Mediation, Context Engine, Capability, Actors/Scheduler, Model Pool | один самодостаточный артефакт (I5, ADR-0020) |
| SQLite | встроенная БД | события, снапшоты, память (4 слоя), граф сущностей | единый файл (ADR-0021) |
| CodeAct-песочница | Wasmtime + WASM | изолированное исполнение сгенерированной программы | структурная изоляция (ADR-0022) |
| Локальный инференс | llama.cpp | инференс без внешнего сервиса | GGUF-веса (ADR-0024) |
| Плагины | изолированные процессы, MCP | интеграция с внешними системами | ACL-манифест, за границей I5 (ADR-0014) |

## 5. Связанные документы

`ideal-agent-architecture.md` §2; `deployment.md`; `stack.md`; ADR-0017, ADR-0020, ADR-0021, ADR-0022, ADR-0024, ADR-0025.

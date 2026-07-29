# Механизмы защиты агентных систем: Guardrails и capability-модели

> Собрано из базы berimor, документации Hermes/Claude Code и реального кода проекта Jarvis.

## 1. Две парадигмы защиты

| Аспект | Guardrails (классические) | Capability-модель |
|--------|---------------------------|-------------------|
| Когда срабатывает | Постфактум, после генерации/действия | До выполнения |
| Кто решает | Часто другой LLM (judge) | Детерминированный код |
| Что ловит | Семантические ошибки, токсичность, политики | Структурные нарушения, опасные операции |
| Можно ли обойти approval'ом | Иногда | Нет (блокировка безусловная) |
| Пример | OpenAI Agents SDK Guardrails, NeMo Guardrails | Jarvis `danger.go`, SSRF-гейт, jail |

**Ключевой принцип базы:** LLM-guardrails («модель проверяет модель») — красный флаг при слабых локальных моделях. Структурная защита (capability-модель, compliance-by-construction) надёжнее и дешевле. Guardrails остаются полезны как дополнение для семантики, но не как единственный слой.

## 2. Что задокументировано в базе

### Hermes Agent (`hermes.md`)

- **Approvals:** `approvals.mode` — `smart` (вспомогательный LLM оценивает деструктивные команды), `manual`, `off`/`--yolo`.
- **Redaction секретов** (`security.redact_secrets`) — включена по умолчанию: stdout терминала, `read_file`, веб-контент, сводки субагентов.
- **PII redaction** (`privacy.redact_pii`) — опционально, для шлюза.
- **Website blocklist** — запрет обращения к доменам.
- **Shell-hooks allowlist** — явное разрешение опасных shell-интеграций.
- **Tirith** — детектор опасных команд.

### Claude Code (`claude_code.md`)

- **Permission modes:** Manual / Accept edits / Plan / Auto (Auto — с фоновыми safety-checks).
- **Allowed commands** — явный список в `.claude/settings.json`.
- **Checkpoints** — снапшоты перед правками, откат независимо от git; не покрывают внешние системы (БД, API, деплои).
- **Hooks** — детерминированная автоматизация на событиях.

### OpenAI Agents SDK

- **Guardrails как примитив фреймворка** — валидация входов и выходов агента, параллельно с исполнением, fail-fast при нарушении.

### Compliance-by-construction (`safety_and_compliance.md`)

- **Медиационный интерфейс** — валидация выхода LLM по формальному контракту шага до перехода к следующему.
- **Uncertainty-oriented autonomy** — низкая калиброванная уверенность LLM → повышение риска → обязательный человек в контуре.
- **Процессные модели (BPMN/SOP)** как ограничители пространства действий.

### Springer-обзор (`surveys.md`)

- Угрозы мультиагентных систем: prompt injection между агентами, заражение общей памяти, имперсонация агента.
- Контрмеры: песочницы, DRIFT (Dynamic Rule-Based Defense with Injection Isolation).

## 3. Что реализовано в Jarvis (проверено по коду)

### Capability-модель терминала — `internal/tools/danger.go`

Статический анализ команды до выполнения; блокировка безусловная (approval не спасает).

- **Безусловный deny:** `mkfs/fdisk/wipefs/shred`, `dd of=/dev/`, fork-бомбы, `shutdown/reboot/poweroff/halt/init 0|6`, `sudo/doas`, рекурсивный `chmod/chown`, запись на блочные устройства (`> /dev/sdX`), `rm --no-preserve-root`.
- **Jail:** удаление вне workspace блокируется; отслеживаются `cd`-цепочки (`&&`, `;`, `||`, `|`) и все цели `rm`, не только последняя (баг из шестого ревью, закрыт).

### Approval-модель инструментов

- `RequiresApproval` у каждого инструмента: `git commit`, `checkpoint restore/prune`, `web` не-GET методы, fetch на приватные адреса.
- UI approval инлайн; конкурентный `Respond` (двойной клик/повтор RPC) закрыт `select/default` — первый побеждает, остальные no-op.

### SSRF-гейт — `internal/tools/ssrf.go`

GET на публичный адрес — свободно; GET/любой метод на внутренний адрес — только через approval.

### Jail для file/terminal/git — `internal/tools/profile.go`, `git.go`

- workdir-jail обязателен для file/terminal/git.
- Git-репозиторий вне jail запрещён; симлинки наружу ловятся (регрессионный тест).

### Vault-scrubber — `internal/vault/scrub.go`

- Маскировка секретов в трёх точках: tool args/output, мост bus→storage, approval-reason.
- LLM видит только алиасы, значения — никогда.
- `Vault.ReloadIfChanged` — подхват секретов, добавленных другим процессом при живом jarvisd (баг из ревью, закрыт).

### Skill Health Loop

Молчаливой мутации навыка/конфига нет: любое изменение — через событие + approval (инвариант 2 проекта).

### Plugin security — `internal/plugins/`

- `Manifest.Emits` — ACL топиков: коннектор не публикует события под чужим топиком; источник только статический `plugin.json` на диске.
- `allowedSecrets` — scoped credentials коннекторам (только `credential_ref`-поля, без `os.Getenv`-fallback на чужие переменные).
- `forwardToPlugin` — payload коннектору проходит scrubber.

### Checkpoints — `internal/tools/checkpoint.go`

Снапшоты перед мутациями; `restore` и `prune` всегда с approval (с ref и числом файлов в reason).

### Voice command policy — `internal/voice`

Политика голосовых команд реальна и протестирована (EnergyVAD/routing/command policy).

### Memory Guard — Context Engine

Дедуп фактов (хэши + cosine через `Store.Embed`) перед записью в семантическую память.

## 4. Классификация механизмов по точке срабатывания

| Точка | Механизм | Тип |
|-------|----------|-----|
| До выполнения команды | `danger.go` deny-паттерны, jail, SSRF-гейт | Capability, безусловная |
| До выполнения инструмента | `RequiresApproval` (git commit, web POST, restore) | Approval-гейт |
| В пути данных | Scrubber (3 точки), `forwardToPlugin` | Redaction |
| До мутации состояния | Skill Health Loop, checkpoints | Approval + snapshot |
| На границе плагинов | `Manifest.Emits`, `allowedSecrets` | ACL |
| При записи памяти | Guard (дедуп хэши + cosine) | Детерминированная фильтрация |
| Голосовой вход | Voice command policy | Policy |

## 5. Известный пробел

Из выбранной архитектуры (`architecture_analysis.md`) в Jarvis пока нет **универсального медиационного слоя валидации выхода LLM по контракту** на каждом шаге агентного цикла. Сейчас JSON-schema + ретраи есть только в воркерах Context Engine (Extractor/Compressor/Guard/Consolidator), но не как единый интерфейс для всех tool-вызовов агента. Зафиксировано как направление для будущего этапа, когда сбор данных перейдёт в постановку задач.

## 6. Ссылки

- `safety_and_compliance.md` — compliance-by-construction, guardrails vs capability.
- `architecture_analysis.md` — выбранная архитектура (сквозной слой approvals).
- `hermes.md`, `claude_code.md` — чужие реализации approvals/redaction/permissions.
- Код Jarvis: `internal/tools/danger.go`, `ssrf.go`, `web.go`, `git.go`, `checkpoint.go`, `internal/vault/scrub.go`, `internal/plugins/runtime.go`, `ui/approval.go`.

# Berimor

**Модель думает. Код решает.**

Универсальный агент для LLM с детерминированным ядром: маршрутизацию задач, ветвление процесса, отбор контекста и допуск к выполнению решает код — модель исполняет узкие, проверяемые шаги. Работает с локальными и облачными моделями, слабыми и сильными.

[![GitHub release](https://img.shields.io/github/v/release/devpilgrin/berimor?logo=github&label=release)](https://github.com/devpilgrin/berimor/releases/latest)
[![npm](https://img.shields.io/npm/v/berimor?logo=npm&label=npm)](https://www.npmjs.com/package/berimor)
[![CI](https://img.shields.io/github/actions/workflow/status/devpilgrin/berimor/ci.yml?branch=main&label=CI)](https://github.com/devpilgrin/berimor/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-719%20green-brightgreen)](#инфраструктура-проекта)

![Rust](https://img.shields.io/badge/Rust-stable-DEA584?logo=rust&logoColor=white)
![WebAssembly](https://img.shields.io/badge/sandbox-Wasmtime-654FF0?logo=webassembly&logoColor=white)
![QuickJS](https://img.shields.io/badge/guest-QuickJS-F7DF1E?logo=javascript&logoColor=black)
![SQLite](https://img.shields.io/badge/storage-SQLite%20%2B%20FTS5%20%2B%20vec-003B57?logo=sqlite&logoColor=white)
![tokio](https://img.shields.io/badge/async-tokio-0B7B8A)
![MCP](https://img.shields.io/badge/protocol-MCP-5B5BD6)
![ratatui](https://img.shields.io/badge/TUI-ratatui-E95420)
![sigstore](https://img.shields.io/badge/supply--chain-sigstore%20keyless-2E8B57)
![oxc](https://img.shields.io/badge/static%20analysis-oxc__parser-black)

---

## Зачем это нужно

Большинство «ИИ-агентов» устроены одинаково: модели дают набор инструментов и просят её саму решить, что делать. Для демо — удобно. В работе — ненадёжно: модель забывает шаги, выдумывает факты, сворачивает не туда, а опасная команда уходит в терминал по нажатию «y» на автомате.

Berimor построен на противоположном допущении: **модели нельзя доверять оркестровку — ей можно доверить исполнение.** Задача раскладывается на шаги заранее или руководится детерминированным циклом; всё, что выдаёт модель, проходит строгую проверку прежде, чем на это можно положиться; всё, что может навредить, проходит через гейт, который не отменяется нажатием Enter.

| | Типичный агентный CLI | Berimor |
|---|---|---|
| Кто решает, что делать дальше | Модель (надежда на здравомыслие) | Код (граф процесса, детерминированный цикл) |
| Сбой посреди задачи | «Перезапустите и помолитесь» | Журнал событий: продолжение ровно с места обрыва |
| Опасное действие | Подтверждение, которое усталость превращает в YOLO | Deny-статика: запрещённое не спрашивается вообще |
| Слабая/локальная модель | «Купите модель подороже» | Медиация: ретрай с объяснением ошибки → эскалация человеку |
| Расширения | Плагин получает всё | Субагент/плагин получает подмножество прав родителя — кодом |
| Воспроизводимость | Нет | Полная: журнал → replay → состояние на любой момент |

## Чем отличается

**1. Решения — детерминированный код, не текст в промпте.**
Ветвление, циклы, таймауты, параллельные ветви с join-барьером, миграция версий работающего процесса — всё это Process Engine, а не надежда на то, что модель помнит инструкции. Слабым моделям нельзя доверять отбор контекста и маршрутизацию — значит, этим занимается код.

**2. Безопасность — структура, а не дисциплина пользователя.**
Deny-таблица деструктивных операций не переизбирается подтверждением. Файловый jail не выходит за рабочую папку. Сетевой гейт не пускает в закрытые диапазоны (включая NAT64/6to4/Teredo-маскировки и обходы через редиректы и userinfo в URL). Секреты маскируются на всех точках утечки — но гейт допуска видит настоящие значения: маскировка не ослепляет проверку.

**3. Свободный цикл — под надзором.**
Режим «рассуждение → действие → наблюдение» для задач, которые не разложить по шагам заранее. Каждое действие внутри проходит тот же capability-гейт, что и шаг процесса — свобода рассуждения не значит свобода от правил. Опционально: самокритика и стратегия «предложи — выполни — проверь».

**4. Код модели исполняется в настоящей песочнице.**
Для «смержи 12 таблиц и найди аномалии» модель пишет JavaScript-программу. Она проходит статический анализ реальным парсером (белый список идентификаторов — `eval`/`Function`/`Math.random` отклоняются до исполнения), а исполняется QuickJS внутри WebAssembly (Wasmtime) с топливом, лимитом памяти и потолком вызовов инструментов. WASI — с пустым набором прав: ни файлов, ни сети даже потенциально. Единственная host-функция идёт через тот же гейт.

**5. Память — как инженерная система, а не как буфер.**
Рабочая память сворачивается при переполнении бюджета. Эпизодическая — полнотекстовый поиск (FTS5). Семантическая — дедупликация фактов, конфликты не перезаписываются молча, сбой хранилища неотличим от «фактов нет» и не порождает ложных дублей. Граф сущностей — связи между фактами, персистентный. Навыки — переиспользуемые рецепты решения похожих задач, читаемые файлы.

**6. Экосистема расширений с потолком прав.**
- **Скилы** (SKILL.md) — экспертные роли для чата: триггер — кодом (не моделью), потолок инструментов — фильтром диспетча.
- **Субагенты** (agent.yaml) — вложенный агентный цикл с собственным бюджетом и журналом; права ребёнка = пересечение с правами родителя, расшириться нельзя. Вложенное порождение — только с явным `allow_spawn: true`, глубина ограничена кодом.
- **Плагины** — изолированные процессы с ACL-манифестом и keyless-подписью sigstore: установка из доверенного списка с TOFU-подтверждением, как SSH.

Всё это устанавливается одной командой — из каталога или **любого git-репозитория**: `berimor skill install code-review-ru --from https://github.com/...`.

## Инфраструктура проекта

**Rust-workspace по крейту на компонент** — Process Engine, Mediation, Executors, Memory, Capability, Model Pool, Actors, Tool Runtime, Context Engine, Eval, Storage. Гостевой WASM-модуль (`codeact-guest/`) живёт отдельным crate и закоммичен как готовый артефакт — обычная сборка не замедляется.

**Дисциплина проверок.** Каждый релиз: `cargo fmt` + `clippy -D warnings` + `cargo test --workspace` (719 тестов: юнит, интеграционные, e2e через настоящий бинарник, золотые фикстуры процессов и вредоносных вводов). Критические компоненты проходят обязательное независимое ревью. Полный самостоятельный аудит (`docs/audit-2026-07-31.md`) — **все находки закрыты или осознанно задокументированы**.

**Supply chain как у взрослых.** Кросс-платформенные релизы (Linux x64/arm64, macOS arm64, Windows x64) с keyless-подписью cosign/sigstore — приватного ключа не существует нигде. Проверка: `berimor verify <архив>`. npm-публикация с provenance, SBOM (CycloneDX) в пайплайне, самообновление (`berimor self-update`) реализовано на примитивах Process Engine — тот же журнал и восстановление после сбоя, что у обычных процессов, а не ad hoc скрипт.

**Архитектура задокументирована до кода.** `docs/arch/` — самодостаточная спецификация, реализуемая на любом стеке; `docs/ADR/` — журнал решений с отклонёнными альтернативами; `docs/ROADMAP.md` — очередь задач с классом модели-исполнителя на каждую.

## Установка

### Способ 1: npm (проще всего)

```sh
npm install -g berimor
berimor --version
```

Установщик сам определяет платформу, скачивает подписанный бинарник из последнего релиза GitHub и сверяет SHA-256 до распаковки. Пакет публикуется с provenance (привязка сборки к CI-workflow).

### Способ 2: готовый бинарник с GitHub

Актуальные версии — на странице [релизов](https://github.com/devpilgrin/berimor/releases/latest). Ниже — команды для скачивания конкретной версии (замените `v0.19.0` на нужную, если вышла более новая).

**Linux** (x64 или arm64):

```sh
VERSION=v0.19.0
ARCH=x64   # или arm64
curl -LO "https://github.com/devpilgrin/berimor/releases/download/${VERSION}/berimor-${VERSION}-linux-${ARCH}.tar.gz"
tar -xzf "berimor-${VERSION}-linux-${ARCH}.tar.gz"
chmod +x berimor
sudo mv berimor /usr/local/bin/
berimor --version
```

**macOS** (только Apple Silicon — M1/M2/M3 и новее; сборки под Intel пока не публикуются, для Intel-Mac — способ 3 ниже):

```sh
VERSION=v0.19.0
curl -LO "https://github.com/devpilgrin/berimor/releases/download/${VERSION}/berimor-${VERSION}-darwin-arm64.tar.gz"
tar -xzf "berimor-${VERSION}-darwin-arm64.tar.gz"
xattr -d com.apple.quarantine berimor   # бинарник пока не подписан Apple — иначе Gatekeeper откажется его запускать
chmod +x berimor
sudo mv berimor /usr/local/bin/
berimor --version
```

**Windows** (x64), PowerShell:

```powershell
$Version = "v0.19.0"
Invoke-WebRequest -Uri "https://github.com/devpilgrin/berimor/releases/download/$Version/berimor-$Version-win32-x64.zip" -OutFile berimor.zip
Expand-Archive -Path berimor.zip -DestinationPath .
.\berimor.exe --version
```

Бинарник пока не подписан — Windows SmartScreen может показать предупреждение «Windows защитила ваш компьютер»: «Дополнительные сведения» → «Выполнить в любом случае». Чтобы вызывать `berimor` из любой папки, переложите `berimor.exe` в каталог, который уже есть в `PATH`, или добавьте текущую папку в `PATH` самостоятельно.

Каждый архив сопровождается файлом `<архив>.sigstore.json` — keyless-подпись cosign/sigstore, привязанная к идентичности CI-workflow, которым собран релиз (ADR-0026). Проверить: `berimor verify <архив>` — сама команда уже в скачанном бинарнике (устанавливает свежий доверенный корень sigstore по сети при первом вызове). Это независимая от Apple/Microsoft подпись — предупреждения Gatekeeper/SmartScreen выше она не снимает, они про отдельный, ещё не сделанный шаг.

### Способ 3: собрать из исходников (любая ОС)

Нужен только [Rust](https://rustup.rs/) (стабильная версия):

```sh
git clone https://github.com/devpilgrin/berimor.git
cd berimor
cargo build --release -p berimor-cli
./target/release/berimor --version
```

На Windows последняя команда — `.\target\release\berimor.exe --version`.

## Быстрый старт

```sh
berimor          # = berimor chat: интерактивный диалог с агентом
```

При первом запуске мастер предложит подключить модели из пресетов (Kimi, DeepSeek, OpenAI, Claude через OpenRouter, локальные через Ollama/llama.cpp/LM Studio) — выберите номера или имена, вставьте ключ API (он попадёт в `~/.config/berimor/secrets.env` с правами «только владелец», не в конфиг). Позже то же самое — `berimor setup` или прямо в чате командой `/models add`.

Полезные команды чата: `/help`, `/models`, `/skills`, `/config`, `/exit`.

Детерминированные процессы (декларативный YAML-план со строгими контрактами — основной «боевой» режим): `berimor run <process.yaml>`. Примеры процессов и конфигураций — в [`fixtures/golden/processes/`](fixtures/golden/processes/) и [`CONTRIBUTING.md`](CONTRIBUTING.md).

Расширения одной командой:

```sh
berimor skill install code-review-ru                                    # из каталога
berimor skill install my-skill --from https://github.com/user/repo      # из любого git
berimor agent install researcher
berimor plugin install devpilgrin/berimor-plugin-hello                  # подписанный плагин
berimor plugin install-local ./my-plugin --allow-unsigned               # локальный, осознанно
```

## Как устроен проект

| Слой | Директория | Содержимое |
|---|---|---|
| Ядро агента | `crates/` | Rust-workspace — по одному крейту на компонент: Process Engine, Mediation, Executors, Memory, Capability, Model Pool, Actors, Tool Runtime, Context Engine, Eval, Storage |
| Песочница CodeAct | `codeact-guest/` | QuickJS-гость под wasm32-wasip1 — отдельный crate, закоммичен как готовый артефакт |
| Bootstrap | `bootstrap/` | npm-пакет установщика/обновления (TypeScript), см. «Установка» выше |
| Архитектура | `docs/arch/` | самодостаточная спецификация — принципы, компоненты, диаграммы (`docs/arch/views/`). См. `docs/arch/README.md` |
| Решения | `docs/ADR/` | журнал архитектурных решений: контекст, альтернативы, последствия. См. `docs/ADR/README.md` |
| План разработки | `docs/ROADMAP.md` | очередь задач по фазам, декомпозиция на подзадачи, сложность, класс модели-исполнителя |
| Аудит | `docs/audit-2026-07-31.md` | независимый аудит безопасности — все находки закрыты или осознанно задокументированы |
| Тестовые данные | `fixtures/golden/` | золотые наборы: примеры процессов, контрактов, вредоносных вводов |
| Исследования | `docs/rnd/` | вспомогательный слой: источники и анализ существующих агентных фреймворков. См. `docs/rnd/README.md` |

`crates/` и `bootstrap/` — сам агент, код, написанный по очереди из `docs/ROADMAP.md`. `docs/arch/` — слой чистых решений позади него: не упоминает конкретные проекты и продукты (кроме `docs/arch/deployment.md` и `docs/arch/stack.md`, где это осознанное исключение), излагает архитектуру так, чтобы её можно было реализовать на любом стеке. `docs/ADR/` фиксирует, почему принято каждое решение, включая отклонённые альтернативы. `docs/rnd/` — вспомогательный слой источников, на который опиралось проектирование, не часть агента.

## Лицензия

Apache License 2.0 — см. [`LICENSE`](LICENSE).

## Участие

См. [`CONTRIBUTING.md`](CONTRIBUTING.md) и [`docs/ROADMAP.md`](docs/ROADMAP.md) для выбора задачи.

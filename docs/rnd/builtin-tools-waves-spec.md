# Спецификация волн встроенных инструментов (A/B/C) — контракт для субагентов

Дата: 2026-08-12. Основание: `docs/rnd/tool-gap-analysis-2026-08-12.md`.
Заказ на 10 инструментов тремя волнами. Каждый инструмент — отдельный
модуль в `crates/berimor-cli/src/`, реестр и политики — РОДИТЕЛЬ при
интеграции (субагентам `builtin_dispatch.rs` НЕ трогать, кроме явно
указанных в их контракте точек).

## Общие правила (обязательны всем)

- Подпись вызова: `berimor_executors::tool_only::{ToolDispatch, DispatchError}`,
  ответ — `serde_json::Value` (объект), ошибка — `DispatchError { tool, reason }`
  с говорящим текстом на русском.
- Общие хелперы уже pub(crate) в `builtin_dispatch.rs`:
  `BuiltinToolDispatch::resolve(raw)`, `::err(tool, reason)`,
  `read_string_capped(reader, cap) -> (String, bool truncated)`,
  константы `CONTENT_CAP` (1 МиБ), `TERMINAL_OUTPUT_CAP` (64 КиБ).
  Модули импортируют: `use crate::builtin_dispatch::BuiltinToolDispatch as Shared;`
  и зовут `Shared::resolve_from(root, raw)` / `Shared::err_str(tool, reason)`
  (свободные fn, экспонированные родителем — см. секцию «Клей родителя»).
- Doc-комментарий модуля на русском со ссылкой на эту спецификацию.
- Юнит-тесты — в самом модуле (`#[cfg(test)] mod tests`). Temp-каталоги:
  `std::env::temp_dir().join("berimor-<mod>-test-<tag>-<pid>")` + `create_dir_all`,
  tag различает тесты модуля (гонка temp-каталогов — известный камень).
- Тесты НЕ ходят в сеть и НЕ вызывают git-хостинг. Мок HTTP — только
  `std::net::TcpListener` на `127.0.0.1:0` (web.search).
- Тройная проверка субагентом перед отчётом: `cargo fmt --all` →
  `cargo clippy -p berimor-cli --all-targets -- -D warnings` →
  `cargo test -p berimor-cli --bin berimor <mod>`. Все вместе, не по отдельности.
- Golden-фикстуры: `fixtures/golden/tools/<tool>/` — входные файлы для
  тестов (текст без машинных путей; маркеры-подстановки при необходимости).
- mutates декларирует РОДИТЕЛЬ в `builtin_policies` — в отчёте субагент
  указывает ожидаемое значение с обоснованием.
- Зависимости: walkdir 2.5.0, regex 1.13.1, globset 0.4.19 уже добавлены
  родителем в `crates/berimor-cli/Cargo.toml` (все транзитивно в Cargo.lock —
  нового внешнего кода нет). Новые зависимости ЗАПРЕЩЕНЫ.
- Отчёт субагента: список файлов, публичные сигнатуры (точные, для клея),
  вывод трёх проверок (счётчики), mutates-рекомендация.

## Клей родителя (уже сделано в контракт-коммите)

`builtin_dispatch.rs` экспонирует pub(crate):

```rust
pub(crate) fn resolve_from(root: &Path, raw: &str) -> PathBuf;
pub(crate) fn err_str(tool: &str, reason: impl Into<String>) -> DispatchError;
pub(crate) const CONTENT_CAP: u64;
pub(crate) const TERMINAL_OUTPUT_CAP: u64;
```

Регистрация (родитель, после каждой волны): `BUILTIN_TOOLS` + ветка в
`call` (делегирует модулю: `<mod>::call(&self.workspace_root, args)`,
кроме terminal.bg/human.ask/memory — им нужен контекст, см. ниже) +
`builtin_policies` (mutates по таблице ниже).

## Волна A

### A1 `files.edit` — модуль `builtin_edit.rs`

Сигнатура: `pub fn call(root: &Path, args: &Value) -> Result<Value, DispatchError>`

Args: `{path: string, old_string: string, new_string: string, replace_all?: bool}`.
Поведение: читать файл (cap CONTENT_CAP, больше — ошибка «файл больше капа»);
подсчитать вхождения `old_string` (строковый поиск, НЕ regex):
0 → ошибка «якорь не найден»; >1 и !replace_all → ошибка «якорь не уникален (N вхождений)»;
заменить (`str::replace`/`replacen` — байтобезопасно для UTF-8), записать файл.
Ответ: `{path, replacements: N, bytes: <новый размер>}`.
Пустой `old_string` — ошибка. Файл не существует — ошибка (НЕ создавать).
mutates: **true**. Фикстура: `fixtures/golden/tools/files.edit/sample.md`.
Тесты: замена одна/все/не уникален/не найден/кириллица/кап.

### A2 `files.search` — модуль `builtin_search.rs`

Сигнатура: та же. Args: `{pattern: string, mode?: "content"|"files" (default "content"),
path?: string (default "."), glob?: string, limit?: number (default 100, cap 500),
context?: number (default 0, cap 5)}`.
Поведение: обход walkdir от resolve(path); пропускать скрытые каталоги,
`.git`, `target`; файлы больше CONTENT_CAP не читать (пропуск с пометкой в
`skipped`). mode=content: regex (компиляция pattern — ошибка «неверный regex»)
по строкам: `{path, line, text}` (+ N строк контекста в `context_lines`);
mode=files: globset из pattern, совпадение по относительному пути.
limit общий на matches; `truncated: true` при упоре. Ответ:
`{matches: [...], truncated, skipped}`.
mutates: **false**. Фикстура: `fixtures/golden/tools/files.search/tree/…`.
Тесты: content-совпадение с номером строки, files-mode по glob, limit/
truncated, пропуск .git/target, битый regex, кириллица, context.

### A3 `vcs.git` — модуль `builtin_vcs.rs`

Сигнатура: та же. Args: `{op: "status"|"diff"|"log"|"show", path?: string,
limit?: number (для log: default 20, cap 100), ref?: string (для show/diff)}`.
Реализация: системный `git` (НЕ libgit2), запуск в root, фиксированные
флаги: status→`git status --short`, diff→`git diff [--cached при ref="--cached"] [path]`,
log→`git log --oneline -n <limit> [-- path]`, show→`git show <ref> [-- path]`
(ref обязателен для show, default HEAD).
Произвольные флаги НЕ принимаются никогда (вне контракта — deny-friendly).
Таймаут 15 с (паттерн try_wait+kill из terminal.exec), вывод cap
TERMINAL_OUTPUT_CAP на поток. git отсутствует/не репозиторий → ошибка с
текстом stderr. Ответ: `{stdout, truncated}`.
mutates: **false**. Тесты: в temp-репозитории (`git init -q -b main` +
env GIT_AUTHOR_*/GIT_COMMITTER_* — чистые машины без user.email): status
пустой/грязный, log после 2 коммитов, diff видит правку, show HEAD, не
репозиторий → ошибка.

### A4 `web.search` — модуль `builtin_websearch.rs`

Сигнатура: та же. Args: `{query: string, limit?: number (default 10, cap 25)}`.
Endpoint: `https://html.duckduckgo.com/html/?q=<urlencoded(query)>`,
GET через reqwest blocking (тот же паттерн клиента, что `http.fetch` в
`builtin_dispatch.rs` — посмотреть и повторить: rustls, без редиректов,
таймаут 10 с, size-cap 512 КиБ, UA `berimor/<version>`).
Перед запросом — `berimor_capability::net_gate::check_host("html.duckduckgo.com", 443)`
(защита в глубину: гейт видит только query, хост конструируется внутри).
Парсинг результата ВРУЧНУЮ (без html-зависимости): блоки
`class="result__a" href="<url>"` → заголовок до `</a>` (теги внутри
срезать), `class="result__snippet"` → сниппет (теги срезать, entities
`&amp;`/`&quot;`/`&#x27;`/`&lt;`/`&gt;` раскодировать). DDG-прямой
redirect `//duckduckgo.com/l/?uddg=<urlencoded>` — распаковать целевой URL.
Ответ: `{results: [{title, url, snippet}], engine: "duckduckgo"}`.
mutates: **false**. Фикстура: `fixtures/golden/tools/web.search/ddg_sample.html`
(самодельная страница в формате DDG: 3 результата, один с uddg-redirect,
один со snippet с entities). Тесты: парсинг фикстуры (3 результата,
url распакован, entities раскодированы), пустая выдача, limit; запрос —
мок TcpListener (endpoint инжектируется параметром внутренней fn
`search_with_base(base, query, limit)`).

## Волна B

### B5 `todo.read` / `todo.write` — модуль `builtin_todo.rs`

Сигнатуры: `pub fn read(root: &Path) -> Result<Value, DispatchError>`,
`pub fn write(root: &Path, args: &Value) -> Result<Value, DispatchError>`.
Хранилище: `<root>/.berimor/todo.json` (каталог создаётся).
todo.write args: `{items: [{id: string, content: string, status: "pending"|"in_progress"|"completed"|"cancelled"}]}` —
ЗАМЕНА всего списка. Валидация: status из перечня (иначе ошибка с
именем поля), не более ОДНОГО in_progress (иначе ошибка), id непустые и
уникальные. JSON: `{"items": [...]}` (serde_json, без serde-derive —
ручная сборка/разбор через Value).
todo.read: `{items}`; файла нет — `{items: []}` (не ошибка).
mutates: **false** (обоснование в doc-комментарии: файл — внутренняя
бухгалтерия агента в .berimor/, как chat_history, не пользовательские
данные; гейт пропускает без вопроса).
Тесты: write→read круг, невалидный status, два in_progress, дубликат id,
read пустого, кириллица.

### B6 `terminal.start`/`terminal.output`/`terminal.kill` — модуль `builtin_terminal_bg.rs`

Родитель добавляет в `BuiltinToolDispatch` поле
`bg: crate::builtin_terminal_bg::BgRegistry` (Default в конструкторах) —
субагент пишет модуль со структурой:

```rust
#[derive(Default)]
pub struct BgRegistry { /* Arc<Mutex<HashMap<u64, BgProc>>> */ }
impl BgRegistry {
    pub fn start(&self, root: &Path, command: &str) -> Result<Value, DispatchError>;
    pub fn output(&self, id: u64, offset: usize) -> Result<Value, DispatchError>;
    pub fn kill(&self, id: u64) -> Result<Value, DispatchError>;
    pub fn next_id(&self) -> u64; // монотонный счётчик от 1
}
```

- start: `sh -c <command>` в root (та же оговорка про оболочку, что в
  terminal.exec), stdout/stderr — потоки-читатели в общий буфер с капом
  TERMINAL_OUTPUT_CAP на поток (кольцевой: при переполнении хранить ХВОСТ
  и флаг truncated). Ответ `{id}`.
- output: `{stdout, stderr, running: bool, truncated}` с offset (байты от).
- kill: kill() ребёнку; `{killed: bool}`; несуществующий id — ошибка.
- Реестр потокобезопасен (Mutex); `&self` вызовы.
mutates: start/kill — **true**, output — **false**.
Тесты: start `echo`-команды → output содержит текст; `sleep 30` →
running=true → kill → running=false; output с offset; несуществующий id;
кап буфера (yes-подобный вывод — маленький цикл печати, не `yes`).

### B7 `human.ask` — модуль `builtin_human.rs` + клей родителя

Самая глубокая проводка. Субагент пишет:

```rust
/// Запрос свободного ответа пользователя — отдельный канал, НЕ
/// capability-гейт (тот решает допуск, тут — данные от человека).
pub trait HumanAsker: Send + Sync {
    fn ask(&self, question: &str) -> Result<String, String>;
}

/// Диспетчер-обёртка (прецедент AgentRunDispatch): human.ask — сам,
/// остальное — inner.
pub struct HumanAskDispatch<'a> {
    pub asker: &'a dyn HumanAsker,
    pub inner: &'a dyn ToolDispatch,
}
impl ToolDispatch for HumanAskDispatch<'_> { /* human.ask → asker, args {question: string, options?: [string]}; ответ {answer} */ }
```

+ REPL-реализация `StdinAsker` (eprintln вопрос + read_line, EOF →
ошибка) в этом же модуле. TUI-реализация — РОДИТЕЛЬ (WorkerMsg::AskRequest
+ модал с вводом, канал answer как у ConfirmRequest).
mutates: **false**. Тесты: FakeAsker → ответ; options добавляются в
текст вопроса; ошибка asker → DispatchError.

## Волна C

### C8 `memory.search` / `memory.save` — модуль `builtin_memory.rs`

Исследовать API `berimor-memory` (semantic.rs: дедуп/поиск фактов;
episodic.rs: FTS5) и `berimor-storage::SqliteEventLog` (та же БД, что
журнал; путь — config.storage_path). Родитель даёт обёртку
`MemoryToolDispatch { storage_path: PathBuf, allow_writes: bool, inner }`
(конструируется в run.rs build_executor_bundle* из конфига).
- memory.search args: `{query: string, limit?: 10}` — семантический поиск
  фактов (semantic API); пусто — `{facts: []}`. Ответ `{facts: [{id, content, topic?}]}`.
- memory.save args: `{content: string, topic?: string}` — только при
  `[memory] tool_writes = true` в конфиге (НОВЫЙ флаг, добавляет родитель
  в MemoryConfig, default false); иначе ошибка «запись через инструмент
  отключена конфигом». Дедуп — через существующий semantic::dedup/fact_hash
  (конфликт — ответ `{status: "conflict"}` без перезаписи).
mutates: search — **false**; save — **false** (внутреннее хранилище, не
пользовательские данные — обоснование в doc, как у todo; запись и так за
флагом конфига — доверенная граница декларируется конфигом, не гейтом).
Тесты: in-memory SqliteEventLog (`open_in_memory()`); save выключен по
умолчанию → ошибка; save→search круг; дедуп-дубликат → status conflict.

### C9 `session.search` — модуль `builtin_sessions_search.rs`

Сигнатура: `pub fn call(sessions_dir: &Path, args: &Value) -> Result<Value, DispatchError>`
(родитель передаёт `config::global_dir()/sessions`, отсутствие — пустой ответ).
Args: `{query: string, limit?: 20, role?: "user"|"assistant"}`.
Скан `*.jsonl` (каждая строка — JSON; формат — см. `chat_history.rs`:
поля role/content/ts или аналогичные — сначала прочитать реальный формат).
Подстрочное совпадение query (регистронезависимо) по content; regex НЕ
требуется (подстрока). Ответ: `{matches: [{file, role, ts, excerpt}]}`,
excerpt — ±60 символов вокруг совпадения, кап.
mutates: **false**. Тесты: фикстурные jsonl в temp: находит по подстроке,
role-фильтр, limit, excerpt с контекстом, битая строка jsonl пропускается,
кириллица.

### C10 snapshots — РОДИТЕЛЬ (интеграция в files.write/files.edit)

Не делегируется: точки перехвата в `builtin_dispatch.rs` (общий файл).
- Перед перезаписью существующего файла в files.write/files.edit: копия в
  `<root>/.berimor/snapshots/<UTC-ts>/` с сохранением относительного пути;
  ротация — последние 50 каталогов (старые удаляются); файлы > CONTENT_CAP
  не снапшотятся (пометка в ответе `snapshot: "skipped"`).
- `snapshot.list` `{limit?: 20}` → `[{id, ts, paths}]`; `snapshot.restore`
  `{id, path?}` → восстановление (mutates=true).
- Модуль `builtin_snapshots.rs` — сам; точки вызова — в ветках write/edit.

## Последовательность (родитель)

1. Контракт-коммит: deps в Cargo.toml + helpers pub(crate) + этот документ.
2. Батч 1: A1+A2+A3 → интеграция → тройная проверка → коммит.
3. Батч 2: A4+B5+B6 → то же.
4. Батч 3: B7+C8+C9 → то же (+ родитель: TUI-asker, MemoryConfig-флаг,
   MemoryToolDispatch wiring, C10).
5. XL-ревью независимым субагентом (граница доверия: мутации ФС, git,
   сеть, человеческий канал) → фиксы.
6. README/ROADMAP-запись, релиз — по решению пользователя.

# Спека: контракты из конфигурации ([[contracts]]) — 2026-08-14

Источник: полевой тест 0.27.0 (macOS), раздел 06 — «главное препятствие к
прикладному использованию»: контракты только кодом (3 демонстрационных),
свой процесс без форка не описать.

## Цель

Оператор объявляет контракт в конфиге (JSON Schema) и использует его в
`llm_structured` (и `codeact`, если без дублирования) наравне с кодовыми.

## Объём

1. **Зависимость**: `jsonschema` (crate) в `berimor-executors` —
   САНКЦИОНИРОВАНО пользователем (отступление от правила «только
   транзитивные», зафиксировать в коммите).
2. **Конфиг** (`berimor-cli/src/config.rs`):
   ```toml
   [[contracts]]
   name = "MeetingMinutes"
   description = "протокол встречи"          # опционально
   schema = """{"type":"object", ...}"""     # inline JSON Schema
   # либо schema_path = "contracts/minutes.schema.json"
   ```
   - Config.contracts: Vec<ContractConfig>; при загрузке: имя уникально и
     НЕ совпадает с кодовыми контрактами (реестр structured_llm), схема
     парсится как JSON и компилируется `jsonschema::validator_for` —
     любая ошибка = ошибка загрузки конфига с понятным текстом.
3. **Реестр** (`berimor-executors/src/structured_llm.rs`):
   - `pub struct ConfigContract { name, description: Option<String>, schema: Value }`
   - `static CONFIG_CONTRACTS: OnceLock<Vec<ConfigContract>>`;
     `pub fn set_config_contracts(...)` (CLI вызывает один раз при старте:
     main.rs до диспетчеризации команд run/chat/observe/daemon/serve);
     `pub fn find_config_contract(name) -> Option<ConfigContract>`.
4. **Исполнение**: `execute()` (и codeact-execute, если без дублирования —
   иначе только llm_structured + документированное ограничение):
   `find_contract` промах → `find_config_contract`. Тот же цикл
   попыток/failover; промпт из JSON Schema (+description, без примера);
   медиация: parse JSON из raw (существующий parser berimor-mediation) →
   `jsonschema` валидация → ошибка = Retry с текстом ошибок в промпт
   следующей попытки; успех → `Patch { step_id, changes: объект }`.
   Policy-правил у конфиг-контрактов нет (ссылки на состояние не
   проверяются) — документировать; publishable = весь объект.
5. **Тесты**: парсинг конфига (inline/path/дубликат с кодовым/битая
   схема); execute через существующий mock-провайдер в тестах
   structured_llm: валид → patch; невалид → retry с причиной; исчерпание
   → ошибка.
6. **Доки**: README (абзац в разделе процессов), docs/process-engine.md
   (подраздел), пример в fixtures/golden/processes/ (contracts.toml +
   процесс, использующий конфиг-контракт).

## Соглашения

Комментарии на русском (стиль проекта), cargo fmt, clippy -D warnings,
тесты workspace зелёные. Коммиты НЕ делать — родитель.

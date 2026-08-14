# Спека: SGR-волна 0.30.0 — issues #3 (json_schema-транспорт) + #4 (reasoning-поля)

Источник: GitHub issues devpilgrin/berimor #3 и #4 (Dmitry-100), одобрены
пользователем 2026-08-14. Ключевая оговорка #4: порядок полей имеет смысл
только в связке с constrained decoding (#3) — реализуем вместе.

## Часть A (#4): reasoning-поля в контрактах

1. `crates/berimor-mediation/src/contracts.rs` — `ClassificationOut`:
   добавить `risk_factors: Vec<String>` (validate length min=1)
   ПЕРЕД полем `risk` (порядок объявления = порядок в схеме и генерации).
   Бамп `SCHEMA_VERSION` 1→2. Комментарий: поле-обоснование заполняется
   до целевого (SGR, issue #4).
2. Пример в реестре (structured_llm) дополнить risk_factors.
3. schemars сохраняет порядок полей структуры в `properties` — тест:
   в сериализованной схеме `risk_factors` идёт раньше `risk` (по индексу
   подстроки в JSON-тексте схемы).
4. Доки: docs/process-engine.md (раздел контрактов из конфигурации —
   правило «обоснования раньше целей», minItems=1), README RU абзац
   SGR (0.30.0) — после абзаца нормализатора.

## Часть B (#3): response_format перечисление + схема в запросе

1. `ProviderConfig` (berimor-cli/src/config.rs): новое поле
   `response_format: Option<String>` со значениями
   "none"|"json_object"|"json_schema"|"grammar"; не задано — derive из
   старого `json_object_response_format: bool` (обратная совместимость).
   Невалидное значение — ошибка конфига.
2. `CompletionRequest` (berimor-model-pool): + `json_schema: Option<Value>`.
3. `http_provider.rs`: при "json_schema" и наличии схемы —
   `response_format: {"type":"json_schema","json_schema":{"name":<contract_name>,
   "schema":<…>,"strict":true}}`; "json_object" — как сейчас; "none" —
   поле не отправляется. Без схемы — молчаливый даунгрейд до json_object
   НЕ делать: отправить как задано (или предупреждение в stderr — выбрать
   предупреждение).
4. Ollama-провайдер: "json_schema" → поле `format` = объект схемы
   (ollama принимает schema в format); "json_object" → format: "json".
5. `local_provider.rs` (llama.cpp server): "grammar" → GBNF из схемы
   ЕСЛИ конвертер уже есть/тривиален; иначе — отдельный комментарий-
   ограничение (llama-server принимает response_format json_schema —
   использовать его), GBNF отложить с пометкой в доке.
6. Проводка схемы от вызывающих: structured_llm::execute (adapter
   json_schema / config contract schema), agent_step decide_turn
   (schemars AgentTurnDecision). codeact не трогать (выход — программа).
7. Пресеты: ТОЛЬКО документированные значения по умолчанию в setup.rs
   НЕ менять без факта; DeepSeek — проверить живым вызовом (400 на
   json_schema → остаётся json_object).

## Тесты

- порядок полей в схеме ClassificationOut (risk_factors раньше risk);
- форма запроса http_provider для всех 4 значений response_format;
- ollama format=schema; конфиг: невалидное значение response_format —
  ошибка; обратная совместимость (только старый bool — json_object);
- workspace зелёный, clippy -D warnings, fmt.

## НЕ делать

GBNF-конвертер с нуля; менять пресеты провайдеров в setup без факта
поддержки; автоисправление вывода моделью (ADR-0002 — отклонено).
Коммиты — родитель.

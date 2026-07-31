//! CodeAct — модель пишет программу, изолированное выполнение в Wasmtime.
//!
//! Источник: `docs/arch/executors.md` §4, `ADR-0022` (структурная изоляция
//! через WASM, а не списковая), `docs/arch/stack.md` §4. ROADMAP:
//! **E6 — сделано** (этот модуль): встраивание Wasmtime как хоста.
//! **E7 — сделано** (`super::static_analysis`): белый список
//! идентификаторов над ТЕКСТОМ программы, ДО компиляции — независимый,
//! композируемый барьер (`ADR-0022`), не вызывается отсюда.
//! **E8 — сделано** (этот модуль): лимиты песочницы (топливо/память/число
//! вызовов host-функции), реальный гость на QuickJS
//! (`codeact-guest/`, `assets/codeact-guest.wasm`, коммитится собранным
//! — см. `codeact-guest/README.md`).
//!
//! ## ABI хост↔гость
//!
//! **Пересмотрен целиком в E8** — прежний протокол (пользовательские
//! смещения линейной памяти `INPUT_OFFSET`/`OUTPUT_OFFSET`, придуманные
//! для рукописных WAT-фикстур E6/E7) был явно провизорным («не
//! обещание совместимости»); реальному гостю на WASI (`rquickjs` тянет
//! wasi-libc) он не подходит — заменён на идиоматичный для
//! `wasm32-wasip1` command-модуль:
//!
//! - Гость — обычный WASI Preview1 "command" (`fn main()`, экспорт
//!   `_start`). Хост настраивает `WasiCtx` с `stdin` =
//!   `MemoryInputPipe(JSON входа)`, `stdout`/`stderr` =
//!   `MemoryOutputPipe`, вызывает `_start`, читает результат из
//!   захваченного stdout (успех) или stderr (отказ) — не из
//!   произвольных смещений памяти, которыми хост более не управляет
//!   напрямую.
//! - `env.call_tool(tool_ptr, tool_len, args_ptr, args_len, out_ptr,
//!   out_cap) -> i32` — ЕДИНСТВЕННОЕ, что осталось от прежнего
//!   протокола: синхронный callback посреди исполнения (для него stdio
//!   не годится — программа зовёт стаб инструмента в произвольной
//!   точке, не только в начале/конце). Семантика не изменилась: гость
//!   сам выбирает адреса в СВОЕЙ памяти, усечение — по возвращённой
//!   длине, `-1` — сентинел «хост не смог прочитать/записать по этим
//!   адресам» (порча указателя), НЕ про итог самого вызова инструмента.
//!   Отказ capability-гейта, сбой инструмента И исчерпание лимита
//!   вызовов (см. ниже) — все три идут ОДНИМ каналом, JSON-конвертом
//!   `{"ok": true, "value": ...}` / `{"ok": false, "error": ...}`, не
//!   `-1`: у хоста есть что сообщить гостю по существу, это не порча
//!   памяти.
//!
//! Различение успеха/отказа ВСЕГО прогона — по коду выхода `_start`
//! (WASI `proc_exit`, `wasmtime_wasi::I32Exit`), не по содержимому
//! stdout: `0` — гость вызвал `finish(result)`, stdout содержит JSON
//! `result`; иначе — стандартный отказ (`finish` не вызван,
//! необработанное исключение JS, невалидный вход), stderr содержит
//! читаемое сообщение (для JS-исключений — реальный текст, через
//! `ctx.catch()`, не просто факт исключения — см. `codeact-guest/src/main.rs`).
//!
//! ## Лимиты песочницы (E8, `executors.md` §4.2: «детерминированные прерыватели»)
//!
//! Три независимых предела, [`WasmLimits`]:
//! - **Топливо** (`fuel`) — `wasmtime` fuel-метрика (ADR-0022: «метрика
//!   топлива, а не wall-clock» — детерминированный лимит по числу
//!   инструкций). Исчерпание — трап при вызове `_start`.
//! - **Память** (`memory_bytes`) — потолок роста линейной памяти гостя
//!   через `wasmtime::StoreLimits`. Независим от собственного лимита
//!   кучи QuickJS внутри гостя (`codeact-guest`) — второй, более
//!   строгий рубеж на уровне движка JS, не единственная линия обороны.
//! - **Число вызовов инструмента** (`max_tool_calls`) — счётчик в
//!   `HostState`, инкрементируется в `host_call_tool`. Исчерпание НЕ
//!   обрывает исполнение гостя трапом — программа получает обычный
//!   отказ `{"ok": false, "error": "..."}"` тем же каналом, что и
//!   отказ capability-гейта/сбой инструмента (грациозная деградация:
//!   программа может доработать с тем, что уже получила, или сама
//!   решить остановиться — `finish`/исключение); что становится
//!   недоступно детерминированно — дальнейшие ЭФФЕКТЫ через
//!   инструменты, не сам факт исполнения WASM.
//!
//! WASI подключается (в отличие от исходного решения E6 — «WASI не
//! подключается вообще», пересмотрено здесь по необходимости: гостю на
//! `rquickjs`/wasi-libc нужен минимум окружения — аллокатор, стандартный
//! ввод/вывод) с ПУСТЫМ `WasiCtx`: без `inherit_stdio`/`inherit_env`,
//! без `preopened_dir`, без сети — deny-by-default. Та же гарантия «нет
//! ambient-доступа к ОС/сети», другой механизм: не полное отсутствие
//! WASI-рантайма (было — `Linker` не регистрировал ничего кроме
//! `call_tool`), а WASI-рантайм с пустыми правами (`WasiCtxBuilder::new()`
//! без единого гранта, кроме явно заданных `stdin`/`stdout`/`stderr` —
//! памятных pipe, не файлов/сети).

use crate::tool_only::{self, ConfirmationHandler, ToolDispatch};
use berimor_capability::CapabilityGate;
use berimor_types::capability::ConfirmationMode;
use serde_json::Value;
use std::sync::Arc;
use wasmtime::{
    Caller, Config, Engine, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder,
};
use wasmtime_wasi::p1::{self, WasiP1Ctx};
use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::{I32Exit, WasiCtxBuilder};

/// Ёмкость захваченного stdout гостя — верхняя граница размера JSON
/// результата ОДНОГО прогона CodeAct. Не тот же лимит, что
/// `call_tool`-ответы (те ограничены `CALL_TOOL_RESPONSE_CAP` внутри
/// `codeact-guest`) — независимые буферы разного назначения.
const STDOUT_CAPACITY_BYTES: usize = 1024 * 1024;
const STDERR_CAPACITY_BYTES: usize = 64 * 1024;

/// Реальный гость CodeAct (E8) — `codeact-guest/src/main.rs`, собран
/// под `wasm32-wasip1`, коммитится как артефакт (`codeact-guest/README.md`
/// — как пересобрать). `pub(crate)`, а не приватная: `codeact::executor`
/// (`CodeActExecutor`) передаёт этот же байткод в [`WasmHost::run`] на
/// каждую попытку — единственный на весь крейт "какой гость реально
/// исполняется", не выбор вызывающего кода снаружи `codeact`.
pub(crate) const GUEST_WASM: &[u8] = include_bytes!("../../assets/codeact-guest.wasm");

/// Три независимых предела песочницы — см. doc-комментарий модуля.
/// Значения — обоснованный, но не выведенный из спецификации выбор
/// (`executors.md` §4.3 говорит «уменьшенный лимит» для среднего класса
/// моделей, не называет чисел) — тот же класс решения, что
/// `MAX_ATTEMPTS = 3` у `StructuredLlm`.
#[derive(Debug, Clone, Copy)]
pub struct WasmLimits {
    pub fuel: u64,
    pub memory_bytes: usize,
    pub max_tool_calls: u32,
}

impl WasmLimits {
    /// Полный бюджет — для сильного класса моделей (`executors.md`
    /// §4.3: «полный CodeAct»).
    pub fn strong() -> Self {
        Self {
            fuel: 500_000_000,
            memory_bytes: 64 * 1024 * 1024,
            max_tool_calls: 32,
        }
    }

    /// Уменьшенный бюджет — для среднего/слабого класса
    /// (`executors.md` §4.3: «CodeAct с уменьшенным лимитом»). Допуск
    /// «слабый — только с явным разрешением в процессе» этот модуль НЕ
    /// проверяет — в системе нет понятия «явное разрешение в процессе»
    /// ни для одного шага; честно не закрытый пробел, тот же класс, что
    /// нереализованный `capability_ceiling` у `AgentStep` (E9).
    pub fn reduced() -> Self {
        Self {
            fuel: 100_000_000,
            memory_bytes: 16 * 1024 * 1024,
            max_tool_calls: 8,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WasmHostError {
    #[error("не удалось скомпилировать WASM-модуль: {0}")]
    Compile(String),
    #[error("не удалось инстанцировать WASM-модуль: {0}")]
    Instantiate(String),
    #[error("гость не экспортирует '{0}'")]
    MissingExport(String),
    #[error("ловушка (trap) во время исполнения гостя: {0}")]
    Trap(String),
    #[error("не удалось получить доступ к памяти гостя: {0}")]
    MemoryAccess(String),
    #[error("гость вернул невалидный UTF-8")]
    InvalidUtf8,
    #[error("гость вернул невалидный JSON: {0}")]
    InvalidJson(String),
    #[error("гость завершился с кодом {code}: {message}")]
    GuestFailed { code: i32, message: String },
}

/// Состояние `Store`. Держит `Arc<dyn Trait + Send + Sync>`, а не
/// заимствование (`&'a dyn Trait`, как у прочих исполнителей) — не
/// стилистический выбор: замыкания в `wasmtime::Linker::func_wrap`
/// обязаны быть `'static`, а `wasmtime_wasi::p1::add_to_linker_sync`
/// прямо требует `T: Send + 'static` — граница жёстче, чем была на
/// момент E6 (тогда WASI не подключался и `Send`/`Sync` компилятору не
/// требовались; теперь требуются реально, не по осторожности).
struct HostState {
    wasi: WasiP1Ctx,
    limits: StoreLimits,
    dispatch: Arc<dyn ToolDispatch + Send + Sync>,
    gate: Arc<dyn CapabilityGate + Send + Sync>,
    mode: ConfirmationMode,
    confirmer: Arc<dyn ConfirmationHandler + Send + Sync>,
    tool_calls_made: u32,
    max_tool_calls: u32,
}

/// Хост Wasmtime: компилирует и исполняет один гостевой WASM-модуль
/// (WASI Preview1 command) за вызов [`WasmHost::run`], с `call_tool`
/// как host-функцией-стабом инструмента и WASI, ограниченным пустым
/// `WasiCtx` (см. doc-комментарий модуля).
pub struct WasmHost {
    dispatch: Arc<dyn ToolDispatch + Send + Sync>,
    gate: Arc<dyn CapabilityGate + Send + Sync>,
    mode: ConfirmationMode,
    confirmer: Arc<dyn ConfirmationHandler + Send + Sync>,
}

impl WasmHost {
    pub fn new(
        dispatch: Arc<dyn ToolDispatch + Send + Sync>,
        gate: Arc<dyn CapabilityGate + Send + Sync>,
        mode: ConfirmationMode,
        confirmer: Arc<dyn ConfirmationHandler + Send + Sync>,
    ) -> Self {
        Self {
            dispatch,
            gate,
            mode,
            confirmer,
        }
    }

    /// Компилирует `wasm` (WASI Preview1 command-модуль — бинарный WASM
    /// или WAT-текст, `wasmtime` определяет формат автоматически),
    /// пишет `input` в stdin гостя, вызывает `_start`, читает результат
    /// из stdout (успех) или stderr (отказ). `limits` — см.
    /// [`WasmLimits`]. Полный ABI — doc-комментарий модуля.
    pub fn run(
        &self,
        wasm: &[u8],
        input: &Value,
        limits: &WasmLimits,
    ) -> Result<Value, WasmHostError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(|err| WasmHostError::Compile(err.to_string()))?;
        let module =
            Module::new(&engine, wasm).map_err(|err| WasmHostError::Compile(err.to_string()))?;

        let mut linker: Linker<HostState> = Linker::new(&engine);
        p1::add_to_linker_sync(&mut linker, |state: &mut HostState| &mut state.wasi)
            .map_err(|err| WasmHostError::Instantiate(err.to_string()))?;
        linker
            .func_wrap("env", "call_tool", host_call_tool)
            .map_err(|err| WasmHostError::Instantiate(err.to_string()))?;

        let input_bytes =
            serde_json::to_vec(input).map_err(|err| WasmHostError::InvalidJson(err.to_string()))?;
        let stdout = MemoryOutputPipe::new(STDOUT_CAPACITY_BYTES);
        let stderr = MemoryOutputPipe::new(STDERR_CAPACITY_BYTES);

        // Deny-by-default: ни inherit_stdio/inherit_env/args, ни
        // preopened_dir, ни сеть — единственное, что гость получает,
        // это память-pipe'ы, которые хост сам сюда положил.
        let wasi = WasiCtxBuilder::new()
            .stdin(MemoryInputPipe::new(input_bytes))
            .stdout(stdout.clone())
            .stderr(stderr.clone())
            .build_p1();

        let store_limits = StoreLimitsBuilder::new()
            .memory_size(limits.memory_bytes)
            .build();

        let mut store = Store::new(
            &engine,
            HostState {
                wasi,
                limits: store_limits,
                dispatch: Arc::clone(&self.dispatch),
                gate: Arc::clone(&self.gate),
                mode: self.mode,
                confirmer: Arc::clone(&self.confirmer),
                tool_calls_made: 0,
                max_tool_calls: limits.max_tool_calls,
            },
        );
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(limits.fuel)
            .map_err(|err| WasmHostError::Compile(err.to_string()))?;

        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|err| WasmHostError::Instantiate(err.to_string()))?;
        let start = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .map_err(|err| WasmHostError::MissingExport(format!("_start: {err}")))?;

        match start.call(&mut store, ()) {
            Ok(()) => read_result(&stdout),
            Err(err) => match err.downcast::<I32Exit>() {
                Ok(exit) if exit.0 == 0 => read_result(&stdout),
                Ok(exit) => Err(WasmHostError::GuestFailed {
                    code: exit.0,
                    message: read_text(&stderr),
                }),
                Err(err) => Err(WasmHostError::Trap(err.to_string())),
            },
        }
    }
}

fn read_result(pipe: &MemoryOutputPipe) -> Result<Value, WasmHostError> {
    let bytes = pipe.contents();
    let text = std::str::from_utf8(&bytes).map_err(|_| WasmHostError::InvalidUtf8)?;
    serde_json::from_str(text).map_err(|err| WasmHostError::InvalidJson(err.to_string()))
}

fn read_text(pipe: &MemoryOutputPipe) -> String {
    String::from_utf8_lossy(&pipe.contents()).into_owned()
}

/// Читает `len` байт по `ptr` из линейной памяти гостя. `None`, если
/// указатель/длина некорректны или выходят за пределы памяти —
/// вызывающий код трактует это как порчу со стороны гостя, не как
/// трап. Проверяет `ptr + len` против реального размера памяти гостя ДО
/// аллокации буфера (не после — независимое ревью E6 нашло здесь
/// host-side вектор исчерпания памяти на заявленной гостем длине
/// вплоть до `i32::MAX`).
fn read_guest_utf8(
    caller: &mut Caller<'_, HostState>,
    memory: &Memory,
    ptr: i32,
    len: i32,
) -> Option<String> {
    if ptr < 0 || len < 0 {
        return None;
    }
    let (ptr, len) = (ptr as usize, len as usize);
    let end = ptr.checked_add(len)?;
    if end > memory.data_size(&*caller) {
        return None;
    }
    let mut buf = vec![0u8; len];
    memory.read(&mut *caller, ptr, &mut buf).ok()?;
    String::from_utf8(buf).ok()
}

/// Host-функция `env.call_tool` — единственный НЕ-WASI импорт, который
/// регистрирует [`WasmHost`]. Читает имя инструмента и JSON аргументов
/// из памяти гостя, проверяет лимит числа вызовов, проводит вызов через
/// `tool_only::dispatch_confirmed` (тот же выбор точки входа, что у
/// `AgentStep`, E9 — capability-гейт не обходится), пишет JSON-конверт
/// `{"ok":...}` обратно в память гостя. Отказ капability-гейта, сбой
/// инструмента И исчерпание лимита вызовов — все три через этот
/// конверт, не через сентинел `-1` (тот — только про порчу
/// указателя/памяти, не про содержательный отказ).
#[allow(clippy::too_many_arguments)]
fn host_call_tool(
    mut caller: Caller<'_, HostState>,
    tool_ptr: i32,
    tool_len: i32,
    args_ptr: i32,
    args_len: i32,
    out_ptr: i32,
    out_cap: i32,
) -> i32 {
    let Some(memory) = caller
        .get_export("memory")
        .and_then(|export| export.into_memory())
    else {
        return -1;
    };

    let Some(tool) = read_guest_utf8(&mut caller, &memory, tool_ptr, tool_len) else {
        return -1;
    };
    let Some(args_raw) = read_guest_utf8(&mut caller, &memory, args_ptr, args_len) else {
        return -1;
    };
    let Ok(args) = serde_json::from_str::<Value>(&args_raw) else {
        return -1;
    };

    let (budget_ok, dispatch, gate, mode, confirmer) = {
        let state = caller.data_mut();
        let budget_ok = state.tool_calls_made < state.max_tool_calls;
        if budget_ok {
            state.tool_calls_made += 1;
        }
        (
            budget_ok,
            Arc::clone(&state.dispatch),
            Arc::clone(&state.gate),
            state.mode,
            Arc::clone(&state.confirmer),
        )
    };

    let envelope = if !budget_ok {
        serde_json::json!({
            "ok": false,
            "error": "лимит вызовов инструментов на эту программу исчерпан"
        })
    } else {
        match tool_only::dispatch_confirmed(
            &tool,
            &args,
            dispatch.as_ref(),
            gate.as_ref(),
            mode,
            confirmer.as_ref(),
        ) {
            Ok(value) => serde_json::json!({ "ok": true, "value": value }),
            Err(err) => serde_json::json!({ "ok": false, "error": err.to_string() }),
        }
    };

    let Ok(bytes) = serde_json::to_vec(&envelope) else {
        return -1;
    };
    let needed = bytes.len() as i32;
    if out_ptr < 0 || out_cap < 0 {
        return -1;
    }
    let to_write = bytes.len().min(out_cap as usize);
    if memory
        .write(&mut caller, out_ptr as usize, &bytes[..to_write])
        .is_err()
    {
        return -1;
    }
    needed
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_types::capability::{CapabilityDecision, ProposedAction};
    use serde_json::json;
    use tool_only::DispatchError;

    use super::GUEST_WASM;

    fn program(input: Value, js: &str) -> Value {
        json!({"program": js, "input": input})
    }

    struct CannedDispatch {
        expected_tool: &'static str,
        value: Value,
    }

    impl ToolDispatch for CannedDispatch {
        fn call(&self, tool: &str, _args: &Value) -> Result<Value, DispatchError> {
            if tool == self.expected_tool {
                Ok(self.value.clone())
            } else {
                Err(DispatchError {
                    tool: tool.to_string(),
                    reason: "неожиданный инструмент в тесте".to_string(),
                })
            }
        }
    }

    struct AlwaysFailsDispatch;
    impl ToolDispatch for AlwaysFailsDispatch {
        fn call(&self, tool: &str, _args: &Value) -> Result<Value, DispatchError> {
            Err(DispatchError {
                tool: tool.to_string(),
                reason: "намеренный сбой теста".to_string(),
            })
        }
    }

    /// Диспетч, который обязан НИКОГДА не быть вызван — если тест
    /// проходит, а этот двойник получил вызов, capability-гейт был
    /// обойдён.
    struct PanicIfCalledDispatch;
    impl ToolDispatch for PanicIfCalledDispatch {
        fn call(&self, _tool: &str, _args: &Value) -> Result<Value, DispatchError> {
            panic!("capability-гейт обойдён: диспетч вызван после отказа");
        }
    }

    struct AllowAll;
    impl CapabilityGate for AllowAll {
        fn check(&self, _action: &ProposedAction, _mode: ConfirmationMode) -> CapabilityDecision {
            CapabilityDecision::Allow
        }
    }

    struct DenyAll;
    impl CapabilityGate for DenyAll {
        fn check(&self, _action: &ProposedAction, _mode: ConfirmationMode) -> CapabilityDecision {
            CapabilityDecision::Deny {
                reason: "заблокировано тестом".to_string(),
            }
        }
    }

    struct AutoConfirm;
    impl ConfirmationHandler for AutoConfirm {
        fn confirm(&self, _action: &ProposedAction, _reason: &str) -> bool {
            true
        }
    }

    fn host(
        dispatch: impl ToolDispatch + Send + Sync + 'static,
        gate: impl CapabilityGate + Send + Sync + 'static,
    ) -> WasmHost {
        WasmHost::new(
            Arc::new(dispatch),
            Arc::new(gate),
            ConfirmationMode::Smart,
            Arc::new(AutoConfirm),
        )
    }

    #[test]
    fn happy_path_finish_produces_the_final_value() {
        let host = host(PanicIfCalledDispatch, AllowAll);
        let result = host
            .run(
                GUEST_WASM,
                &program(json!(null), "finish(1 + 1)"),
                &WasmLimits::strong(),
            )
            .unwrap();
        assert_eq!(result, json!(2));
    }

    #[test]
    fn input_global_carries_the_supplied_json() {
        let host = host(PanicIfCalledDispatch, AllowAll);
        let result = host
            .run(
                GUEST_WASM,
                &program(json!({"x": 5}), "finish(input.x * 2)"),
                &WasmLimits::strong(),
            )
            .unwrap();
        assert_eq!(result, json!(10));
    }

    #[test]
    fn call_tool_routes_through_dispatch_confirmed_and_returns_envelope() {
        let host = host(
            CannedDispatch {
                expected_tool: "echo_tool",
                value: json!({"echoed": true}),
            },
            AllowAll,
        );
        let result = host
            .run(
                GUEST_WASM,
                &program(
                    json!(null),
                    "var r = call_tool('echo_tool', {}); finish(r);",
                ),
                &WasmLimits::strong(),
            )
            .unwrap();
        assert_eq!(result, json!({"ok": true, "value": {"echoed": true}}));
    }

    #[test]
    fn capability_deny_blocks_call_tool_before_dispatch_is_ever_called() {
        let host = host(PanicIfCalledDispatch, DenyAll);
        let result = host
            .run(
                GUEST_WASM,
                &program(
                    json!(null),
                    "var r = call_tool('echo_tool', {}); finish(r);",
                ),
                &WasmLimits::strong(),
            )
            .unwrap();
        assert_eq!(result["ok"], json!(false));
        assert!(result["error"]
            .as_str()
            .unwrap()
            .contains("заблокировано тестом"));
    }

    #[test]
    fn dispatch_failure_is_recoverable_and_still_returns_an_envelope() {
        let host = host(AlwaysFailsDispatch, AllowAll);
        let result = host
            .run(
                GUEST_WASM,
                &program(
                    json!(null),
                    "var r = call_tool('echo_tool', {}); finish(r);",
                ),
                &WasmLimits::strong(),
            )
            .unwrap();
        assert_eq!(result["ok"], json!(false));
        assert!(result["error"]
            .as_str()
            .unwrap()
            .contains("намеренный сбой теста"));
    }

    #[test]
    fn tool_call_budget_exhausted_returns_envelope_not_a_trap() {
        let host = host(
            CannedDispatch {
                expected_tool: "echo_tool",
                value: json!(1),
            },
            AllowAll,
        );
        let limits = WasmLimits {
            max_tool_calls: 1,
            ..WasmLimits::strong()
        };
        let result = host
            .run(
                GUEST_WASM,
                &program(
                    json!(null),
                    "call_tool('echo_tool', {}); var r = call_tool('echo_tool', {}); finish(r);",
                ),
                &limits,
            )
            .unwrap();
        assert_eq!(result["ok"], json!(false));
        assert!(result["error"].as_str().unwrap().contains("лимит"));
    }

    #[test]
    fn unhandled_js_exception_surfaces_as_guest_failed_with_real_message() {
        let host = host(PanicIfCalledDispatch, AllowAll);
        let err = host
            .run(
                GUEST_WASM,
                &program(json!(null), "throw new Error('boom')"),
                &WasmLimits::strong(),
            )
            .unwrap_err();
        match err {
            WasmHostError::GuestFailed { code, message } => {
                assert_eq!(code, 1);
                assert!(message.contains("boom"), "{message}");
            }
            other => panic!("ожидался GuestFailed, получено {other:?}"),
        }
    }

    #[test]
    fn program_that_never_calls_finish_is_a_guest_failure() {
        let host = host(PanicIfCalledDispatch, AllowAll);
        let err = host
            .run(
                GUEST_WASM,
                &program(json!(null), "1 + 1"),
                &WasmLimits::strong(),
            )
            .unwrap_err();
        assert!(matches!(err, WasmHostError::GuestFailed { .. }), "{err:?}");
    }

    #[test]
    fn fuel_exhaustion_traps_instead_of_hanging() {
        let host = host(PanicIfCalledDispatch, AllowAll);
        let tiny = WasmLimits {
            fuel: 1_000,
            ..WasmLimits::strong()
        };
        let err = host
            .run(GUEST_WASM, &program(json!(null), "while (true) {}"), &tiny)
            .unwrap_err();
        assert!(matches!(err, WasmHostError::Trap(_)), "{err:?}");
    }

    #[test]
    fn memory_limit_stops_growth_instead_of_letting_it_run_unbounded() {
        let host = host(PanicIfCalledDispatch, AllowAll);
        // Одна страница WASM (64 КиБ) заведомо мало для инициализации
        // QuickJS/wasi-libc (аллокатору некуда расти) — гость обязан
        // упасть управляемо, не повиснуть и не дать хосту попытаться
        // выделить сколько угодно. На практике манифестируется ещё на
        // инстанцировании: у гостя есть минимальный размер памяти
        // (статические данные wasi-libc), уже превышающий такой
        // потолок — `Instantiate` тут не менее весомое доказательство
        // ограничения, чем трап во время исполнения (`memory.grow`).
        let tiny_memory = WasmLimits {
            memory_bytes: 64 * 1024,
            ..WasmLimits::strong()
        };
        let err = host
            .run(GUEST_WASM, &program(json!(null), "finish(1)"), &tiny_memory)
            .unwrap_err();
        assert!(
            matches!(
                err,
                WasmHostError::Trap(_)
                    | WasmHostError::GuestFailed { .. }
                    | WasmHostError::Instantiate(_)
            ),
            "{err:?}"
        );
    }
}

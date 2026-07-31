//! CodeAct — модель пишет программу, изолированное выполнение в Wasmtime.
//!
//! Источник: `docs/arch/executors.md` §4, `ADR-0022` (структурная изоляция
//! через WASM, а не списковая), `docs/arch/stack.md` §4. ROADMAP:
//! **E6 — сделано** (этот модуль): встраивание Wasmtime как хоста —
//! компиляция/инстанцирование гостевого WASM-модуля, вызов его
//! экспортированной точки входа, host-функция `call_tool` как стаб
//! инструмента, проведённая через тот же `tool_only::dispatch_confirmed`,
//! что `ToolOnly`/`AgentStep` (capability-гейт не обходится,
//! `executors.md` §7).
//!
//! Осознанно НЕ входит в E6 (следующие задачи ROADMAP §9):
//! - **E7** (сделано, `super::static_analysis`) — белый список
//!   идентификаторов над ТЕКСТОМ программы, ДО компиляции. Этот модуль
//!   исполняет ЛЮБОЙ корректный WASM-модуль, поданный в
//!   [`WasmHost::run`] — не только «программу на ограниченном
//!   подмножестве JS», и НЕ вызывает `static_analysis::analyze`
//!   изнутри: это два независимых, композируемых барьера
//!   (`ADR-0022`), не один встроенный в другой; связывающий их код —
//!   у вызывающей стороны (задача E8).
//! - Реальный JS-движок (`stack.md` §4: встроенный QuickJS,
//!   компилируемый в WASM) — тоже НЕ входит ни в E6, ни в E7 буквально:
//!   ни одна строка ROADMAP §9 не называет эту работу явно. `analyze()`
//!   проверяет ТЕКСТ JS-программы; [`WasmHost::run`] исполняет уже
//!   ГОТОВЫЙ WASM. Компиляция проверенного JS в то, что реально запустит
//!   `WasmHost` — честно не закрытый пробел, не задача какой-то одной
//!   уже существующей строки ROADMAP; понадобится либо в рамках E8, либо
//!   отдельной задачей — решать при подключении `StepKind::CodeAct`.
//! - **E8** — лимиты песочницы (топливо/память/число вызовов host-функции
//!   за прогон) и проводка результата через Mediation в `Patch`.
//!   [`WasmHost::run`] НЕ ограничивает время/память исполнения гостя —
//!   тот же класс честно не закрытого пробела, что `ProcessLimits.token_budget`
//!   (P6) и бюджет токенов `AgentStep` (E9) до появления реального
//!   прерывателя. Тестовые WAT-фикстуры этого модуля — авторские и
//!   заведомо не зацикливаются.
//! - `StepKind::CodeAct` не подключён к `CliExecutor` — подключать пока
//!   нечего целиком (нет лимитов, нет проводки результата через
//!   Mediation, нет компиляции проверенного JS в WASM) — это задача E8.
//!
//! ## ABI хост↔гость
//!
//! Внутренний протокол ЭТОЙ задачи, не финальный провод CodeAct — у гостя
//! нет `alloc`, вместо этого фиксированное соглашение об адресах в его
//! линейной памяти (гость сам решает, что там лежит статически, при
//! написании/компиляции гостевого модуля):
//!
//! - [`INPUT_OFFSET`] (байт 0) — куда хост пишет JSON входа ПЕРЕД вызовом
//!   `run`.
//! - [`OUTPUT_OFFSET`] (байт 4096) — откуда хост читает JSON результата
//!   ПОСЛЕ вызова `run`.
//! - Экспорт `run(input_len: i32, out_cap: i32) -> i32` — гость читает
//!   `input_len` байт входа с [`INPUT_OFFSET`], пишет результат (не более
//!   `out_cap` байт) на [`OUTPUT_OFFSET`], возвращает РЕАЛЬНУЮ длину
//!   результата. Если она больше `out_cap` — хост трактует это как
//!   усечение ([`WasmHostError::Truncated`]) и не читает память вовсе
//!   (не читает мусор за пределами того, что гость подтвердил как
//!   записанное).
//! - Импорт `env.call_tool(tool_ptr, tool_len, args_ptr, args_len, out_ptr,
//!   out_cap) -> i32` — гость сам выбирает адреса (это его собственная
//!   память, он вызывающая сторона); хост читает имя инструмента и JSON
//!   аргументов, вызывает `tool_only::dispatch_confirmed`, пишет ответ как
//!   `{"ok": true, "value": ...}` или `{"ok": false, "error": ...}` —
//!   ОБА пути (отказ capability-гейта/подтверждения и сбой самого
//!   инструмента) идут этим каналом, не трапом. В отличие от `AgentStep`
//!   (E9), где отказ capability-слоя терминален на уровне
//!   Rust-исполнителя (останавливает цикл), а сбой самого инструмента —
//!   нет: здесь host-функция не решает за гостя, что терминально, а что
//!   нет — оба исхода одинаково нетерминальны НА ГРАНИЦЕ хоста, решение
//!   «что делать дальше» целиком у гостевой программы (E7 может дать ей
//!   средства различать их по содержимому `"error"`). Гейт при этом не
//!   обходится и не «протухает» в разрешение — отрабатывает заново на
//!   каждый вызов, независимо от предыдущих. Возвращает длину так же,
//!   как `run` — с усечением по
//!   тому же принципу. Отдельный сентинел `-1` — не про итог вызова
//!   инструмента, а про то, что ХОСТ не смог прочитать память гостя по
//!   переданным `ptr`/`len` (испорченный указатель со стороны гостя).
//!
//! E7, встраивая реальный JS-движок со своим C ABI, может пересмотреть
//! этот протокол целиком — это не обещание совместимости.

use crate::tool_only::{self, ConfirmationHandler, ToolDispatch};
use berimor_capability::CapabilityGate;
use berimor_types::capability::ConfirmationMode;
use serde_json::Value;
use std::sync::Arc;
use wasmtime::{Caller, Engine, Linker, Memory, Module, Store, TypedFunc};

/// Смещение в линейной памяти гостя, куда хост пишет JSON входа перед
/// вызовом `run` (см. doc-комментарий модуля — «ABI хост↔гость»).
pub const INPUT_OFFSET: usize = 0;

/// Смещение в линейной памяти гостя, откуда хост читает JSON результата
/// после вызова `run` (см. doc-комментарий модуля — «ABI хост↔гость»).
pub const OUTPUT_OFFSET: usize = 4096;

/// Ёмкость буфера результата, которую хост сообщает гостю через `out_cap`
/// параметр `run` — весь остаток первой страницы линейной памяти после
/// [`OUTPUT_OFFSET`]. Единственная страница (64 КиБ) — минимум, который
/// требует WASM MVP; тестовые фикстуры этой задачи меньше на порядки.
const WASM_PAGE_SIZE: usize = 65536;

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
    #[error("результат гостя ({needed} байт) не поместился в буфер хоста ({cap} байт)")]
    Truncated { needed: i32, cap: i32 },
}

/// Состояние `Store` — то, что нужно host-функциям, зарегистрированным в
/// `Linker`, чтобы дойти до `tool_only::dispatch_confirmed`. Держит
/// `Arc`, а не заимствование (`&'a dyn Trait`, как у прочих исполнителей
/// — `tool_only.rs`/`agent_step.rs`) — не стилистический выбор:
/// замыкания, зарегистрированные в `wasmtime::Linker::func_wrap`, обязаны
/// быть `'static` (`Store`/`Linker` могут пережить конкретный вызов
/// `WasmHost::run`), а заимствование с произвольным временем жизни этому
/// требованию не удовлетворяет. `Arc`-обёртка на границе встраивания
/// хоста — устоявшийся паттерн `wasmtime`, не изобретение этой задачи.
struct HostState {
    dispatch: Arc<dyn ToolDispatch>,
    gate: Arc<dyn CapabilityGate>,
    mode: ConfirmationMode,
    confirmer: Arc<dyn ConfirmationHandler>,
}

/// Хост Wasmtime: компилирует и исполняет один гостевой WASM-модуль за
/// вызов [`WasmHost::run`], с `call_tool` как host-функцией-стабом
/// инструмента. Никакой WASI-рантайм не подключается — `Linker`
/// регистрирует ровно одну явно определённую host-функцию, ничего
/// больше; гостевой модуль, попытавшийся импортировать что-то ещё (в
/// том числе что угодно из `wasi_snapshot_preview1`), не слинкуется —
/// это и есть «нет ambient-доступа к ОС/сети» структурно, а не через
/// выключенные права внутри WASI-контекста.
pub struct WasmHost {
    dispatch: Arc<dyn ToolDispatch>,
    gate: Arc<dyn CapabilityGate>,
    mode: ConfirmationMode,
    confirmer: Arc<dyn ConfirmationHandler>,
}

impl WasmHost {
    pub fn new(
        dispatch: Arc<dyn ToolDispatch>,
        gate: Arc<dyn CapabilityGate>,
        mode: ConfirmationMode,
        confirmer: Arc<dyn ConfirmationHandler>,
    ) -> Self {
        Self {
            dispatch,
            gate,
            mode,
            confirmer,
        }
    }

    /// Компилирует `wasm` (бинарный WASM или WAT-текст — `wasmtime`
    /// определяет формат автоматически), пишет `input` на
    /// [`INPUT_OFFSET`], вызывает экспортированную `run`, читает результат
    /// с [`OUTPUT_OFFSET`]. См. doc-комментарий модуля — полный ABI.
    pub fn run(&self, wasm: &[u8], input: &Value) -> Result<Value, WasmHostError> {
        let engine = Engine::default();
        let module =
            Module::new(&engine, wasm).map_err(|err| WasmHostError::Compile(err.to_string()))?;

        let mut linker: Linker<HostState> = Linker::new(&engine);
        linker
            .func_wrap("env", "call_tool", host_call_tool)
            .map_err(|err| WasmHostError::Instantiate(err.to_string()))?;

        let mut store = Store::new(
            &engine,
            HostState {
                dispatch: Arc::clone(&self.dispatch),
                gate: Arc::clone(&self.gate),
                mode: self.mode,
                confirmer: Arc::clone(&self.confirmer),
            },
        );

        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|err| WasmHostError::Instantiate(err.to_string()))?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| WasmHostError::MissingExport("memory".to_string()))?;

        let input_bytes =
            serde_json::to_vec(input).map_err(|err| WasmHostError::InvalidJson(err.to_string()))?;
        memory
            .write(&mut store, INPUT_OFFSET, &input_bytes)
            .map_err(|err| WasmHostError::MemoryAccess(err.to_string()))?;

        let run_fn: TypedFunc<(i32, i32), i32> = instance
            .get_typed_func(&mut store, "run")
            .map_err(|err| WasmHostError::MissingExport(format!("run: {err}")))?;

        let out_cap = (WASM_PAGE_SIZE - OUTPUT_OFFSET) as i32;
        let written = run_fn
            .call(&mut store, (input_bytes.len() as i32, out_cap))
            .map_err(|err| WasmHostError::Trap(err.to_string()))?;

        if written < 0 {
            return Err(WasmHostError::Trap(format!(
                "гость вернул отрицательную длину результата: {written}"
            )));
        }
        if written > out_cap {
            return Err(WasmHostError::Truncated {
                needed: written,
                cap: out_cap,
            });
        }

        let mut buf = vec![0u8; written as usize];
        memory
            .read(&store, OUTPUT_OFFSET, &mut buf)
            .map_err(|err| WasmHostError::MemoryAccess(err.to_string()))?;

        let text = String::from_utf8(buf).map_err(|_| WasmHostError::InvalidUtf8)?;
        serde_json::from_str(&text).map_err(|err| WasmHostError::InvalidJson(err.to_string()))
    }
}

/// Читает `len` байт по `ptr` из линейной памяти гостя. `None`, если
/// указатель/длина некорректны или выходят за пределы памяти — вызывающий
/// код трактует это как порчу со стороны гостя, не как трап.
///
/// Проверяет `ptr + len` против реального размера памяти гостя ДО
/// аллокации буфера — независимого ревью нашло, что аллокация
/// `vec![0u8; len]` для необоснованного `len` (гость волен передать
/// вплоть до `i32::MAX`, независимо от того, что его собственная память
/// — одна страница в 64 КиБ) до вызова `memory.read` (единственного
/// места, которое реально делает bounds-check) — host-side вектор
/// исчерпания памяти: не тот же класс пробела, что отсутствие
/// fuel-лимитов (E8, про число WASM-инструкций гостя), а ошибка
/// дисциплины «проверяй перед аллокацией» именно в этой функции.
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

/// Host-функция `env.call_tool` — единственный импорт, который регистрирует
/// [`WasmHost`]. Читает имя инструмента и JSON аргументов из памяти гостя,
/// проводит вызов через `tool_only::dispatch_confirmed` (тот же выбор
/// точки входа, что у `AgentStep`, E9 — capability-гейт не обходится),
/// пишет JSON-конверт `{"ok":...}` обратно в память гостя.
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

    let envelope = {
        let state = caller.data();
        match tool_only::dispatch_confirmed(
            &tool,
            &args,
            state.dispatch.as_ref(),
            state.gate.as_ref(),
            state.mode,
            state.confirmer.as_ref(),
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

    /// Модуль без импортов: `run` игнорирует вход и всегда возвращает
    /// один и тот же статический JSON — проверяет голое встраивание
    /// (компиляция → инстанцирование → вызов экспорта → чтение памяти)
    /// без единой host-функции.
    const WAT_ECHO_STATIC: &str = r#"
        (module
          (memory (export "memory") 1)
          (data (i32.const 4096) "{\"hello\":\"world\"}")
          (func (export "run") (param $input_len i32) (param $out_cap i32) (result i32)
            i32.const 17))
    "#;

    /// `run` исполняет `unreachable` — гость обязан упасть трапом, хост
    /// обязан вернуть управляемую ошибку, а не запаниковать сам.
    const WAT_TRAP: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "run") (param i32 i32) (result i32)
            unreachable))
    "#;

    /// Гость объявляет импорт из WASI — `Linker` в `WasmHost` не
    /// регистрирует WASI вообще, поэтому линковка обязана упасть.
    /// Структурное доказательство «нет ambient-доступа», не список
    /// запретов.
    const WAT_UNRESOLVED_WASI_IMPORT: &str = r#"
        (module
          (import "wasi_snapshot_preview1" "fd_write"
            (func $fd_write (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (func (export "run") (param i32 i32) (result i32)
            i32.const 0))
    "#;

    /// Гость зовёт `env.call_tool` с зашитыми `tool`/`args`
    /// (`"echo_tool"`, `"{}"`) и напрямую возвращает то, что вернул
    /// host — проверяет весь путь host-функция → `dispatch_confirmed` →
    /// ответ обратно в гостя.
    const WAT_CALL_TOOL: &str = r#"
        (module
          (import "env" "call_tool"
            (func $call_tool (param i32 i32 i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 512) "echo_tool")
          (data (i32.const 600) "{}")
          (func (export "run") (param $input_len i32) (param $out_cap i32) (result i32)
            (call $call_tool
              (i32.const 512) (i32.const 9)
              (i32.const 600) (i32.const 2)
              (i32.const 4096) (local.get $out_cap))))
    "#;

    /// `run` всегда заявляет длину результата, заведомо превышающую
    /// ёмкость буфера хоста, ничего реально не записывая — проверяет,
    /// что усечение обнаруживается по возвращённой длине, а не читается
    /// как мусор за пределами записанного.
    const WAT_OVERSIZED_RESULT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "run") (param i32 i32) (result i32)
            i32.const 999999))
    "#;

    /// Гость зовёт `call_tool` с отрицательной `tool_len` — проверяет
    /// сентинел `-1` (порча указателя со стороны гостя), НЕ канал
    /// `{"ok":false,...}`. `run` напрямую возвращает то, что вернул
    /// `call_tool`, поэтому `-1` долетает до `WasmHost::run` как
    /// отрицательная длина результата — тот же путь, что и обычный
    /// отрицательный возврат `run`.
    const WAT_CALL_TOOL_NEGATIVE_LEN: &str = r#"
        (module
          (import "env" "call_tool"
            (func $call_tool (param i32 i32 i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 512) "echo_tool")
          (func (export "run") (param $input_len i32) (param $out_cap i32) (result i32)
            (call $call_tool
              (i32.const 512) (i32.const -1)
              (i32.const 512) (i32.const 9)
              (i32.const 4096) (local.get $out_cap))))
    "#;

    /// Гость зовёт `call_tool` с легитимно неотрицательными, но
    /// выходящими за пределы его единственной страницы памяти (64 КиБ)
    /// `tool_ptr`/`tool_len` — проверяет, что out-of-bounds указатель
    /// отклоняется как порча ДО попытки прочитать память (граница —
    /// `crates/berimor-executors/src/codeact.rs::read_guest_utf8`, не
    /// после аллокации буфера произвольного размера).
    const WAT_CALL_TOOL_OUT_OF_BOUNDS_PTR: &str = r#"
        (module
          (import "env" "call_tool"
            (func $call_tool (param i32 i32 i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 600) "{}")
          (func (export "run") (param $input_len i32) (param $out_cap i32) (result i32)
            (call $call_tool
              (i32.const 65530) (i32.const 100)
              (i32.const 600) (i32.const 2)
              (i32.const 4096) (local.get $out_cap))))
    "#;

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
        dispatch: impl ToolDispatch + 'static,
        gate: impl CapabilityGate + 'static,
    ) -> WasmHost {
        WasmHost::new(
            Arc::new(dispatch),
            Arc::new(gate),
            ConfirmationMode::Smart,
            Arc::new(AutoConfirm),
        )
    }

    #[test]
    fn bare_module_echoes_static_output_without_host_functions() {
        let host = host(PanicIfCalledDispatch, AllowAll);
        let result = host
            .run(WAT_ECHO_STATIC.as_bytes(), &json!({"ignored": true}))
            .unwrap();
        assert_eq!(result, json!({"hello": "world"}));
    }

    #[test]
    fn guest_trap_surfaces_as_wasm_host_error_not_a_panic() {
        let host = host(PanicIfCalledDispatch, AllowAll);
        let err = host.run(WAT_TRAP.as_bytes(), &json!(null)).unwrap_err();
        assert!(matches!(err, WasmHostError::Trap(_)), "{err:?}");
    }

    #[test]
    fn guest_importing_wasi_fails_to_link_because_wasi_is_never_registered() {
        let host = host(PanicIfCalledDispatch, AllowAll);
        let err = host
            .run(WAT_UNRESOLVED_WASI_IMPORT.as_bytes(), &json!(null))
            .unwrap_err();
        assert!(matches!(err, WasmHostError::Instantiate(_)), "{err:?}");
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
        let result = host.run(WAT_CALL_TOOL.as_bytes(), &json!(null)).unwrap();
        assert_eq!(result, json!({"ok": true, "value": {"echoed": true}}));
    }

    #[test]
    fn capability_deny_blocks_call_tool_before_dispatch_is_ever_called() {
        let host = host(PanicIfCalledDispatch, DenyAll);
        let result = host.run(WAT_CALL_TOOL.as_bytes(), &json!(null)).unwrap();
        assert_eq!(result["ok"], json!(false));
        assert!(result["error"]
            .as_str()
            .unwrap()
            .contains("заблокировано тестом"));
    }

    #[test]
    fn dispatch_failure_is_recoverable_and_still_returns_an_envelope() {
        let host = host(AlwaysFailsDispatch, AllowAll);
        let result = host.run(WAT_CALL_TOOL.as_bytes(), &json!(null)).unwrap();
        assert_eq!(result["ok"], json!(false));
        assert!(result["error"]
            .as_str()
            .unwrap()
            .contains("намеренный сбой теста"));
    }

    #[test]
    fn truncated_result_is_detected_not_read_as_garbage() {
        let host = host(PanicIfCalledDispatch, AllowAll);
        let err = host
            .run(WAT_OVERSIZED_RESULT.as_bytes(), &json!(null))
            .unwrap_err();
        match err {
            WasmHostError::Truncated { needed, .. } => assert_eq!(needed, 999_999),
            other => panic!("ожидался Truncated, получено {other:?}"),
        }
    }

    #[test]
    fn call_tool_negative_length_is_rejected_as_sentinel_not_dispatched() {
        let host = host(PanicIfCalledDispatch, AllowAll);
        let err = host
            .run(WAT_CALL_TOOL_NEGATIVE_LEN.as_bytes(), &json!(null))
            .unwrap_err();
        assert!(matches!(err, WasmHostError::Trap(_)), "{err:?}");
    }

    #[test]
    fn call_tool_out_of_bounds_pointer_is_rejected_before_dispatch() {
        let host = host(PanicIfCalledDispatch, AllowAll);
        let err = host
            .run(WAT_CALL_TOOL_OUT_OF_BOUNDS_PTR.as_bytes(), &json!(null))
            .unwrap_err();
        assert!(matches!(err, WasmHostError::Trap(_)), "{err:?}");
    }
}

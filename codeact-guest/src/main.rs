//! Гостевой WASM-модуль CodeAct: исполняет проверенную статическим
//! анализом (E7) JS-программу через QuickJS (`rquickjs`) внутри
//! песочницы Wasmtime (`berimor-executors::codeact::wasm_host`, E6/E8).
//!
//! НЕ член основного workspace — собирается отдельно
//! (`cargo build --release --target wasm32-wasip1`), результат
//! коммитится как `crates/berimor-executors/assets/codeact-guest.wasm`
//! (см. README.md рядом).
//!
//! Протокол ввода/вывода — через WASI stdin/stdout (host настраивает
//! `MemoryInputPipe`/`MemoryOutputPipe` и вызывает `_start`), НЕ через
//! пользовательские смещения линейной памяти, которыми пользовались
//! тестовые WAT-фикстуры E6/E7 — та часть ABI была явно провизорной
//! («не обещание совместимости», `wasm_host.rs`) именно для такого
//! пересмотра. Единственное, что осталось от прежнего протокола —
//! host-функция `env.call_tool`, вызываемая СИНХРОННО в середине
//! исполнения (stdio для этого не годится: программа зовёт стаб
//! инструмента в произвольной точке, а не только в начале/конце).
//!
//! Вход (stdin): `{"program": "<JS-текст>", "input": <JSON>}`. Внутри
//! программы доступны: глобальная `input` (то самое JSON-значение),
//! `call_tool(name, args) -> {ok, value|error}` (стаб инструмента —
//! конверт успеха/отказа, а не исключение: сбой инструмента
//! восстановим для программы, тот же принцип, что у `AgentStep`, E9),
//! `finish(result)` — единственный выход (`executors.md` §4.2): его
//! аргумент становится результатом всего прогона.
//!
//! Выход: при успехе (`finish` вызван) — JSON-значение аргумента
//! `finish` печатается в stdout, код возврата 0. При отказе (сбой
//! разбора входа, необработанное исключение JS, `finish` ни разу не
//! вызван) — сообщение в stderr, код возврата 1; хост различает эти
//! случаи по коду выхода `_start` (`wasmtime_wasi::I32Exit` или трап),
//! не по содержимому stdout.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::rc::Rc;

use rquickjs::{Context, Function, Runtime};

/// 64 МиБ — потолок кучи QuickJS. Второй, дополнительный рубеж поверх
/// собственно wasmtime-лимита памяти линейной памяти (`WasmLimits`,
/// `wasm_host.rs`) — оба защищают от одного и того же класса
/// исчерпания, но на разных уровнях (движок JS vs хост WASM).
const QUICKJS_MEMORY_LIMIT_BYTES: usize = 64 * 1024 * 1024;
const QUICKJS_MAX_STACK_SIZE_BYTES: usize = 1024 * 1024;

/// Ёмкость буфера под ответ `call_tool` — тот же класс ограничения, что
/// был у тестовых WAT-фикстур E6 (там — константа хоста; здесь —
/// константа гостя, обе стороны договариваются о разумном верхнем
/// пределе размера ОДНОГО ответа инструмента, не всей программы).
const CALL_TOOL_RESPONSE_CAP: usize = 256 * 1024;

#[link(wasm_import_module = "env")]
extern "C" {
    fn call_tool(
        tool_ptr: i32,
        tool_len: i32,
        args_ptr: i32,
        args_len: i32,
        out_ptr: i32,
        out_cap: i32,
    ) -> i32;
}

/// Маршалит один вызов стаба инструмента через WASM-импорт
/// `env.call_tool` — тот же ptr/len протокол, что host ожидает
/// (`wasm_host.rs::host_call_tool`): указатели — в СОБСТВЕННУЮ память
/// этого модуля (мы вызывающая сторона, адреса выбираем сами).
/// Усечение по возвращённой длине трактуется как отказ хоста, не как
/// повод читать частично записанный буфер.
fn host_call_tool(name: &str, args: &serde_json::Value) -> serde_json::Value {
    let tool_bytes = name.as_bytes();
    let args_bytes = serde_json::to_vec(args).unwrap_or_else(|_| b"null".to_vec());
    let mut out_buf = vec![0u8; CALL_TOOL_RESPONSE_CAP];

    let written = unsafe {
        call_tool(
            tool_bytes.as_ptr() as i32,
            tool_bytes.len() as i32,
            args_bytes.as_ptr() as i32,
            args_bytes.len() as i32,
            out_buf.as_mut_ptr() as i32,
            out_buf.len() as i32,
        )
    };

    if written < 0 || written as usize > out_buf.len() {
        return serde_json::json!({
            "ok": false,
            "error": "хост отклонил вызов инструмента (испорченный указатель или лимит вызовов исчерпан)"
        });
    }
    out_buf.truncate(written as usize);
    serde_json::from_slice(&out_buf).unwrap_or_else(
        |_| serde_json::json!({"ok": false, "error": "хост вернул невалидный JSON"}),
    )
}

/// Функция-элемент (не замыкание) — `Value<'js>` инвариантен по `'js`
/// (rquickjs); когда один и тот же `'js` нужен ОДНОВРЕМЕННО в позиции
/// параметра И в позиции возврата, компилятор не выводит для замыкания
/// нужную универсальную квантификацию `for<'js> Fn(Value<'js>) ->
/// Result<Value<'js>>` — а для обобщённого элемента функции выводит
/// корректно. `finish` (ниже) этой проблемы не имеет — там `Value`
/// только в параметре, возврат `()`.
fn call_tool_js<'js>(
    name: String,
    args: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let ctx = args.ctx().clone();
    let args_text = ctx
        .json_stringify(args)?
        .and_then(|s| s.to_string().ok())
        .unwrap_or_else(|| "null".to_string());
    let args_json: serde_json::Value =
        serde_json::from_str(&args_text).unwrap_or(serde_json::Value::Null);
    let response = host_call_tool(&name, &args_json);
    let response_text = serde_json::to_string(&response).unwrap_or_else(|_| "null".to_string());
    ctx.json_parse(response_text)
}

fn run(program: &str, input: &serde_json::Value) -> Result<serde_json::Value, String> {
    let runtime = Runtime::new().map_err(|e| format!("не удалось создать Runtime QuickJS: {e}"))?;
    runtime.set_memory_limit(QUICKJS_MEMORY_LIMIT_BYTES);
    runtime.set_max_stack_size(QUICKJS_MAX_STACK_SIZE_BYTES);

    let context =
        Context::full(&runtime).map_err(|e| format!("не удалось создать Context QuickJS: {e}"))?;

    let finished: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let input_text = serde_json::to_string(input).map_err(|e| e.to_string())?;

    let outcome: Result<(), String> = context.with(|ctx| {
        let globals = ctx.globals();

        let input_value = ctx
            .json_parse(input_text)
            .map_err(|e| format!("не удалось разобрать 'input': {e}"))?;
        globals
            .set("input", input_value)
            .map_err(|e| e.to_string())?;

        {
            let finished = Rc::clone(&finished);
            // Ctx достаётся из `value.ctx()`, а не отдельным параметром
            // замыкания — с ДВУМЯ независимо параметризованными по
            // `'js` параметрами (`Ctx<'js>` и `Value<'js>`) компилятор
            // не выводит, что оба должны разделять одно и то же `'js`
            // (`Ctx`/`Value` инвариантны, rquickjs); `Value::ctx()`
            // возвращает `&Ctx<'js>` С ТЕМ ЖЕ `'js`, что и сам `value`,
            // по построению — это снимает проблему без unsafe и без
            // отдельного захваченного клона `ctx` с чужим временем жизни.
            let finish_fn = Function::new(ctx.clone(), move |value: rquickjs::Value| {
                let text = value
                    .ctx()
                    .json_stringify(value.clone())
                    .ok()
                    .flatten()
                    .and_then(|s| s.to_string().ok());
                *finished.borrow_mut() = text;
            })
            .map_err(|e| e.to_string())?;
            globals
                .set("finish", finish_fn)
                .map_err(|e| e.to_string())?;
        }

        {
            let call_tool_fn =
                Function::new(ctx.clone(), call_tool_js).map_err(|e| e.to_string())?;
            globals
                .set("call_tool", call_tool_fn)
                .map_err(|e| e.to_string())?;
        }

        ctx.eval::<(), _>(program.as_bytes()).map_err(|e| {
            // `ctx.catch()` достаёт сам объект исключения — без него
            // сообщение об ошибке из `rquickjs::Error::Exception` не
            // содержит текста самого JS-исключения (только факт, что
            // оно произошло), а именно текст нужен модели для retry.
            let detail = ctx
                .catch()
                .as_exception()
                .and_then(|exc| exc.message())
                .unwrap_or_else(|| e.to_string());
            format!("необработанное исключение JS: {detail}")
        })?;

        Ok(())
    });

    outcome?;

    let taken = finished.borrow_mut().take();
    match taken {
        Some(text) => serde_json::from_str(&text)
            .map_err(|e| format!("finish(...) получил невалидный для JSON аргумент: {e}")),
        None => Err("программа завершилась, не вызвав finish(...)".to_string()),
    }
}

fn main() {
    let mut raw_input = String::new();
    if let Err(err) = std::io::stdin().read_to_string(&mut raw_input) {
        die(&format!("не удалось прочитать stdin: {err}"));
    }

    let envelope: serde_json::Value = match serde_json::from_str(&raw_input) {
        Ok(value) => value,
        Err(err) => die(&format!("невалидный JSON входа: {err}")),
    };

    let program = match envelope.get("program").and_then(|v| v.as_str()) {
        Some(text) => text,
        None => die("вход не содержит строкового поля 'program'"),
    };
    let input = envelope
        .get("input")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    match run(program, &input) {
        Ok(result) => {
            let text = serde_json::to_string(&result).unwrap_or_else(|_| "null".to_string());
            print!("{text}");
            let _ = std::io::stdout().flush();
            std::process::exit(0);
        }
        Err(message) => die(&message),
    }
}

fn die(message: &str) -> ! {
    eprint!("{message}");
    let _ = std::io::stderr().flush();
    std::process::exit(1);
}

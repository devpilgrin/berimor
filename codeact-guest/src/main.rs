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
    // Техдолг TD3.2 (`docs/audit-2026-07-31.md`): раньше `finish` только
    // писала значение и ВОЗВРАЩАЛА управление программе — код после
    // `finish(...)` всё равно исполнялся (побочные эффекты через
    // `call_tool`), а исключение ПОСЛЕ `finish` уничтожало уже записанный
    // результат (весь прогон падал в `GuestFailed`). `finish` — заявленный
    // единственный выход (`executors.md` §4.2), обязан реально
    // останавливать исполнение. Флаг ниже отличает «наше собственное
    // останавливающее исключение из finish» от настоящего сбоя программы —
    // без него пришлось бы сравнивать текст исключения (хрупко).
    let finished_called: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
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
            let finished_called = Rc::clone(&finished_called);
            // Ctx достаётся из `value.ctx()`, а не отдельным параметром
            // замыкания — с ДВУМЯ независимо параметризованными по
            // `'js` параметрами (`Ctx<'js>` и `Value<'js>`) компилятор
            // не выводит, что оба должны разделять одно и то же `'js`
            // (`Ctx`/`Value` инвариантны, rquickjs); `Value::ctx()`
            // возвращает `&Ctx<'js>` С ТЕМ ЖЕ `'js`, что и сам `value`,
            // по построению — это снимает проблему без unsafe и без
            // отдельного захваченного клона `ctx` с чужим временем жизни.
            //
            // Возврат `rquickjs::Result<()>` — не `()` — намеренно: это
            // и есть механизм остановки (TD3.2). `rquickjs::IntoJs`
            // реализован для `Result<T, E>` блок-импломом крейта, `Err`
            // из замыкания пробрасывается как настоящее JS-исключение
            // через `Ctx::throw`, а не молча теряется — `ctx.throw(...)`
            // сам вызывает `JS_Throw` и возвращает `Error::Exception`,
            // ЭТО и есть корректный способ прервать `ctx.eval(...)`
            // изнутри Rust-функции у rquickjs (не паника, не `std::process::exit`
            // — оба не размотали бы стек QuickJS корректно).
            let finish_fn = Function::new(
                ctx.clone(),
                move |value: rquickjs::Value| -> rquickjs::Result<()> {
                    let ctx = value.ctx().clone();
                    // Находка 3.7 аудита: сериализация огромного значения
                    // выжигала fuel ДО любого капа (Trap с бэктрейсом
                    // вместо ошибки). Длина JS-строки читается дёшево —
                    // отсекаем гигантов ДО json_stringify. Составные
                    // значения сериализуются под fuel как раньше, а хост
                    // отдаёт модели чистую ошибку без дампа (wasm_host).
                    const MAX_RESULT_CHARS: usize = 1024 * 1024;
                    if let Some(s) = value.clone().into_string() {
                        let text = s.to_string().unwrap_or_default();
                        if text.len() > MAX_RESULT_CHARS {
                            *finished_called.borrow_mut() = true;
                            let js_message = rquickjs::String::from_str(
                                ctx.clone(),
                                &format!(
                                    "результат превышает {MAX_RESULT_CHARS} байт ({}): уменьшите объём возвращаемых данных",
                                    text.len()
                                ),
                            )
                            .map_err(|e| rquickjs::Error::from(e))?;
                            return Err(ctx.throw(js_message.into_value()));
                        }
                    }
                    let text = ctx
                        .json_stringify(value.clone())
                        .ok()
                        .flatten()
                        .and_then(|s| s.to_string().ok());
                    *finished.borrow_mut() = text;
                    *finished_called.borrow_mut() = true;
                    Err(ctx.throw(value))
                },
            )
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

        let eval_result = ctx.eval::<(), _>(program.as_bytes());
        if let Err(e) = eval_result {
            if *finished_called.borrow() {
                // Независимое ревью (закрытие техдолга TD3.2) нашло
                // major-дефект в исходной версии этого фикса: булев флаг
                // `finished_called` сам по себе не отличает «это НАШЕ
                // завершающее исключение из finish, дошедшее до верха
                // необработанным» от «finish был вызван и ПЕРЕХВАЧЕН
                // try/catch, а ПОЗЖЕ произошла совершенно несвязанная
                // необработанная ошибка» — второй случай раньше молча
                // трактовался как успех со СТАРЫМ записанным значением,
                // маскируя реальный сбой скрипта. Различаем: реально
                // ПРОПАГИРУЮЩЕЕ сейчас исключение (`ctx.catch()`)
                // обязано быть буквально тем же значением, что записал
                // `finish` — сравниваем через JSON-сериализацию (тот же
                // канал, каким `finish_fn` сохраняет `finished`).
                let propagating = ctx.catch();
                let propagating_text = ctx
                    .json_stringify(propagating.clone())
                    .ok()
                    .flatten()
                    .and_then(|s| s.to_string().ok());
                if propagating_text.as_deref() == finished.borrow().as_deref() {
                    // Действительно наше собственное исключение из
                    // finish — легитимная остановка, не сбой.
                    return Ok(());
                }
                let detail = propagating
                    .as_exception()
                    .and_then(|exc| exc.message())
                    .or(propagating_text)
                    .unwrap_or_else(|| e.to_string());
                return Err(format!(
                    "необработанное исключение JS после finish(...) (не само служебное завершение, finish был перехвачен раньше): {detail}"
                ));
            }
            // `ctx.catch()` достаёт сам объект исключения — без него
            // сообщение об ошибке из `rquickjs::Error::Exception` не
            // содержит текста самого JS-исключения (только факт, что
            // оно произошло), а именно текст нужен модели для retry.
            let detail = ctx
                .catch()
                .as_exception()
                .and_then(|exc| exc.message())
                .unwrap_or_else(|| e.to_string());
            return Err(format!("необработанное исключение JS: {detail}"));
        }

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
            // Находка 3.7 аудита: результат >1 МиБ ронял гостя паникой в
            // stdio::_print (panic=abort) — хост получал нечитаемый Trap
            // с wasm-бэктрейсом, уезжавший в retry-фидбек модели. Кап с
            // ЧИТАЕМОЙ ошибкой (чистый exit 1, не Trap) + запись без
            // паникующих макросов.
            const MAX_RESULT_BYTES: usize = 1024 * 1024;
            if text.len() > MAX_RESULT_BYTES {
                die(&format!(
                    "результат превышает {} байт ({}): уменьшите объём возвращаемых данных",
                    MAX_RESULT_BYTES,
                    text.len()
                ));
            }
            let mut stdout = std::io::stdout().lock();
            if stdout.write_all(text.as_bytes()).is_err() || stdout.flush().is_err() {
                die("запись результата в stdout не удалась");
            }
            std::process::exit(0);
        }
        Err(message) => die(&message),
    }
}

fn die(message: &str) -> ! {
    let mut stderr = std::io::stderr().lock();
    let _ = stderr.write_all(message.as_bytes());
    let _ = stderr.flush();
    std::process::exit(1);
}

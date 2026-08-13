//! Инструмент `web.search` — поисковая выдача DuckDuckGo (html-эндпоинт)
//! (контракт A4 спецификации `docs/rnd/builtin-tools-waves-spec.md`).
//!
//! GET `https://html.duckduckgo.com/html/?q=<urlencoded(query)>` через
//! reqwest blocking — тот же паттерн клиента, что ветка `http.fetch` в
//! `builtin_dispatch.rs`: rustls, без редиректов, таймаут 10 с, кап тела
//! 512 КиБ, UA `berimor/<version>`. Перед запросом — сетевой гейт
//! [`net_gate::check_host`] (защита в глубину: гейт видит только query,
//! хост конструируется внутри модуля). Парсинг результата ВРУЧНУЮ (без
//! html-зависимостей): блоки `class="result__a"` → заголовок/url,
//! `class="result__snippet"` → сниппет; теги срезаются, entities
//! раскодируются, DDG-redirect `//duckduckgo.com/l/?uddg=...`
//! распаковывается до целевого URL.
//!
//! Гейт — только в публичной точке [`call`]; сетевая часть
//! [`search_with_base`] принимает базовый endpoint параметром, чтобы
//! тесты ходили в мок `TcpListener` на 127.0.0.1 (приватный адрес гейтом
//! запрещён — это и есть обход гейта для тестов по контракту).
//! Инструмент ничего не изменяет (mutates: false).

use berimor_capability::net_gate;
use berimor_executors::tool_only::DispatchError;
use serde_json::{json, Value};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use crate::builtin_dispatch::err_str;

/// Имя инструмента — для DispatchError и doc-ссылок.
const TOOL: &str = "web.search";
/// Значение поля `engine` ответа (контракт A4).
const ENGINE: &str = "duckduckgo";
/// Базовый endpoint боевого поиска (путь `/html/` добавляется внутри).
const DEFAULT_BASE: &str = "https://html.duckduckgo.com";
/// Лимит результатов по умолчанию (контракт A4).
const DEFAULT_LIMIT: u64 = 10;
/// Потолок лимита результатов (контракт A4).
const MAX_LIMIT: u64 = 25;
/// Таймаут HTTP-запроса (образец — `http.fetch`).
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Кап тела ответа — защита памяти процесса (контракт A4: 512 КиБ).
const BODY_CAP: u64 = 512 * 1024;

/// Точка входа инструмента (родитель регистрирует ветку в
/// `BuiltinToolDispatch::call` — spec, секция «Клей родителя»).
///
/// Args: `{query: string, limit?: number (default 10, cap 25)}`.
/// Ответ: `{results: [{title, url, snippet}], engine: "duckduckgo"}`.
/// `root` не используется: инструмент не трогает файловую систему,
/// параметр — часть единой сигнатуры волн A/B/C.
/// allow(dead_code) — до интеграции родителем (ветка в builtin_dispatch),
/// по образцу builtin_todo; убрать с первым потребителем.
pub fn call(_root: &Path, args: &Value) -> Result<Value, DispatchError> {
    let query = args["query"]
        .as_str()
        .ok_or_else(|| err_str(TOOL, "аргумент 'query' обязателен (строка)"))?;
    if query.trim().is_empty() {
        return Err(err_str(TOOL, "аргумент 'query' пуст"));
    }
    let limit = args["limit"]
        .as_u64()
        .unwrap_or(DEFAULT_LIMIT)
        .min(MAX_LIMIT) as usize;
    // Сетевой гейт (S3) — тот же, что у http.fetch: приватные/локальные
    // адреса запрещены. Гейт видит только фиксированный хост эндпоинта —
    // query передаётся внутри URL после проверки.
    let decision = net_gate::check_host("html.duckduckgo.com", 443);
    if !decision.is_allowed() {
        return Err(err_str(
            TOOL,
            "сетевой гейт: html.duckduckgo.com:443 — адрес вне разрешённых сетей",
        ));
    }
    search_with_base(DEFAULT_BASE, query, limit)
}

/// Сетевая часть поиска с инъекцией endpoint (контракт A4): в бою base —
/// [`DEFAULT_BASE`], в тестах — мок `http://127.0.0.1:<port>`. Гейта
/// здесь НЕТ: он обязан сработать до сети в публичной [`call`], а тесты
/// идут в локальный мок без него.
fn search_with_base(base: &str, query: &str, limit: usize) -> Result<Value, DispatchError> {
    let url = format!("{base}/html/?q={}", url_encode(query));
    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        // Редиректы запрещены: цель редиректа не проходила бы гейт
        // (обход одной проверкой) — как у http.fetch.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("berimor/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| err_str(TOOL, format!("http-клиент: {e}")))?;
    let response = client
        .get(&url)
        .send()
        .map_err(|e| err_str(TOOL, format!("запрос: {e}")))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(err_str(
            TOOL,
            format!("duckduckgo ответил статусом {status}"),
        ));
    }
    let mut buf = Vec::new();
    response
        .take(BODY_CAP + 1)
        .read_to_end(&mut buf)
        .map_err(|e| err_str(TOOL, format!("чтение тела: {e}")))?;
    // Хвост сверх капа отбрасывается: парсер работает с префиксом
    // документа, усечённый последний блок просто не распознается.
    buf.truncate(BODY_CAP as usize);
    let body = String::from_utf8_lossy(&buf);
    Ok(json!({
        "results": parse_results(&body, limit),
        "engine": ENGINE,
    }))
}

/// Разбор html-выдачи: пары `result__a` (заголовок+url) и
/// `result__snippet` (сниппет) сопоставляются по порядку следования в
/// документе — так устроен html-эндпоинт DDG.
fn parse_results(html: &str, limit: usize) -> Vec<Value> {
    let anchors = scan_blocks(html, "result__a");
    let snippets = scan_blocks(html, "result__snippet");
    anchors
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(i, (href, title))| {
            json!({
                "title": title,
                "url": unpack_url(&href),
                "snippet": snippets.get(i).map(|(_, text)| text.clone()).unwrap_or_default(),
            })
        })
        .collect()
}

/// Блоки выдачи по css-классу открывающего тега: `(href, чистый текст
/// содержимого)` в порядке следования. Ручной сканер тегов (без
/// html-зависимости, контракт A4): комментарии и закрывающие теги
/// пропускаются, класс сравнивается по токенам (class="a result__a b").
fn scan_blocks(html: &str, class: &str) -> Vec<(String, String)> {
    let mut blocks = Vec::new();
    let mut cursor = 0;
    while let Some(rel) = html[cursor..].find('<') {
        let start = cursor + rel;
        let rest = &html[start..];
        if rest.starts_with("<!--") {
            match rest.find("-->") {
                Some(end) => cursor = start + end + 3,
                None => break,
            }
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let tag = &rest[..=gt];
        cursor = start + gt + 1;
        if tag.starts_with("</") || tag.starts_with("<!") || !tag_has_class(tag, class) {
            continue;
        }
        let close = format!("</{}>", tag_name(tag));
        let href = extract_attr(tag, "href").unwrap_or_default();
        if let Some(close_rel) = html[cursor..].find(&close) {
            blocks.push((href, clean_text(&html[cursor..cursor + close_rel])));
            cursor += close_rel + close.len();
        }
    }
    blocks
}

/// Имя тега из открывающего тега (`<a class=...>` → `a`).
fn tag_name(tag: &str) -> String {
    tag[1..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// Проверка css-класса по токенам атрибута class.
fn tag_has_class(tag: &str, class: &str) -> bool {
    extract_attr(tag, "class")
        .map(|value| value.split_whitespace().any(|token| token == class))
        .unwrap_or(false)
}

/// Значение атрибута тега (`href="..."` / `href='...'`); имя атрибута
/// ищется на границе слова, чтобы `data-href` не совпал с `href`.
fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let mut search_from = 0;
    while let Some(rel) = tag[search_from..].find(name) {
        let pos = search_from + rel;
        let boundary = pos == 0 || tag.as_bytes()[pos - 1].is_ascii_whitespace();
        let after = tag[pos + name.len()..].trim_start();
        if boundary && after.starts_with('=') {
            let value = after[1..].trim_start();
            let quote = value.chars().next()?;
            if quote == '"' || quote == '\'' {
                let end = value[1..].find(quote)?;
                return Some(value[1..1 + end].to_string());
            }
        }
        search_from = pos + name.len();
    }
    None
}

/// Чистый текст блока: теги срезаны, entities раскодированы, пробелы
/// схлопнуты до одного (DDG разбивает сниппеты переводами строк).
fn clean_text(html: &str) -> String {
    let decoded = decode_entities(&strip_tags(html));
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Срезка разметки: всё между `<` и `>` отбрасывается.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Раскодирование entities выдачи (контракт A4: `&amp;`/`&quot;`/`&#x27;`/
/// `&lt;`/`&gt;`, плюс `&#39;` и `&nbsp;` как синонимы). Однопроходный
/// сканер: уже раскодированный текст повторно не декодируется
/// (`&amp;lt;` → `&lt;`, а не `<`).
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos..];
        let decoded = if let Some(r) = after.strip_prefix("&amp;") {
            Some(("&", r))
        } else if let Some(r) = after.strip_prefix("&lt;") {
            Some(("<", r))
        } else if let Some(r) = after.strip_prefix("&gt;") {
            Some((">", r))
        } else if let Some(r) = after.strip_prefix("&quot;") {
            Some(("\"", r))
        } else if let Some(r) = after
            .strip_prefix("&#x27;")
            .or_else(|| after.strip_prefix("&#X27;"))
            .or_else(|| after.strip_prefix("&#39;"))
        {
            Some(("'", r))
        } else {
            after.strip_prefix("&nbsp;").map(|r| (" ", r))
        };
        match decoded {
            Some((s, r)) => {
                out.push_str(s);
                rest = r;
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Целевой URL результата: DDG-redirect
/// `//duckduckgo.com/l/?uddg=<urlencoded>` распаковывается (контракт
/// A4), прямая ссылка возвращается с раскодированными entities
/// (`&amp;` в query).
fn unpack_url(raw_href: &str) -> String {
    let href = decode_entities(raw_href);
    if (href.contains("duckduckgo.com/l?") || href.contains("duckduckgo.com/l/"))
        && href.contains("uddg=")
    {
        let after = &href[href.find("uddg=").unwrap() + 5..];
        let end = after.find('&').unwrap_or(after.len());
        return percent_decode(&after[..end]);
    }
    href
}

/// Percent-decode (`%XX` → байт, `+` → пробел) для параметра uddg.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                // XL-ревью 2026-08-13 HIGH #1: срез `&text[i+1..i+3]`
                // падал на границе UTF-8 («%aп» — сетевой ввод, воркер
                // умирал паникой). Парсим ТОЛЬКО из байтов: не-ASCII
                // пара — не hex, '%' уходит литералом.
                let parsed = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                match parsed {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode запроса для query-параметра: не резервированные
/// символы (RFC 3986 unreserved) остаются, остальное — `%XX`.
fn url_encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // XL-ревью 2026-08-13 HIGH #1: «%a<кириллица>» не должна паниковать.
    #[test]
    fn percent_decode_survives_utf8_boundary_after_hex_prefix() {
        let decoded = percent_decode("%aп");
        assert!(decoded.starts_with('%'), "{decoded}");
        assert_eq!(percent_decode("%41%42"), "AB");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    /// Путь golden-фикстуры относительно корня workspace
    /// (crate = crates/berimor-cli) — как у builtin_edit.
    fn fixture() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/golden/tools/web.search/ddg_sample.html");
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn parses_golden_fixture_three_results() {
        let results = parse_results(&fixture(), MAX_LIMIT as usize);
        assert_eq!(results.len(), 3);
        assert_eq!(
            results[0]["title"],
            "Learn Rust — Rust Programming Language"
        );
        assert_eq!(results[0]["url"], "https://www.rust-lang.org/learn");
        assert!(results[0]["snippet"]
            .as_str()
            .unwrap()
            .contains("документация, книга и курсы"));
    }

    #[test]
    fn fixture_uddg_redirect_is_unpacked() {
        let results = parse_results(&fixture(), MAX_LIMIT as usize);
        // uddg=<urlencoded> + entity &amp; в href → чистый целевой URL.
        assert_eq!(
            results[1]["url"],
            "https://berimor.dev/docs?lang=ru&tab=tools"
        );
        assert_eq!(results[1]["title"], "Berimor — документация проекта");
    }

    #[test]
    fn fixture_tags_stripped_and_entities_decoded() {
        let results = parse_results(&fixture(), MAX_LIMIT as usize);
        // <b>язык</b> внутри заголовка срезается без потери текста.
        assert_eq!(results[2]["title"], "Rust — язык программирования");
        assert_eq!(
            results[2]["snippet"],
            "Rust & Cargo — \"системный\" язык: <быстро> и 'надёжно', без сборщика мусора."
        );
    }

    #[test]
    fn empty_page_yields_empty_results() {
        let results = parse_results("<html><body><p>ничего не найдено</p></body></html>", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn limit_caps_result_count() {
        let results = parse_results(&fixture(), 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["url"], "https://www.rust-lang.org/learn");
    }

    /// Мок HTTP-сервера на 127.0.0.1:0 (контракт: только
    /// std::net::TcpListener): принимает один запрос, отдаёт заданный
    /// status/тело, возвращает захваченный запрос для проверок.
    fn mock_server(status: u16, body: String) -> (String, Arc<Mutex<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(String::new()));
        let captured_in_thread = Arc::clone(&captured);
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            *captured_in_thread.lock().unwrap() = String::from_utf8_lossy(&request).to_string();
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        (format!("http://{addr}"), captured)
    }

    #[test]
    fn search_with_base_against_mock_server() {
        let (base, captured) = mock_server(200, fixture());
        let result = search_with_base(&base, "rust язык", 10).unwrap();
        assert_eq!(result["engine"], "duckduckgo");
        assert_eq!(result["results"].as_array().unwrap().len(), 3);
        // Запрос ушёл на /html/ с urlencoded query (кириллица → %XX).
        let request = captured.lock().unwrap().clone();
        assert!(request.starts_with("GET /html/?q="), "запрос: {request}");
        assert!(request.contains("rust%20"), "запрос: {request}");
    }

    #[test]
    fn mock_server_error_status_is_dispatch_error() {
        let (base, _captured) = mock_server(500, "ошибка".to_string());
        let result = search_with_base(&base, "rust", 10);
        let err = result.unwrap_err();
        assert_eq!(err.tool, TOOL);
        assert!(err.reason.contains("500"), "причина: {}", err.reason);
    }

    #[test]
    fn uddg_helper_cases() {
        assert_eq!(
            unpack_url("//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.b%2Fc%3Fx%3D1&amp;rut=zz"),
            "https://a.b/c?x=1"
        );
        assert_eq!(
            unpack_url("https://a.b/c?x=1&amp;y=2"),
            "https://a.b/c?x=1&y=2"
        );
        assert_eq!(
            url_encode("привет мир"),
            "%D0%BF%D1%80%D0%B8%D0%B2%D0%B5%D1%82%20%D0%BC%D0%B8%D1%80"
        );
    }
}

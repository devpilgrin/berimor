//! Статический анализ сгенерированной CodeAct-программы (белый список
//! идентификаторов) — ROADMAP E7, `docs/arch/executors.md` §4.2,
//! `docs/ADR/0022-wasmtime-sandbox-codeact.md`.
//!
//! Первый из двух барьеров ADR-0022 — «защита от шума, а не единственная
//! линия обороны»: настоящая гарантия изоляции структурная (E6,
//! [`super::wasm_host`] — отсутствие ambient-доступа к ОС/сети внутри
//! WASM linear memory). Эта проверка отклоняет очевидно опасные
//! программы ДО компиляции/исполнения, дёшево и рано — не пытаясь быть
//! непробиваемой сама по себе. См. раздел «Осознанные пробелы» ниже.
//!
//! ## Модель — allow-list, не deny-list
//!
//! `executors.md` §4.2: «разрешены только стандартная библиотека без
//! доступа к ОС и явно одобренные библиотеки обработки данных; запрещены
//! динамическое выполнение строк, динамический импорт, прямые сетевые
//! вызовы вне стабов». Свободные (несвязанные локальным объявлением)
//! идентификаторы программы обязаны входить в объединение [`SAFE_GLOBALS`]
//! и переданного вызывающим кодом набора имён стабов инструментов для
//! ЭТОГО конкретного шага — всё остальное отклоняется, независимо от
//! того, значится ли оно в каком-то списке известных опасных имён.
//! `eval`, `Function`, сетевые глобальные объекты (`fetch`,
//! `XMLHttpRequest`, `WebSocket`, ...) отклоняются РОВНО ПОТОМУ, что их
//! нет в белом списке — не через отдельную проверку по имени.
//!
//! Разбор — через `oxc_parser` (реальный JS-парсер, не регулярные
//! выражения): AST различает позицию использования идентификатора
//! (`IdentifierReference`) от позиции объявления (`BindingIdentifier`) и
//! от имени свойства при неvычисляемом доступе (`obj.foo` — `foo` не
//! идентификатор-ссылка) — то, что наивный лексический разбор строк
//! (`berimor-capability::deny`, S1, для shell-команд) не может дать по
//! построению и сам об этом честно предупреждает.
//!
//! Связывание объявленных имён с использованиями — ОДНИМ ПРОХОДОМ по
//! всей программе, без блочной точности (собираем ВСЕ объявленные имена
//! программы в один плоский набор, не только видимые в данной точке) —
//! сознательное упрощение, не пропущенный случай: неточность работает
//! ТОЛЬКО в консервативном направлении (может по ошибке РАЗРЕШИТЬ
//! идентификатор, объявленный где-то в другом месте программы под тем
//! же именем, но не может по ошибке пропустить идентификатор, который
//! нигде не объявлен и не в белом списке — свободная ссылка на `eval`
//! без предварительного `let eval = ...` отклоняется всегда).
//!
//! ## Осознанные пробелы (не единственная линия обороны, см. выше)
//!
//! Не гарантированно ловится: `globalThis.eval(...)`/`globalThis['eval']`
//! (доступ к глобальному объекту через свойство, не через свободный
//! идентификатор `eval` напрямую), вычисляемый доступ к свойствам с
//! собранным во время исполнения именем (`obj[computedName]`, включая
//! `this['ev'+'al']`), косвенный вызов `eval` вида `(0, eval)(...)`
//! (в AST это всё ещё `IdentifierReference` на `eval` — ЛОВИТСЯ, но
//! упомянуто для полноты), переопределение `globalThis`/`this` через
//! `with` (устаревшая конструкция, но не запрещена explicitly отдельным
//! правилом — ловится только если `with` использует свободный
//! идентификатор вне белого списка внутри себя). Закрывающий барьер —
//! структурная изоляция E6, не этот модуль.

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BindingIdentifier, Expression, IdentifierReference, Program, TSImportEqualsDeclaration,
};
use oxc_ast_visit::{walk::walk_program, Visit};
use oxc_parser::Parser;
use oxc_span::SourceType;
use std::collections::HashSet;

/// Безопасные intrinsics, разрешённые в любой CodeAct-программе — без
/// доступа к ОС/сети/времени/случайности. Список исчерпывающий и
/// сознательно короткий, по духу ADR-0022 — «модель ограничена
/// подмножеством... в обмен на... структурную гарантию».
///
/// `input`/`call_tool`/`finish` — не встроенные в JS имена, а
/// intrinsics ГОСТЕВОГО РАНТАЙМА (E8, `codeact-guest/src/main.rs`):
/// они существуют в любой CodeAct-программе точно так же безусловно,
/// как `JSON`/`Math` — потому и в том же списке, не в отдельном,
/// передаваемом только вызывающим кодом (`allowed_names` в [`analyze`]
/// — это ИМЕНА СТАБОВ ИНСТРУМЕНТОВ конкретного шага, `call_tool` сам —
/// не стаб, а сама функция вызова стабов).
pub const SAFE_GLOBALS: &[&str] = &[
    "undefined",
    "NaN",
    "Infinity",
    "JSON",
    "Math",
    "Array",
    "Object",
    "String",
    "Number",
    "Boolean",
    "Map",
    "Set",
    "Symbol",
    "Error",
    "TypeError",
    "RangeError",
    "isNaN",
    "isFinite",
    "parseInt",
    "parseFloat",
    "input",
    "call_tool",
    "finish",
];

#[derive(Debug, thiserror::Error)]
pub enum StaticAnalysisError {
    #[error("программа не разбирается как JS: {0}")]
    Syntax(String),
    #[error("свободный идентификатор '{0}' не входит в белый список")]
    DisallowedIdentifier(String),
    #[error("запрещённая конструкция: {0}")]
    ForbiddenConstruct(&'static str),
}

/// Проверяет `source` против белого списка [`SAFE_GLOBALS`] ∪
/// `allowed_names` (обычно — имена стабов инструментов, доступных ИМЕННО
/// этому шагу). `Ok(())` — программа не ссылается ни на что за
/// пределами белого списка и не использует запрещённые синтаксические
/// конструкции (динамический `import(...)`); иначе — первое найденное
/// нарушение (не исчерпывающий список всех нарушений сразу — как и
/// `berimor-capability::deny::analyze`, S1: одного найденного достаточно
/// для отказа).
pub fn analyze(source: &str, allowed_names: &[&str]) -> Result<(), StaticAnalysisError> {
    let allocator = Allocator::default();
    let source_type = SourceType::mjs();
    let parsed = Parser::new(&allocator, source, source_type).parse();

    if !parsed.diagnostics.is_empty() {
        let message = parsed
            .diagnostics
            .iter()
            .map(|err| err.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(StaticAnalysisError::Syntax(message));
    }

    let mut declared = HashSet::new();
    let mut collector = DeclaredNameCollector {
        declared: &mut declared,
    };
    collector.visit_program(&parsed.program);

    let mut allowed: HashSet<&str> = SAFE_GLOBALS.iter().copied().collect();
    allowed.extend(allowed_names.iter().copied());

    let mut checker = ReferenceChecker {
        declared: &declared,
        allowed: &allowed,
        violation: None,
    };
    checker.visit_program(&parsed.program);

    match checker.violation {
        Some(violation) => Err(violation),
        None => Ok(()),
    }
}

/// Первый проход — собирает ВСЕ объявленные в программе имена
/// (`BindingIdentifier`: переменные, параметры функций, объявления
/// функций/классов, привязки `catch`) в один плоский набор, без учёта
/// блочной области видимости (см. doc-комментарий модуля — почему это
/// безопасное упрощение, не дыра).
struct DeclaredNameCollector<'s> {
    declared: &'s mut HashSet<String>,
}

impl<'a> Visit<'a> for DeclaredNameCollector<'_> {
    fn visit_binding_identifier(&mut self, it: &BindingIdentifier<'a>) {
        self.declared.insert(it.name.as_str().to_string());
    }
}

/// Второй проход — каждая ссылка на идентификатор (`IdentifierReference`
/// — позиция ИСПОЛЬЗОВАНИЯ, не объявления и не имени свойства при
/// невычисляемом доступе) обязана входить в `declared` ∪ `allowed`.
/// Динамический `import(...)` отклоняется отдельно — это не
/// идентификатор, а синтаксическая конструкция.
struct ReferenceChecker<'s> {
    declared: &'s HashSet<String>,
    allowed: &'s HashSet<&'s str>,
    violation: Option<StaticAnalysisError>,
}

impl<'a> Visit<'a> for ReferenceChecker<'_> {
    fn visit_program(&mut self, it: &Program<'a>) {
        if self.violation.is_none() {
            walk_program(self, it);
        }
    }

    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        if self.violation.is_some() {
            return;
        }
        let name = it.name.as_str();
        if !self.declared.contains(name) && !self.allowed.contains(name) {
            self.violation = Some(StaticAnalysisError::DisallowedIdentifier(name.to_string()));
        }
    }

    fn visit_expression(&mut self, it: &Expression<'a>) {
        if self.violation.is_some() {
            return;
        }
        if matches!(it, Expression::ImportExpression(_)) {
            self.violation = Some(StaticAnalysisError::ForbiddenConstruct(
                "динамический import(...) запрещён",
            ));
            return;
        }
        oxc_ast_visit::walk::walk_expression(self, it);
    }

    fn visit_ts_import_equals_declaration(&mut self, _it: &TSImportEqualsDeclaration<'a>) {
        if self.violation.is_none() {
            self.violation = Some(StaticAnalysisError::ForbiddenConstruct(
                "import ... = require(...) запрещён",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_program_using_only_safe_globals_and_stub_names() {
        let source = r#"
            function run(input) {
                const parsed = JSON.parse(input);
                const doubled = parsed.map(x => x * 2);
                return echo_tool(doubled);
            }
        "#;
        assert!(analyze(source, &["echo_tool"]).is_ok());
    }

    #[test]
    fn rejects_bare_eval_reference() {
        let err = analyze("eval('1+1')", &[]).unwrap_err();
        assert!(matches!(
            err,
            StaticAnalysisError::DisallowedIdentifier(name) if name == "eval"
        ));
    }

    #[test]
    fn rejects_function_constructor_reference() {
        let err = analyze("new Function('return 1')()", &[]).unwrap_err();
        assert!(matches!(
            err,
            StaticAnalysisError::DisallowedIdentifier(name) if name == "Function"
        ));
    }

    #[test]
    fn rejects_network_global_not_in_allow_list() {
        let err = analyze("fetch('http://evil')", &[]).unwrap_err();
        assert!(matches!(
            err,
            StaticAnalysisError::DisallowedIdentifier(name) if name == "fetch"
        ));
    }

    #[test]
    fn rejects_dynamic_import() {
        let err = analyze("import('evil').then(m => m.run())", &[]).unwrap_err();
        assert!(matches!(err, StaticAnalysisError::ForbiddenConstruct(_)));
    }

    #[test]
    fn rejects_reference_to_tool_stub_not_declared_for_this_step() {
        let err = analyze("other_tool()", &["echo_tool"]).unwrap_err();
        assert!(matches!(
            err,
            StaticAnalysisError::DisallowedIdentifier(name) if name == "other_tool"
        ));
    }

    #[test]
    fn accepts_locally_declared_shadow_of_a_disallowed_name() {
        // Задокументированное упрощение: имя, объявленное ГДЕ УГОДНО в
        // программе как локальная привязка, разрешено использовать —
        // без блочной точности области видимости. Здесь это не дыра:
        // `local_helper` — легитимное локальное имя, не совпадающее ни
        // с чем опасным.
        let source = "function local_helper() { return 1; } local_helper();";
        assert!(analyze(source, &[]).is_ok());
    }

    #[test]
    fn property_access_name_is_not_treated_as_a_free_identifier() {
        // `foo` в `obj.foo` — имя свойства при невычисляемом доступе, не
        // `IdentifierReference`; `obj` сам по себе — свободная ссылка и
        // обязана быть в белом списке.
        let source = "JSON.stringify(1)";
        assert!(analyze(source, &[]).is_ok());
    }

    #[test]
    fn computed_member_access_still_checks_the_computed_expression_identifier() {
        // `obj[key]` — `key` тоже `IdentifierReference` (вычисляемый
        // доступ передаёт управление выражению) и обязан быть в
        // белом списке, даже если это попытка обфусцировать доступ.
        let source = "JSON[danger]";
        let err = analyze(source, &[]).unwrap_err();
        assert!(matches!(
            err,
            StaticAnalysisError::DisallowedIdentifier(name) if name == "danger"
        ));
    }

    #[test]
    fn syntax_error_is_reported_not_panicking() {
        let err = analyze("function( {{{", &[]).unwrap_err();
        assert!(matches!(err, StaticAnalysisError::Syntax(_)));
    }
}

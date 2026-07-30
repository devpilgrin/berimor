//! Процедурная память: навыки как документы, описание всегда/тело по требованию.
//!
//! Источник: `docs/arch/memory-model.md` §1, §3, §7. ROADMAP: MEM6.
//!
//! Формат файла — YAML-фронтматтер (метаданные + описание) между двумя
//! строками `---`, тело (markdown-инструкции) ниже. Тот же приём, что и
//! у процесса (P1, `serde_norway`): YAML для структурированной части,
//! свободный текст — для инструкций, которые синтаксис YAML только
//! испортил бы.
//!
//! «Описание всегда в доступе, тело — по требованию» (§3) — не поведение
//! рантайма (кэш/лень), а разделение на уровне типов: [`parse_summary`]
//! возвращает [`SkillSummary`], у которого попросту нет поля с телом —
//! вызывающий код (будущий слой Skills Context Engine, Фаза 3) не может
//! случайно утащить тело туда, где предполагалось только описание.
//! [`parse_full`] — то же самое плюс тело, когда навык понадобился целиком.
//!
//! «Версионируется... меняется только через событие и подтверждение
//! человека» (§7): версия — обязательное поле (её отсутствие — ошибка
//! разбора, не молчаливый `0`), но САМ запрет менять файл в обход
//! подтверждения — не то, что может обеспечить парсер; это процесс,
//! которым файлы навыков редактируются вне работающей системы.

use serde::Deserialize;

const DELIMITER: &str = "---";

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct SkillFrontmatter {
    name: String,
    /// Обязателен (§7: «версионируется») — навык без версии неотличим от
    /// случайно отредактированного файла.
    version: u32,
    /// Единственное, что грузится «всегда в доступе» (§3) — обязано быть
    /// достаточным, чтобы решить, нужен ли навык целиком.
    description: String,
}

/// Навык без тела — то, что реально «всегда в доступе» (§3).
#[derive(Debug, Clone, PartialEq)]
pub struct SkillSummary {
    pub name: String,
    pub version: u32,
    pub description: String,
}

/// Навык целиком — по требованию (§3).
#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    pub name: String,
    pub version: u32,
    pub description: String,
    pub body: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillParseError {
    #[error("файл навыка обязан начинаться со строки `---`")]
    MissingFrontmatterDelimiter,
    #[error("фронтматтер не закрыт второй строкой `---`")]
    UnterminatedFrontmatter,
    #[error("фронтматтер навыка не разобран: {0}")]
    Frontmatter(#[from] serde_norway::Error),
}

/// Разбирает только фронтматтер — тело не включается в результат.
pub fn parse_summary(raw: &str) -> Result<SkillSummary, SkillParseError> {
    let (frontmatter_text, _body) = split_frontmatter(raw)?;
    let frontmatter: SkillFrontmatter = serde_norway::from_str(frontmatter_text)?;
    Ok(SkillSummary {
        name: frontmatter.name,
        version: frontmatter.version,
        description: frontmatter.description,
    })
}

/// Разбирает навык целиком, включая тело.
pub fn parse_full(raw: &str) -> Result<Skill, SkillParseError> {
    let (frontmatter_text, body) = split_frontmatter(raw)?;
    let frontmatter: SkillFrontmatter = serde_norway::from_str(frontmatter_text)?;
    Ok(Skill {
        name: frontmatter.name,
        version: frontmatter.version,
        description: frontmatter.description,
        body: body.to_string(),
    })
}

/// Находит фронтматтер между первой строкой `---` и следующей строкой,
/// состоящей ровно из `---` — без обращения к содержимому между ними
/// (YAML внутри может само содержать `-` в начале строк списков, важно
/// не спутать их с закрывающим разделителем: разделитель — это ЦЕЛАЯ
/// строка `---`, ровно три символа, не префикс).
fn split_frontmatter(raw: &str) -> Result<(&str, &str), SkillParseError> {
    let mut lines = raw.split_inclusive('\n');

    let first = lines.next().unwrap_or("");
    if first.trim_end_matches(['\n', '\r']) != DELIMITER {
        return Err(SkillParseError::MissingFrontmatterDelimiter);
    }

    let mut offset = first.len();
    for segment in lines {
        let line = segment.trim_end_matches(['\n', '\r']);
        if line == DELIMITER {
            let frontmatter = &raw[first.len()..offset];
            let body = &raw[offset + segment.len()..];
            return Ok((frontmatter, body));
        }
        offset += segment.len();
    }
    Err(SkillParseError::UnterminatedFrontmatter)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WELL_FORMED: &str = "---\nname: card-status-lookup\nversion: 3\ndescription: Как проверить статус доставки карты клиента через CRM.\n---\n# Инструкция\n\n1. Вызови `crm.get_card_status` с id клиента.\n2. Сформулируй ответ по контракту SupportReply.\n";

    #[test]
    fn parse_full_extracts_frontmatter_and_body() {
        let skill = parse_full(WELL_FORMED).unwrap();
        assert_eq!(skill.name, "card-status-lookup");
        assert_eq!(skill.version, 3);
        assert_eq!(
            skill.description,
            "Как проверить статус доставки карты клиента через CRM."
        );
        assert!(skill.body.contains("crm.get_card_status"));
        assert!(skill.body.starts_with("# Инструкция"));
    }

    #[test]
    fn parse_summary_matches_parse_full_metadata_without_body() {
        let summary = parse_summary(WELL_FORMED).unwrap();
        let full = parse_full(WELL_FORMED).unwrap();
        assert_eq!(summary.name, full.name);
        assert_eq!(summary.version, full.version);
        assert_eq!(summary.description, full.description);
    }

    #[test]
    fn missing_opening_delimiter_is_an_error() {
        let raw = "name: x\nversion: 1\ndescription: y\n---\nтело\n";
        assert!(matches!(
            parse_summary(raw),
            Err(SkillParseError::MissingFrontmatterDelimiter)
        ));
    }

    #[test]
    fn unterminated_frontmatter_is_an_error_not_a_guess() {
        let raw = "---\nname: x\nversion: 1\ndescription: y\nтело без закрывающего разделителя\n";
        assert!(matches!(
            parse_summary(raw),
            Err(SkillParseError::UnterminatedFrontmatter)
        ));
    }

    #[test]
    fn missing_required_field_is_a_frontmatter_error() {
        // version отсутствует — «версионируется» (§7) не необязательное поле.
        let raw = "---\nname: x\ndescription: y\n---\nтело\n";
        assert!(matches!(
            parse_summary(raw),
            Err(SkillParseError::Frontmatter(_))
        ));
    }

    #[test]
    fn empty_body_after_frontmatter_is_not_an_error() {
        let raw = "---\nname: x\nversion: 1\ndescription: y\n---\n";
        let skill = parse_full(raw).unwrap();
        assert_eq!(skill.body, "");
    }

    #[test]
    fn body_preserves_multiline_content_exactly() {
        let raw = "---\nname: x\nversion: 1\ndescription: y\n---\nстрока1\nстрока2\n\nстрока4\n";
        let skill = parse_full(raw).unwrap();
        assert_eq!(skill.body, "строка1\nстрока2\n\nстрока4\n");
    }

    #[test]
    fn windows_line_endings_are_handled() {
        let raw = "---\r\nname: x\r\nversion: 1\r\ndescription: y\r\n---\r\nтело\r\n";
        let skill = parse_full(raw).unwrap();
        assert_eq!(skill.name, "x");
        assert_eq!(skill.body, "тело\r\n");
    }

    #[test]
    fn dash_prefixed_yaml_list_lines_do_not_get_mistaken_for_the_closing_delimiter() {
        // YAML-список в значении поля не должен закрыть фронтматтер раньше
        // времени: закрывающая строка — ровно `---`, не любая строка,
        // начинающаяся с `-`.
        let raw =
            "---\nname: x\nversion: 1\ndescription: |\n  - шаг один\n  - шаг два\n---\nтело\n";
        let skill = parse_full(raw).unwrap();
        assert!(skill.description.contains("шаг один"));
        assert_eq!(skill.body, "тело\n");
    }
}

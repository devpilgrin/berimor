//! Граф сущностей (опциональный слой): entity resolution, типизированные контракты узлов/рёбер.
//!
//! Источник: `docs/arch/memory-model.md` §4, ADR-0016. ROADMAP: MEM7.
//!
//! Три механизма ADR-0016 дословно:
//! 1. **Entity resolution** — [`resolve_node`]: точный идентификатор домена
//!    → явный псевдоним (`alias_of`) → близкое совпадение по эмбеддингу
//!    (только ПРЕДЛОЖЕНИЕ кандидата, не автослияние — «риск слияния двух
//!    разных сущностей с похожими текстовыми описаниями», альтернатива
//!    ADR-0016) → новый узел.
//! 2. **Согласованность типов** — [`validate_node`]/[`validate_edge`]:
//!    тип узла/ребра — контракт (обязательные поля, допустимые пары
//!    связей); несоответствие — отказ, не свободнотипированная запись.
//! 3. **Конфликтующие рёбра** — [`detect_edge_conflict`]: та же логика,
//!    что `semantic::detect_conflict` (MEM5), перенесённая на граф —
//!    тот же узел-источник и тип ребра, но другая цель, значит одно и то
//!    же отношение утверждает две разные вещи.
//!
//! Сознательная граница scope (честно, для ревью): этот модуль — ЛОГИКА,
//! не персистентность. Как `semantic::dedup`/`resolve` (MEM3) работали со
//! срезом уже загруженных фактов ДО того, как MEM4 добавил персистентность
//! в `berimor-storage`, этот модуль работает со срезом уже загруженных
//! узлов/рёбер. Хранение графа в SQLite — естественное продолжение (по
//! аналогии с MEM4), но задача ROADMAP MEM7 («узлы/рёбра, entity
//! resolution, типизированные контракты») этого не называет явно, в
//! отличие от MEM4, где «sqlite-vec» — часть названия задачи; отдельная
//! персистентность графа сюда не входит.
//!
//! Типы узлов/рёбер здесь — НЕ конкретные Rust-структуры уровня
//! `ClassificationOut` (M1): домены, которым нужен граф сущностей, у
//! этого ядра не известны заранее (в отличие от двух контрактов golden-
//! процесса). Свойства узла/ребра — `serde_json::Value` (JSON-объект),
//! схема — декларация обязательных полей и допустимых пар связей;
//! проверка — «все обязательные поля присутствуют», не полная JSON
//! Schema с ограничениями типов и диапазонов, как у `Contract` (M1).
//! Честная, а не притворная строгость: ядро не может проверить то, чего
//! оно не знает про конкретный домен.

use serde_json::Value;

/// Идентификатор узла.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeId(pub String);

/// Идентификатор ребра.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EdgeId(pub String);

/// Схема типа узла — контракт (ADR-0016: «версионируемый контракт
/// Mediation, объявляющий обязательные поля»).
#[derive(Debug, Clone, Copy)]
pub struct NodeTypeSchema {
    pub name: &'static str,
    pub schema_version: u32,
    /// Поле-идентификатор домена (например `"batch_number"`) — если тип
    /// его объявляет, entity resolution проверяет точное совпадение по
    /// нему в первую очередь (§4: «по точному идентификатору домена»).
    /// `None` — у типа нет доменного идентификатора, точное совпадение
    /// entity resolution для него недоступно (только близкое по эмбеддингу).
    pub identifier_field: Option<&'static str>,
    pub required_fields: &'static [&'static str],
}

/// Схема типа ребра — те же требования ADR-0016, что и у узла, плюс
/// допустимые пары типов узлов, которые ребро может связывать.
#[derive(Debug, Clone, Copy)]
pub struct EdgeTypeSchema {
    pub name: &'static str,
    pub schema_version: u32,
    /// `(тип узла-источника, тип узла-цели)` — связь вне объявленных пар
    /// отклоняется (§4: «допустимые типы связей между парами типов узлов»).
    pub allowed_pairs: &'static [(&'static str, &'static str)],
}

/// Зарезервированный тип ребра для явных псевдонимов (§4: «явная таблица
/// псевдонимов (`alias_of` — типизированное ребро)»). Ребро связывает
/// узел-альтернативное-представление (источник) с каноническим узлом
/// (цель) — то же направление, что и у любого другого типа ребра, не
/// особый случай на уровне хранения.
pub const ALIAS_OF_EDGE_TYPE: &str = "alias_of";

/// Кандидат на новый узел — ещё не прошёл entity resolution.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeCandidate {
    pub node_type: String,
    pub properties: Value,
}

/// Узел, уже находящийся в графе.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredNode {
    pub id: NodeId,
    pub node_type: String,
    pub properties: Value,
}

/// Кандидат на новое ребро — ещё не прошёл проверку типов/конфликтов.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeCandidate {
    pub edge_type: String,
    pub source: NodeId,
    pub target: NodeId,
    pub properties: Value,
}

/// Ребро, уже находящееся в графе.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEdge {
    pub id: EdgeId,
    pub edge_type: String,
    pub source: NodeId,
    pub target: NodeId,
    pub properties: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeError {
    /// Кандидат объявляет тип, для которого нет схемы в реестре
    /// вызывающего кода — ядро не угадывает форму незнакомого типа.
    UnknownType(String),
    /// Свойства кандидата — не JSON-объект (значит проверка обязательных
    /// полей в принципе невозможна).
    PropertiesNotAnObject,
    MissingRequiredField {
        node_type: String,
        field: String,
    },
    /// Пара типов узлов, которую пытается связать ребро, не входит в
    /// `allowed_pairs` его схемы.
    DisallowedNodePair {
        edge_type: String,
        source_type: String,
        target_type: String,
    },
}

/// Проверяет кандидата на узел против его схемы (ADR-0016: «объявляет
/// обязательные поля»). Схема передаётся вызывающим кодом (реестр типов
/// — его ответственность, как `contract_registry()` в `structured_llm.rs`,
/// E2), не выводится этим модулем.
pub fn validate_node(candidate: &NodeCandidate, schema: &NodeTypeSchema) -> Result<(), TypeError> {
    if candidate.node_type != schema.name {
        return Err(TypeError::UnknownType(candidate.node_type.clone()));
    }
    let object = candidate
        .properties
        .as_object()
        .ok_or(TypeError::PropertiesNotAnObject)?;
    for field in schema.required_fields {
        if !object.contains_key(*field) {
            return Err(TypeError::MissingRequiredField {
                node_type: schema.name.to_string(),
                field: (*field).to_string(),
            });
        }
    }
    Ok(())
}

/// Проверяет кандидата на ребро против его схемы: обязательные поля (как
/// у узла) плюс допустимость пары типов узлов, которые оно связывает.
/// `source_type`/`target_type` — типы узлов на концах ребра, уже
/// известные вызывающему коду (он же разрешил `source`/`target` до
/// реальных узлов через `resolve_node`, значит их тип уже известен).
pub fn validate_edge(
    candidate: &EdgeCandidate,
    schema: &EdgeTypeSchema,
    source_type: &str,
    target_type: &str,
) -> Result<(), TypeError> {
    if candidate.edge_type != schema.name {
        return Err(TypeError::UnknownType(candidate.edge_type.clone()));
    }
    if !schema
        .allowed_pairs
        .iter()
        .any(|(s, t)| *s == source_type && *t == target_type)
    {
        return Err(TypeError::DisallowedNodePair {
            edge_type: schema.name.to_string(),
            source_type: source_type.to_string(),
            target_type: target_type.to_string(),
        });
    }
    Ok(())
}

/// Близость кандидата на узел к уже существующему — тот же принцип
/// границы, что `semantic::SimilaritySource` (MEM3): entity resolution
/// не знает и не должно знать, как считается близость (эмбеддинги —
/// MEM4-подобная интеграция за пределами этого модуля).
pub trait NodeSimilaritySource {
    /// Значение в `[0.0, 1.0]`; `1.0` — совпадающая сущность.
    fn similarity(&self, candidate: &NodeCandidate, existing: &StoredNode) -> f32;
}

/// Похожести не существует — для вызывающего кода без эмбеддингов;
/// entity resolution тогда работает только через точный
/// идентификатор/псевдоним (строго лучше, чем совсем без resolution).
pub struct NoNodeSimilarity;

impl NodeSimilaritySource for NoNodeSimilarity {
    fn similarity(&self, _candidate: &NodeCandidate, _existing: &StoredNode) -> f32 {
        0.0
    }
}

/// Итог entity resolution (ADR-0016, порядок буквально):
/// точный идентификатор → псевдоним → близкое совпадение (ПРЕДЛОЖЕНИЕ,
/// не слияние) → новый узел.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolutionOutcome {
    /// Точное совпадение доменного идентификатора — та же сущность,
    /// автоматически (единственный случай автослияния без подтверждения,
    /// ADR-0016: «Автоматическое слияние без подтверждения — только при
    /// точном совпадении идентификатора»).
    ExactMatch(NodeId),
    /// Кандидат совпал с узлом, который сам — источник `alias_of` на
    /// канонический узел; возвращается КАНОНИЧЕСКИЙ узел, не узел-псевдоним.
    AliasMatch(NodeId),
    /// Близкое совпадение по эмбеддингу выше порога — только предложение
    /// человеку (конфликт-событие, как при консолидации фактов, §4):
    /// решение о слиянии эта функция не принимает, только предлагает
    /// кандидата на рассмотрение.
    AmbiguousCandidate {
        suggested: NodeId,
        similarity: f32,
    },
    New,
}

/// Разрешает кандидата на узел относительно уже существующих узлов и
/// известных псевдонимов — дословный порядок ADR-0016.
pub fn resolve_node(
    candidate: &NodeCandidate,
    schema: &NodeTypeSchema,
    existing: &[StoredNode],
    edges: &[StoredEdge],
    similarity: &dyn NodeSimilaritySource,
    threshold: f32,
) -> ResolutionOutcome {
    if let Some(id_field) = schema.identifier_field {
        if let Some(candidate_id) = candidate.properties.get(id_field) {
            if let Some(matched) = existing.iter().find(|n| {
                n.node_type == candidate.node_type
                    && n.properties.get(id_field) == Some(candidate_id)
            }) {
                // Найденный узел сам может быть источником alias_of —
                // тогда кандидат разрешается до КАНОНИЧЕСКОЙ цели, не до
                // узла-псевдонима, на который он совпал буквально по
                // идентификатору (иначе повторная подача того же
                // альтернативного идентификатора каждый раз возвращала бы
                // узел-псевдоним вместо канонической сущности).
                if let Some(canonical) = edges
                    .iter()
                    .find(|e| e.edge_type == ALIAS_OF_EDGE_TYPE && e.source == matched.id)
                    .map(|e| e.target.clone())
                {
                    return ResolutionOutcome::AliasMatch(canonical);
                }
                return ResolutionOutcome::ExactMatch(matched.id.clone());
            }
        }
    }

    let best = existing
        .iter()
        .filter(|n| n.node_type == candidate.node_type)
        .map(|n| (n, similarity.similarity(candidate, n)))
        .filter(|(_, score)| *score >= threshold)
        .max_by(|(_, a), (_, b)| a.total_cmp(b));

    match best {
        Some((node, score)) => ResolutionOutcome::AmbiguousCandidate {
            suggested: node.id.clone(),
            similarity: score,
        },
        None => ResolutionOutcome::New,
    }
}

/// Ребро, структурно противоречащее кандидату — та же логика, что
/// `semantic::detect_conflict` (MEM5), перенесённая на граф: тот же
/// узел-источник и тип ребра, но другая цель (§4: «два источника
/// утверждают взаимоисключающие связи для одной пары узлов»
/// — на уровне отношения «источник+тип ребра», не пары «источник+цель»:
/// если бы конфликт проверялся по паре источник-цель, две одинаковые
/// связи с разными свойствами прошли бы как «разные пары», хотя это и
/// есть искомое противоречие — то же самое отношение утверждает разные
/// цели).
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeConflict {
    pub existing: EdgeId,
    pub existing_target: NodeId,
    pub candidate_target: NodeId,
}

pub fn detect_edge_conflict(
    candidate: &EdgeCandidate,
    existing_edges: &[StoredEdge],
) -> Option<EdgeConflict> {
    existing_edges.iter().find_map(|edge| {
        let same_relation =
            edge.edge_type == candidate.edge_type && edge.source == candidate.source;
        let different_target = edge.target != candidate.target;
        if same_relation && different_target {
            Some(EdgeConflict {
                existing: edge.id.clone(),
                existing_target: edge.target.clone(),
                candidate_target: candidate.target.clone(),
            })
        } else {
            None
        }
    })
}

/// Итог полного решения по кандидату на ребро: точный дубликат →
/// конфликт → новое ребро.
#[derive(Debug, Clone, PartialEq)]
pub enum EdgeResolution {
    /// Точно та же тройка источник+тип+цель с теми же свойствами — уже
    /// записано, писать нечего.
    Duplicate(EdgeId),
    Conflict(EdgeConflict),
    New,
}

/// Разрешает кандидата на ребро: точный дубликат — безусловно первым
/// (детерминирован, не зависит от того, что решит проверка конфликта);
/// иначе — конфликт (MEM5-подобная логика, см. `detect_edge_conflict`);
/// иначе — новое ребро.
pub fn resolve_edge(candidate: &EdgeCandidate, existing_edges: &[StoredEdge]) -> EdgeResolution {
    if let Some(duplicate) = existing_edges.iter().find(|e| {
        e.edge_type == candidate.edge_type
            && e.source == candidate.source
            && e.target == candidate.target
            && e.properties == candidate.properties
    }) {
        return EdgeResolution::Duplicate(duplicate.id.clone());
    }
    if let Some(conflict) = detect_edge_conflict(candidate, existing_edges) {
        return EdgeResolution::Conflict(conflict);
    }
    EdgeResolution::New
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BATCH: NodeTypeSchema = NodeTypeSchema {
        name: "batch",
        schema_version: 1,
        identifier_field: Some("batch_number"),
        required_fields: &["batch_number", "product"],
    };

    const SUPPLIER: NodeTypeSchema = NodeTypeSchema {
        name: "supplier",
        schema_version: 1,
        identifier_field: Some("supplier_id"),
        required_fields: &["supplier_id", "name"],
    };

    const SUPPLIED_BY: EdgeTypeSchema = EdgeTypeSchema {
        name: "supplied_by",
        schema_version: 1,
        allowed_pairs: &[("batch", "supplier")],
    };

    fn node(id: &str, node_type: &str, properties: Value) -> StoredNode {
        StoredNode {
            id: NodeId(id.into()),
            node_type: node_type.into(),
            properties,
        }
    }

    fn edge(
        id: &str,
        edge_type: &str,
        source: &str,
        target: &str,
        properties: Value,
    ) -> StoredEdge {
        StoredEdge {
            id: EdgeId(id.into()),
            edge_type: edge_type.into(),
            source: NodeId(source.into()),
            target: NodeId(target.into()),
            properties,
        }
    }

    // --- validate_node ---------------------------------------------------

    #[test]
    fn validate_node_accepts_all_required_fields_present() {
        let candidate = NodeCandidate {
            node_type: "batch".into(),
            properties: json!({"batch_number": "B-1", "product": "widget"}),
        };
        assert!(validate_node(&candidate, &BATCH).is_ok());
    }

    #[test]
    fn validate_node_rejects_missing_required_field() {
        let candidate = NodeCandidate {
            node_type: "batch".into(),
            properties: json!({"batch_number": "B-1"}),
        };
        assert_eq!(
            validate_node(&candidate, &BATCH),
            Err(TypeError::MissingRequiredField {
                node_type: "batch".into(),
                field: "product".into()
            })
        );
    }

    #[test]
    fn validate_node_rejects_type_mismatch_with_schema() {
        let candidate = NodeCandidate {
            node_type: "supplier".into(),
            properties: json!({"batch_number": "B-1", "product": "widget"}),
        };
        assert!(matches!(
            validate_node(&candidate, &BATCH),
            Err(TypeError::UnknownType(_))
        ));
    }

    #[test]
    fn validate_node_rejects_non_object_properties() {
        let candidate = NodeCandidate {
            node_type: "batch".into(),
            properties: json!("не объект"),
        };
        assert_eq!(
            validate_node(&candidate, &BATCH),
            Err(TypeError::PropertiesNotAnObject)
        );
    }

    #[test]
    fn validate_node_allows_extra_fields_beyond_required() {
        // Схема объявляет ОБЯЗАТЕЛЬНЫЕ поля, не закрытый список — лишние
        // поля не запрещены этим уровнем проверки (в отличие от
        // deny_unknown_fields контрактов M1, у графа схема мягче: она не
        // знает форму домена настолько же строго).
        let candidate = NodeCandidate {
            node_type: "batch".into(),
            properties: json!({"batch_number": "B-1", "product": "widget", "note": "extra"}),
        };
        assert!(validate_node(&candidate, &BATCH).is_ok());
    }

    // --- validate_edge -----------------------------------------------------

    #[test]
    fn validate_edge_accepts_allowed_pair() {
        let candidate = EdgeCandidate {
            edge_type: "supplied_by".into(),
            source: NodeId("b-1".into()),
            target: NodeId("s-1".into()),
            properties: json!({}),
        };
        assert!(validate_edge(&candidate, &SUPPLIED_BY, "batch", "supplier").is_ok());
    }

    #[test]
    fn validate_edge_rejects_disallowed_pair() {
        let candidate = EdgeCandidate {
            edge_type: "supplied_by".into(),
            source: NodeId("s-1".into()),
            target: NodeId("s-2".into()),
            properties: json!({}),
        };
        assert_eq!(
            validate_edge(&candidate, &SUPPLIED_BY, "supplier", "supplier"),
            Err(TypeError::DisallowedNodePair {
                edge_type: "supplied_by".into(),
                source_type: "supplier".into(),
                target_type: "supplier".into(),
            })
        );
    }

    #[test]
    fn validate_edge_rejects_type_mismatch_with_schema() {
        let candidate = EdgeCandidate {
            edge_type: "alias_of".into(),
            source: NodeId("b-1".into()),
            target: NodeId("s-1".into()),
            properties: json!({}),
        };
        assert!(matches!(
            validate_edge(&candidate, &SUPPLIED_BY, "batch", "supplier"),
            Err(TypeError::UnknownType(_))
        ));
    }

    // --- resolve_node: точный идентификатор -------------------------------

    #[test]
    fn resolve_node_exact_identifier_match_regardless_of_similarity() {
        let existing = [node(
            "n-1",
            "batch",
            json!({"batch_number": "B-1", "product": "widget"}),
        )];
        let candidate = NodeCandidate {
            node_type: "batch".into(),
            properties: json!({"batch_number": "B-1", "product": "разное описание"}),
        };

        struct AlwaysZero;
        impl NodeSimilaritySource for AlwaysZero {
            fn similarity(&self, _c: &NodeCandidate, _e: &StoredNode) -> f32 {
                0.0
            }
        }

        let outcome = resolve_node(&candidate, &BATCH, &existing, &[], &AlwaysZero, 0.9);

        assert_eq!(outcome, ResolutionOutcome::ExactMatch(NodeId("n-1".into())));
    }

    #[test]
    fn resolve_node_identifier_mismatch_falls_through_to_similarity() {
        let existing = [node(
            "n-1",
            "batch",
            json!({"batch_number": "B-1", "product": "widget"}),
        )];
        let candidate = NodeCandidate {
            node_type: "batch".into(),
            properties: json!({"batch_number": "B-2", "product": "widget"}),
        };

        let outcome = resolve_node(&candidate, &BATCH, &existing, &[], &NoNodeSimilarity, 0.9);

        assert_eq!(outcome, ResolutionOutcome::New);
    }

    // --- resolve_node: псевдоним --------------------------------------------

    #[test]
    fn resolve_node_alias_match_returns_the_canonical_target_not_the_alias_node() {
        let existing = [
            node(
                "canonical",
                "batch",
                json!({"batch_number": "B-CANON", "product": "widget"}),
            ),
            node(
                "alias-node",
                "batch",
                json!({"batch_number": "B-OLD-NAME", "product": "widget"}),
            ),
        ];
        let edges = [edge(
            "e-1",
            ALIAS_OF_EDGE_TYPE,
            "alias-node",
            "canonical",
            json!({}),
        )];
        let candidate = NodeCandidate {
            node_type: "batch".into(),
            properties: json!({"batch_number": "B-OLD-NAME", "product": "widget"}),
        };

        let outcome = resolve_node(
            &candidate,
            &BATCH,
            &existing,
            &edges,
            &NoNodeSimilarity,
            0.9,
        );

        assert_eq!(
            outcome,
            ResolutionOutcome::AliasMatch(NodeId("canonical".into()))
        );
    }

    #[test]
    fn resolve_node_matching_id_without_alias_of_edge_is_not_an_alias() {
        // Совпадение идентификатора без явного alias_of-ребра — это уже
        // ExactMatch (случай выше), не AliasMatch: два пути к одному и
        // тому же исходу, не два разных случая.
        let existing = [node(
            "n-1",
            "batch",
            json!({"batch_number": "B-1", "product": "widget"}),
        )];
        let candidate = NodeCandidate {
            node_type: "batch".into(),
            properties: json!({"batch_number": "B-1", "product": "widget"}),
        };

        let outcome = resolve_node(&candidate, &BATCH, &existing, &[], &NoNodeSimilarity, 0.9);

        assert_eq!(outcome, ResolutionOutcome::ExactMatch(NodeId("n-1".into())));
    }

    // --- resolve_node: близкое совпадение (только предложение) -------------

    #[test]
    fn resolve_node_close_similarity_is_a_suggestion_not_automerge() {
        let existing = [node(
            "n-1",
            "supplier",
            json!({"supplier_id": "S-1", "name": "ООО Ромашка"}),
        )];
        // У кандидата ДРУГОЙ идентификатор — точное совпадение и
        // псевдоним недоступны, единственный сигнал — похожесть.
        let candidate = NodeCandidate {
            node_type: "supplier".into(),
            properties: json!({"supplier_id": "S-2", "name": "Ромашка ООО"}),
        };

        struct HighSimilarity;
        impl NodeSimilaritySource for HighSimilarity {
            fn similarity(&self, _c: &NodeCandidate, _e: &StoredNode) -> f32 {
                0.95
            }
        }

        let outcome = resolve_node(&candidate, &SUPPLIER, &existing, &[], &HighSimilarity, 0.9);

        assert_eq!(
            outcome,
            ResolutionOutcome::AmbiguousCandidate {
                suggested: NodeId("n-1".into()),
                similarity: 0.95
            },
            "близкое совпадение обязано быть ПРЕДЛОЖЕНИЕМ (AmbiguousCandidate), \
             не автослиянием (ExactMatch/AliasMatch) — ADR-0016"
        );
    }

    #[test]
    fn resolve_node_similarity_below_threshold_is_new() {
        let existing = [node(
            "n-1",
            "supplier",
            json!({"supplier_id": "S-1", "name": "ООО Ромашка"}),
        )];
        let candidate = NodeCandidate {
            node_type: "supplier".into(),
            properties: json!({"supplier_id": "S-2", "name": "Совсем другое"}),
        };

        struct LowSimilarity;
        impl NodeSimilaritySource for LowSimilarity {
            fn similarity(&self, _c: &NodeCandidate, _e: &StoredNode) -> f32 {
                0.3
            }
        }

        let outcome = resolve_node(&candidate, &SUPPLIER, &existing, &[], &LowSimilarity, 0.9);
        assert_eq!(outcome, ResolutionOutcome::New);
    }

    #[test]
    fn resolve_node_similarity_only_compares_within_the_same_node_type() {
        let existing = [node(
            "n-1",
            "supplier",
            json!({"supplier_id": "S-1", "name": "ООО Ромашка"}),
        )];
        let candidate = NodeCandidate {
            node_type: "batch".into(),
            properties: json!({"batch_number": "B-1", "product": "widget"}),
        };

        struct AlwaysMax;
        impl NodeSimilaritySource for AlwaysMax {
            fn similarity(&self, _c: &NodeCandidate, _e: &StoredNode) -> f32 {
                1.0
            }
        }

        // BATCH.identifier_field ("batch_number") отсутствует у supplier-узла,
        // так что точное совпадение и псевдоним недостижимы; узел другого
        // типа не должен предлагаться похожестью вовсе.
        let outcome = resolve_node(&candidate, &BATCH, &existing, &[], &AlwaysMax, 0.9);
        assert_eq!(outcome, ResolutionOutcome::New);
    }

    #[test]
    fn resolve_node_without_identifier_field_relies_only_on_similarity() {
        const UNTYPED: NodeTypeSchema = NodeTypeSchema {
            name: "note",
            schema_version: 1,
            identifier_field: None,
            required_fields: &["text"],
        };
        let existing = [node("n-1", "note", json!({"text": "старая заметка"}))];
        let candidate = NodeCandidate {
            node_type: "note".into(),
            properties: json!({"text": "новая заметка"}),
        };

        struct HighSimilarity;
        impl NodeSimilaritySource for HighSimilarity {
            fn similarity(&self, _c: &NodeCandidate, _e: &StoredNode) -> f32 {
                0.95
            }
        }

        let outcome = resolve_node(&candidate, &UNTYPED, &existing, &[], &HighSimilarity, 0.9);
        assert_eq!(
            outcome,
            ResolutionOutcome::AmbiguousCandidate {
                suggested: NodeId("n-1".into()),
                similarity: 0.95
            }
        );
    }

    // --- detect_edge_conflict / resolve_edge --------------------------------

    #[test]
    fn detect_edge_conflict_finds_same_source_and_type_with_different_target() {
        let existing = [edge(
            "e-1",
            "supplied_by",
            "batch-1",
            "supplier-a",
            json!({}),
        )];
        let candidate = EdgeCandidate {
            edge_type: "supplied_by".into(),
            source: NodeId("batch-1".into()),
            target: NodeId("supplier-b".into()),
            properties: json!({}),
        };

        let conflict = detect_edge_conflict(&candidate, &existing).unwrap();

        assert_eq!(conflict.existing, EdgeId("e-1".into()));
        assert_eq!(conflict.existing_target, NodeId("supplier-a".into()));
        assert_eq!(conflict.candidate_target, NodeId("supplier-b".into()));
    }

    #[test]
    fn detect_edge_conflict_none_when_target_matches() {
        let existing = [edge(
            "e-1",
            "supplied_by",
            "batch-1",
            "supplier-a",
            json!({}),
        )];
        let candidate = EdgeCandidate {
            edge_type: "supplied_by".into(),
            source: NodeId("batch-1".into()),
            target: NodeId("supplier-a".into()),
            properties: json!({"note": "подтверждено другим источником"}),
        };

        assert!(detect_edge_conflict(&candidate, &existing).is_none());
    }

    #[test]
    fn detect_edge_conflict_none_when_source_differs() {
        let existing = [edge(
            "e-1",
            "supplied_by",
            "batch-1",
            "supplier-a",
            json!({}),
        )];
        let candidate = EdgeCandidate {
            edge_type: "supplied_by".into(),
            source: NodeId("batch-2".into()),
            target: NodeId("supplier-b".into()),
            properties: json!({}),
        };

        assert!(detect_edge_conflict(&candidate, &existing).is_none());
    }

    #[test]
    fn detect_edge_conflict_none_when_edge_type_differs() {
        let existing = [edge(
            "e-1",
            "supplied_by",
            "batch-1",
            "supplier-a",
            json!({}),
        )];
        let candidate = EdgeCandidate {
            edge_type: "inspected_by".into(),
            source: NodeId("batch-1".into()),
            target: NodeId("supplier-b".into()),
            properties: json!({}),
        };

        assert!(detect_edge_conflict(&candidate, &existing).is_none());
    }

    #[test]
    fn resolve_edge_exact_duplicate_takes_precedence_over_conflict_check() {
        let existing = [edge(
            "e-1",
            "supplied_by",
            "batch-1",
            "supplier-a",
            json!({"since": "2026-01-01"}),
        )];
        let candidate = EdgeCandidate {
            edge_type: "supplied_by".into(),
            source: NodeId("batch-1".into()),
            target: NodeId("supplier-a".into()),
            properties: json!({"since": "2026-01-01"}),
        };

        assert_eq!(
            resolve_edge(&candidate, &existing),
            EdgeResolution::Duplicate(EdgeId("e-1".into()))
        );
    }

    #[test]
    fn resolve_edge_reports_conflict_for_same_relation_different_target() {
        let existing = [edge(
            "e-1",
            "supplied_by",
            "batch-1",
            "supplier-a",
            json!({}),
        )];
        let candidate = EdgeCandidate {
            edge_type: "supplied_by".into(),
            source: NodeId("batch-1".into()),
            target: NodeId("supplier-b".into()),
            properties: json!({}),
        };

        assert_eq!(
            resolve_edge(&candidate, &existing),
            EdgeResolution::Conflict(EdgeConflict {
                existing: EdgeId("e-1".into()),
                existing_target: NodeId("supplier-a".into()),
                candidate_target: NodeId("supplier-b".into()),
            })
        );
    }

    #[test]
    fn resolve_edge_is_new_when_nothing_matches_or_conflicts() {
        let existing = [edge(
            "e-1",
            "supplied_by",
            "batch-1",
            "supplier-a",
            json!({}),
        )];
        let candidate = EdgeCandidate {
            edge_type: "supplied_by".into(),
            source: NodeId("batch-2".into()),
            target: NodeId("supplier-a".into()),
            properties: json!({}),
        };

        assert_eq!(resolve_edge(&candidate, &existing), EdgeResolution::New);
    }
}

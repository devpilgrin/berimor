//! Преднастроенные пресеты провайдеров (§20.12) — код-данные, как
//! прайс-таблица (ADR-0011): адреса и имена переменных ключей меняются
//! релизом, не «научены» моделью. Мастер настройки предлагает их
//! интерактивно; пользователь может скорректировать `model_id`/`base_url`
//! до записи — пресет это заготовка, не догма.
//!
//! Покрываются только OpenAI-совместимые endpoint'ы — единственный
//! диалект `HttpProvider`. Нативный API Anthropic несовместим (другой
//! протокол), поэтому `claude` идёт через OpenRouter — честная пометка
//! в `about`, а не молчаливая подмена.

use crate::config::ProviderConfig;
use berimor_types::model::ModelTier;

pub struct ProviderPreset {
    /// Короткое имя — имя провайдера в конфиге и ключ в `/models add`.
    pub name: &'static str,
    /// Человекочитаемое название для меню мастера.
    pub display: &'static str,
    /// Честное пояснение (например, «через OpenRouter» для claude).
    pub about: &'static str,
    pub base_url: &'static str,
    pub default_model: &'static str,
    pub tier: ModelTier,
    /// Имя переменной ключа; `None` — ключ не нужен (локальные серверы).
    pub key_env: Option<&'static str>,
    /// Приватный endpoint (сетевой гейт S3): локальные серверы — true.
    pub private: bool,
    /// Обязательная температура, если модель её диктует (Kimi k3 — 1.0).
    pub temperature: Option<f32>,
    /// `false` — сервер не принимает `response_format: json_object`
    /// (репорт 2026-08-08: LM Studio отвечает 400 на каждый структурный
    /// запрос — весь `berimor chat` был неработоспособен с этим пресетом).
    pub json_object_response_format: bool,
}

pub const PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        // Kimi Code (подписка): ОТДЕЛЬНЫЙ endpoint от платформы
        // Moonshot — ключи несовместимы (репорт 2026-08-03: ключ Kimi
        // Code на api.moonshot.ai даёт 401 «Invalid Authentication»).
        // Актуальные модели по доке kimi.com/code/docs: k3, k3-256k,
        // kimi-for-coding[-highspeed]; живой список — /models.
        name: "kimi",
        display: "Kimi Code (подписка Kimi)",
        about: "api.kimi.com/coding — ключ с kimi.com, модели k3/kimi-for-coding",
        base_url: "https://api.kimi.com/coding/v1",
        default_model: "k3-256k",
        tier: ModelTier::Strong,
        key_env: Some("MOONSHOT_API_KEY"),
        private: false,
        // k3/kimi-for-coding: «only 1 is allowed for this model».
        temperature: Some(1.0),
        json_object_response_format: true,
    },
    ProviderPreset {
        name: "moonshot",
        display: "Moonshot AI (платформа)",
        about: "api.moonshot.ai — ключ с platform.moonshot.ai, НЕ ключ Kimi Code",
        base_url: "https://api.moonshot.ai/v1",
        default_model: "kimi-k2-0711-preview",
        tier: ModelTier::Strong,
        key_env: Some("MOONSHOT_PLATFORM_API_KEY"),
        private: false,
        temperature: None,
        json_object_response_format: true,
    },
    ProviderPreset {
        name: "deepseek",
        display: "DeepSeek",
        about: "api.deepseek.com, OpenAI-совместимый API",
        base_url: "https://api.deepseek.com",
        default_model: "deepseek-chat",
        tier: ModelTier::Strong,
        key_env: Some("DEEPSEEK_API_KEY"),
        private: false,
        temperature: None,
        json_object_response_format: true,
    },
    ProviderPreset {
        name: "openai",
        display: "OpenAI",
        about: "api.openai.com, эталонный OpenAI API",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o-mini",
        tier: ModelTier::Strong,
        key_env: Some("OPENAI_API_KEY"),
        private: false,
        temperature: None,
        json_object_response_format: true,
    },
    ProviderPreset {
        name: "claude",
        display: "Claude (через OpenRouter)",
        about: "нативный API Anthropic не OpenAI-совместим — доступ через openrouter.ai",
        base_url: "https://openrouter.ai/api/v1",
        default_model: "anthropic/claude-sonnet-4",
        tier: ModelTier::Strong,
        key_env: Some("OPENROUTER_API_KEY"),
        private: false,
        temperature: None,
        json_object_response_format: true,
    },
    ProviderPreset {
        name: "ollama",
        display: "Ollama (локальные модели)",
        about: "локальный сервер ollama serve, порт 11434",
        base_url: "http://localhost:11434/v1",
        default_model: "qwen2.5:7b-instruct",
        tier: ModelTier::Strong,
        key_env: None,
        private: true,
        temperature: None,
        json_object_response_format: true,
    },
    ProviderPreset {
        name: "llamacpp",
        display: "llama.cpp server (локальные модели)",
        about: "локальный llama-server, порт 8080",
        base_url: "http://localhost:8080/v1",
        default_model: "local",
        tier: ModelTier::Weak,
        key_env: None,
        private: true,
        temperature: None,
        json_object_response_format: true,
    },
    ProviderPreset {
        name: "lmstudio",
        display: "LM Studio (локальные модели)",
        about: "локальный сервер LM Studio, порт 1234",
        base_url: "http://localhost:1234/v1",
        default_model: "local-model",
        tier: ModelTier::Weak,
        key_env: None,
        private: true,
        temperature: None,
        // Репорт 2026-08-08: LM Studio (в отличие от ollama/llama-server)
        // отвечает 400 «response_format.type must be 'json_schema' or
        // 'text'» на `{"type": "json_object"}` — живой прогон против
        // localhost:1234 подтвердил и саму ошибку, и что без поля запрос
        // проходит 200 OK. Живой список моделей (§20.14) работал всегда —
        // это отдельный GET /models без chat/completions и без этого поля.
        json_object_response_format: false,
    },
    ProviderPreset {
        // Директива пользователя 2026-08-08: «llamaCpp, Ollama и другие
        // локальные» — расширение набора локальных OpenAI-совместимых
        // серверов инференса сверх уже имевшихся ollama/llamacpp/lmstudio.
        name: "vllm",
        display: "vLLM OpenAI-сервер (локальные модели)",
        about: "локальный vllm serve --enable-auto-tool-choice, порт 8000",
        base_url: "http://localhost:8000/v1",
        default_model: "local",
        tier: ModelTier::Strong,
        key_env: None,
        private: true,
        temperature: None,
        json_object_response_format: true,
    },
    ProviderPreset {
        name: "textgenwebui",
        display: "text-generation-webui / oobabooga (локальные модели)",
        about: "локальный сервер с расширением openai, порт 5000",
        base_url: "http://localhost:5000/v1",
        default_model: "local",
        tier: ModelTier::Weak,
        key_env: None,
        private: true,
        temperature: None,
        json_object_response_format: true,
    },
    ProviderPreset {
        name: "koboldcpp",
        display: "KoboldCpp (локальные модели)",
        about: "локальный koboldcpp, OpenAI-совместимый endpoint, порт 5001",
        base_url: "http://localhost:5001/v1",
        default_model: "local",
        tier: ModelTier::Weak,
        key_env: None,
        private: true,
        temperature: None,
        json_object_response_format: true,
    },
];

pub fn find_preset(name: &str) -> Option<&'static ProviderPreset> {
    PRESETS.iter().find(|preset| preset.name == name)
}

/// Собирает запись провайдера из пресета с возможными правками
/// пользователя (model_id/base_url спрашиваются в мастере).
pub fn instantiate(
    preset: &ProviderPreset,
    model_id: Option<String>,
    base_url: Option<String>,
) -> ProviderConfig {
    ProviderConfig {
        name: preset.name.to_string(),
        model_id: model_id.unwrap_or_else(|| preset.default_model.to_string()),
        tier: preset.tier,
        base_url: base_url.unwrap_or_else(|| preset.base_url.to_string()),
        model_path: None,
        api_key_env: preset.key_env.map(str::to_string),
        auth: None,
        oauth_profile: None,
        allow_private_endpoint: preset.private,
        cost_per_1k_tokens: None,
        temperature: preset.temperature,
        json_object_response_format: preset.json_object_response_format,
    }
}

/// TOML-блок провайдера для дописывания в файл конфигурации (ручной
/// рендер: `Config` — Deserialize-only, сериализация всего конфига
/// переписывала бы чужие правки и комментарии).
pub fn render_provider_toml(provider: &ProviderConfig) -> String {
    // tier — через serde (snake_case), НЕ Debug: `Strong` в файле не
    // распарсится обратно. toml::to_string голый enum не берёт
    // (UnsupportedType), serde_json даёт `"strong"` с кавычками —
    // валидная TOML-строка.
    let tier = serde_json::to_string(&provider.tier).expect("enum сериализуем");
    let mut block = format!(
        "\n[[providers]]\nname = {:?}\nmodel_id = {:?}\ntier = {}\nbase_url = {:?}\n",
        provider.name,
        provider.model_id,
        tier.trim(),
        provider.base_url
    );
    if let Some(key_env) = &provider.api_key_env {
        block.push_str(&format!("api_key_env = {key_env:?}\n"));
    }
    if provider.allow_private_endpoint {
        block.push_str("allow_private_endpoint = true\n");
    }
    if let Some(temperature) = provider.temperature {
        block.push_str(&format!("temperature = {temperature:?}\n"));
    }
    if !provider.json_object_response_format {
        block.push_str("json_object_response_format = false\n");
    }
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_cover_all_promised_providers() {
        let names: Vec<&str> = PRESETS.iter().map(|p| p.name).collect();
        for expected in [
            "kimi",
            "moonshot",
            "deepseek",
            "openai",
            "claude",
            "ollama",
            "llamacpp",
            "lmstudio",
            "vllm",
            "textgenwebui",
            "koboldcpp",
        ] {
            assert!(names.contains(&expected), "нет пресета {expected}");
        }
    }

    #[test]
    fn every_preset_renders_valid_toml_that_parses_back() {
        for preset in PRESETS {
            let provider = instantiate(preset, None, None);
            let toml_text = render_provider_toml(&provider);
            let parsed: crate::config::PartialConfig =
                toml::from_str(&toml_text).expect(&toml_text);
            assert_eq!(parsed.providers.len(), 1);
            assert_eq!(parsed.providers[0].name, preset.name);
        }
    }

    #[test]
    fn local_presets_are_private_and_keyless() {
        for name in [
            "ollama",
            "llamacpp",
            "lmstudio",
            "vllm",
            "textgenwebui",
            "koboldcpp",
        ] {
            let preset = find_preset(name).unwrap();
            assert!(preset.private, "{name} обязан быть приватным");
            assert!(preset.key_env.is_none(), "{name} не требует ключа");
        }
    }

    /// Репорт 2026-08-08: LM Studio отвечает 400 на
    /// `response_format: {"type": "json_object"}` — пресет обязан
    /// отключать поле, а отключение обязано пережить запись/чтение
    /// TOML (иначе `/models add` пишет провайдера, который снова ловит
    /// 400 на первом же структурном ходе).
    #[test]
    fn lmstudio_disables_json_object_response_format_and_it_round_trips_through_toml() {
        let preset = find_preset("lmstudio").unwrap();
        assert!(!preset.json_object_response_format);

        let provider = instantiate(preset, None, None);
        let toml_text = render_provider_toml(&provider);
        assert!(toml_text.contains("json_object_response_format = false"));

        let parsed: crate::config::PartialConfig = toml::from_str(&toml_text).unwrap();
        assert!(!parsed.providers[0].json_object_response_format);
    }

    /// Остальные пресеты не платят за чужую находку: поле в TOML не
    /// пишется вовсе (умолчание true и без него).
    #[test]
    fn presets_other_than_lmstudio_keep_json_object_response_format_enabled() {
        for preset in PRESETS.iter().filter(|p| p.name != "lmstudio") {
            assert!(
                preset.json_object_response_format,
                "{} неожиданно отключает json_object_response_format",
                preset.name
            );
            let toml_text = render_provider_toml(&instantiate(preset, None, None));
            assert!(
                !toml_text.contains("json_object_response_format"),
                "{}: поле не должно писаться при умолчании",
                preset.name
            );
        }
    }

    #[test]
    fn claude_preset_is_honest_about_openrouter() {
        let preset = find_preset("claude").unwrap();
        assert!(preset.base_url.contains("openrouter"));
        assert!(preset.about.contains("OpenRouter") || preset.about.contains("openrouter"));
    }
}

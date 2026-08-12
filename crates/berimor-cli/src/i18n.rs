//! Локализация интерфейса TUI (задача пользователя 2026-08-09): 8
//! локалей семейства README (ru/en/de/fr/es/zh-CN/ja/ko).
//!
//! Таблица строк — КОД, не модель и не конфиг: полнота перевода
//! гарантируется компилятором (`Strings` с обязательными полями —
//! локаль с пропущенной строкой просто не соберётся). Разрешение
//! локали: `[ui] locale` в конфиге → LANG/LC_ALL окружения → ru.
//! Динамические сообщения об ошибках и журналь событий остаются
//! русскими (диагностика, не интерфейс); локализуется «хром»: шапка,
//! подсказки, модал подтверждения, пикеры, промпты, slash-команды и
//! ответы самих slash-команд.

/// Локаль интерфейса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    Ru,
    En,
    De,
    Fr,
    Es,
    ZhCn,
    Ja,
    Ko,
}

impl Locale {
    pub const ALL: [Locale; 8] = [
        Locale::Ru,
        Locale::En,
        Locale::De,
        Locale::Fr,
        Locale::Es,
        Locale::ZhCn,
        Locale::Ja,
        Locale::Ko,
    ];

    /// Канонический код (как в именах README: en, de, fr, es, zh-CN, ja, ko).
    pub fn code(self) -> &'static str {
        match self {
            Locale::Ru => "ru",
            Locale::En => "en",
            Locale::De => "de",
            Locale::Fr => "fr",
            Locale::Es => "es",
            Locale::ZhCn => "zh-CN",
            Locale::Ja => "ja",
            Locale::Ko => "ko",
        }
    }

    /// Самоназвание — для пикера выбора локали.
    pub fn native_name(self) -> &'static str {
        match self {
            Locale::Ru => "Русский",
            Locale::En => "English",
            Locale::De => "Deutsch",
            Locale::Fr => "Français",
            Locale::Es => "Español",
            Locale::ZhCn => "简体中文",
            Locale::Ja => "日本語",
            Locale::Ko => "한국어",
        }
    }

    /// Разбор кода (регистронезависимо; "zh"/"zh-cn" тоже принимаем).
    pub fn from_code(code: &str) -> Option<Locale> {
        match code.to_lowercase().as_str() {
            "ru" => Some(Locale::Ru),
            "en" => Some(Locale::En),
            "de" => Some(Locale::De),
            "fr" => Some(Locale::Fr),
            "es" => Some(Locale::Es),
            "zh-cn" | "zh" => Some(Locale::ZhCn),
            "ja" => Some(Locale::Ja),
            "ko" => Some(Locale::Ko),
            _ => None,
        }
    }

    /// Локаль окружения (LANG/LC_ALL вида "de_DE.UTF-8"); неизвестная —
    /// ru (текущее поведение интерфейса).
    pub fn detect() -> Locale {
        for var in ["LC_ALL", "LANG"] {
            if let Ok(value) = std::env::var(var) {
                let head = value
                    .split(['.', '@'])
                    .next()
                    .unwrap_or("")
                    .replace('_', "-");
                // "de-DE" → "de", "zh-CN" → "zh-CN"
                let lang = head.split('-').next().unwrap_or("");
                let candidate = if lang.eq_ignore_ascii_case("zh") {
                    "zh".to_string()
                } else {
                    lang.to_string()
                };
                if let Some(locale) = Locale::from_code(&candidate) {
                    return locale;
                }
            }
        }
        Locale::Ru
    }

    /// Разрешение эффективной локали: значение `[ui] locale` из
    /// конфига, иначе окружение, иначе ru.
    pub fn resolve(configured: Option<&str>) -> Locale {
        configured
            .and_then(Locale::from_code)
            .unwrap_or_else(Locale::detect)
    }
}

/// Таблица строк интерфейса. Все поля обязательны — компилятор
/// гарантирует полноту каждой локали.
pub struct Strings {
    // Строка подсказок под полем ввода.
    pub hint_default: &'static str,
    pub hint_busy: &'static str,
    pub hint_confirm: &'static str,
    pub hint_picker: &'static str,
    pub hint_slash: &'static str,
    pub hint_log_focus: &'static str,
    pub hint_mouse_off: &'static str,
    // Шапка.
    pub header_workspace: &'static str,
    pub header_models: &'static str,
    pub header_models_empty: &'static str,
    pub header_thinking: &'static str,
    // Модал подтверждения capability-гейта.
    pub confirm_title: &'static str,
    pub confirm_yes: &'static str,
    pub confirm_yes_hint: &'static str,
    pub confirm_session: &'static str,
    pub confirm_session_hint: &'static str,
    pub confirm_project: &'static str,
    pub confirm_project_hint: &'static str,
    pub confirm_all: &'static str,
    pub confirm_all_hint: &'static str,
    pub confirm_no: &'static str,
    pub confirm_no_hint: &'static str,
    pub confirm_nav: &'static str,
    // Промпты потоков (ключ API, URL плагина).
    pub secret_title: &'static str,
    pub key_label: &'static str,
    pub key_hint: &'static str,
    pub plugin_title: &'static str,
    pub plugin_repo_label: &'static str,
    pub plugin_repo_hint: &'static str,
    // Заголовки пикеров.
    pub picker_presets: &'static str,
    pub picker_provider: &'static str,
    pub picker_remove: &'static str,
    pub picker_locale: &'static str,
    // Описания slash-команд (палитра и /help).
    pub slash_help: &'static str,
    pub slash_config: &'static str,
    pub slash_models: &'static str,
    pub slash_models_add: &'static str,
    pub slash_model: &'static str,
    pub slash_tools: &'static str,
    pub slash_skills: &'static str,
    pub slash_skills_add: &'static str,
    pub slash_skills_remove: &'static str,
    pub slash_agents: &'static str,
    pub slash_agents_add: &'static str,
    pub slash_agents_remove: &'static str,
    pub slash_plugins: &'static str,
    pub slash_plugins_add: &'static str,
    pub slash_plugins_remove: &'static str,
    pub slash_exit: &'static str,
    pub slash_mouse: &'static str,
    pub slash_copy: &'static str,
    // Sys-ответы slash-команд.
    pub sys_mouse_on: &'static str,
    pub sys_mouse_off: &'static str,
    pub sys_copied: &'static str,
    pub sys_nothing_to_copy: &'static str,
    pub sys_no_clipboard: &'static str,
    pub sys_providers_empty: &'static str,
    pub sys_config_journal: &'static str,
    pub sys_config_mode: &'static str,
    pub sys_config_providers: &'static str,
    pub sys_config_locale: &'static str,
    pub sys_locale_set: &'static str,
    pub sys_locale_unknown: &'static str,
}

const RU: Strings = Strings {
    hint_default: " Enter — отправить · Alt+Enter — новая строка · колесо/клик — журнал · выделение — Shift+drag · / — команды",
    hint_busy: " агент работает… · Ctrl+C — выход",
    hint_confirm: " ←→ — выбор · Enter — активировать · y/д/n/н — сразу · Esc — нет",
    hint_picker: " ↑↓ — выбор · Space — пометить · Enter — подтвердить · Esc — отмена",
    hint_slash: " ↑↓ — выбор · Tab/Enter — вставить · Esc — закрыть",
    hint_log_focus: " журнал в фокусе · ↑↓/PgUp/PgDn/колесо — прокрутка · Esc или клик по вводу — назад",
    hint_mouse_off: " мышь отпущена — выделение нативное · /mouse — вернуть колесо · /copy — ответ в буфер",
    header_workspace: " область: ",
    header_models: " модели: ",
    header_models_empty: "не настроены — /models add",
    header_thinking: " думаю…",
    confirm_title: " подтверждение действия ",
    confirm_yes: " да [y] ",
    confirm_yes_hint: "разрешить",
    confirm_session: " сессия [с] ",
    confirm_session_hint: "до конца сессии",
    confirm_project: " проект [п] ",
    confirm_project_hint: "инструмент — в .berimor/allow",
    confirm_all: " всё [в] ",
    confirm_all_hint: "ВСЁ для проекта",
    confirm_no: " нет [n] ",
    confirm_no_hint: "отказ",
    confirm_nav: "  ←→/Tab — выбор, Enter — активировать",
    secret_title: " секрет ",
    key_label: " Ключ API",
    key_hint: " Enter — сохранить · Esc — пропустить",
    plugin_title: " плагин ",
    plugin_repo_label: " URL репозитория плагина:",
    plugin_repo_hint: " Enter — установить (приостановит TUI на время установки) · Esc — отмена",
    picker_presets: "Пресеты (Space — пометить, Enter — подтвердить)",
    picker_provider: "Провайдер (Enter — выбрать)",
    picker_remove: "Удалить (Enter — подтвердить)",
    picker_locale: "Локаль интерфейса (Enter — выбрать)",
    slash_help: "список команд",
    slash_config: "эффективная конфигурация",
    slash_models: "провайдеры моделей",
    slash_models_add: "мастер: пресеты → живой список моделей → ключ",
    slash_model: "сменить модель сессии (выбор из списка провайдера)",
    slash_tools: "список доступных инструментов",
    slash_skills: "список установленных скилов",
    slash_skills_add: "установить скилл из каталога berimor-skills",
    slash_skills_remove: "удалить установленный скилл",
    slash_agents: "список установленных субагентов",
    slash_agents_add: "установить субагента из каталога berimor-agents",
    slash_agents_remove: "удалить установленного субагента",
    slash_plugins: "список установленных плагинов",
    slash_plugins_add: "установить плагин из доверенного репозитория",
    slash_plugins_remove: "удалить установленный плагин",
    slash_exit: "завершить",
    slash_mouse: "отпустить/захватить мышь (нативное выделение vs колесо)",
    slash_copy: "последний ответ агента — в буфер обмена",
    sys_mouse_on: "мышь захвачена: колесо и клик-фокус работают; выделение — Shift+drag",
    sys_mouse_off: "мышь отпущена: выделение нативное; колесо недоступно. Вернуть — /mouse",
    sys_copied: "последний ответ скопирован в буфер",
    sys_nothing_to_copy: "журнал пуст — нечего копировать",
    sys_no_clipboard: "буфер обмена недоступен: не найдено wl-copy/xclip/xsel/pbcopy",
    sys_providers_empty: "провайдеры не настроены — /models add",
    sys_config_journal: "журнал: ",
    sys_config_mode: "режим подтверждений: ",
    sys_config_providers: "провайдеров: ",
    sys_config_locale: "локаль интерфейса: ",
    sys_locale_set: "— сохранена в локальном конфиге",
    sys_locale_unknown: "неизвестная локаль — доступны: ru, en, de, fr, es, zh-CN, ja, ko",
};

const EN: Strings = Strings {
    hint_default: " Enter — send · Alt+Enter — new line · wheel/click — log · selection — Shift+drag · / — commands",
    hint_busy: " agent is working… · Ctrl+C — quit",
    hint_confirm: " ←→ — choose · Enter — activate · y/n — instant · Esc — no",
    hint_picker: " ↑↓ — choose · Space — mark · Enter — confirm · Esc — cancel",
    hint_slash: " ↑↓ — choose · Tab/Enter — insert · Esc — close",
    hint_log_focus: " log focused · ↑↓/PgUp/PgDn/wheel — scroll · Esc or click on input — back",
    hint_mouse_off: " mouse released — native selection · /mouse — restore wheel · /copy — reply to clipboard",
    header_workspace: " workspace: ",
    header_models: " models: ",
    header_models_empty: "not configured — /models add",
    header_thinking: " thinking…",
    confirm_title: " action confirmation ",
    confirm_yes: " yes [y] ",
    confirm_yes_hint: "allow once",
    confirm_session: " session [s] ",
    confirm_session_hint: "for this session",
    confirm_project: " project [p] ",
    confirm_project_hint: "tool — to .berimor/allow",
    confirm_all: " all [a] ",
    confirm_all_hint: "ALL for the project",
    confirm_no: " no [n] ",
    confirm_no_hint: "deny",
    confirm_nav: "  ←→/Tab — choose, Enter — activate",
    secret_title: " secret ",
    key_label: " API key",
    key_hint: " Enter — save · Esc — skip",
    plugin_title: " plugin ",
    plugin_repo_label: " Plugin repository URL:",
    plugin_repo_hint: " Enter — install (TUI pauses during install) · Esc — cancel",
    picker_presets: "Presets (Space — mark, Enter — confirm)",
    picker_provider: "Provider (Enter — select)",
    picker_remove: "Remove (Enter — confirm)",
    picker_locale: "Interface locale (Enter — select)",
    slash_help: "command list",
    slash_config: "effective configuration",
    slash_models: "model providers",
    slash_models_add: "wizard: presets → live model list → key",
    slash_model: "switch session model (pick from provider list)",
    slash_tools: "available tools",
    slash_skills: "installed skills",
    slash_skills_add: "install skill from berimor-skills catalog",
    slash_skills_remove: "remove installed skill",
    slash_agents: "installed subagents",
    slash_agents_add: "install subagent from berimor-agents catalog",
    slash_agents_remove: "remove installed subagent",
    slash_plugins: "installed plugins",
    slash_plugins_add: "install plugin from trusted repository",
    slash_plugins_remove: "remove installed plugin",
    slash_exit: "quit",
    slash_mouse: "release/capture mouse (native selection vs wheel)",
    slash_copy: "last agent reply — to clipboard",
    sys_mouse_on: "mouse captured: wheel and click focus work; selection — Shift+drag",
    sys_mouse_off: "mouse released: native selection; wheel unavailable. Restore — /mouse",
    sys_copied: "last reply copied to clipboard",
    sys_nothing_to_copy: "log is empty — nothing to copy",
    sys_no_clipboard: "clipboard unavailable: wl-copy/xclip/xsel/pbcopy not found",
    sys_providers_empty: "no providers configured — /models add",
    sys_config_journal: "journal: ",
    sys_config_mode: "confirmation mode: ",
    sys_config_providers: "providers: ",
    sys_config_locale: "interface locale: ",
    sys_locale_set: "— saved to local config",
    sys_locale_unknown: "unknown locale — available: ru, en, de, fr, es, zh-CN, ja, ko",
};

const DE: Strings = Strings {
    hint_default: " Enter — senden · Alt+Enter — neue Zeile · Rad/Klick — Log · Auswahl — Shift+drag · / — Befehle",
    hint_busy: " Agent arbeitet… · Ctrl+C — beenden",
    hint_confirm: " ←→ — wählen · Enter — aktivieren · y/n — sofort · Esc — nein",
    hint_picker: " ↑↓ — wählen · Space — markieren · Enter — bestätigen · Esc — abbrechen",
    hint_slash: " ↑↓ — wählen · Tab/Enter — einfügen · Esc — schließen",
    hint_log_focus: " Log fokussiert · ↑↓/PgUp/PgDn/Rad — scrollen · Esc oder Klick ins Eingabefeld — zurück",
    hint_mouse_off: " Maus freigegeben — native Auswahl · /mouse — Rad zurück · /copy — Antwort in Zwischenablage",
    header_workspace: " Bereich: ",
    header_models: " Modelle: ",
    header_models_empty: "nicht konfiguriert — /models add",
    header_thinking: " denkt nach…",
    confirm_title: " Aktionsbestätigung ",
    confirm_yes: " ja [y] ",
    confirm_yes_hint: "erlauben",
    confirm_session: " Sitzung [s] ",
    confirm_session_hint: "bis Sitzungsende",
    confirm_project: " Projekt [p] ",
    confirm_project_hint: "Werkzeug — in .berimor/allow",
    confirm_all: " alles [a] ",
    confirm_all_hint: "ALLES fürs Projekt",
    confirm_no: " nein [n] ",
    confirm_no_hint: "ablehnen",
    confirm_nav: "  ←→/Tab — wählen, Enter — aktivieren",
    secret_title: " Geheimnis ",
    key_label: " API-Schlüssel",
    key_hint: " Enter — speichern · Esc — überspringen",
    plugin_title: " Plugin ",
    plugin_repo_label: " URL des Plugin-Repositorys:",
    plugin_repo_hint: " Enter — installieren (TUI pausiert währenddessen) · Esc — abbrechen",
    picker_presets: "Presets (Space — markieren, Enter — bestätigen)",
    picker_provider: "Provider (Enter — wählen)",
    picker_remove: "Entfernen (Enter — bestätigen)",
    picker_locale: "Oberflächensprache (Enter — wählen)",
    slash_help: "Befehlsliste",
    slash_config: "effektive Konfiguration",
    slash_models: "Modell-Provider",
    slash_models_add: "Assistent: Presets → Live-Modellliste → Schlüssel",
    slash_model: "Sitzungsmodell wechseln (aus Providerliste wählen)",
    slash_tools: "verfügbare Werkzeuge",
    slash_skills: "installierte Skills",
    slash_skills_add: "Skill aus dem berimor-skills-Katalog installieren",
    slash_skills_remove: "installierten Skill entfernen",
    slash_agents: "installierte Subagenten",
    slash_agents_add: "Subagenten aus dem berimor-agents-Katalog installieren",
    slash_agents_remove: "installierten Subagenten entfernen",
    slash_plugins: "installierte Plugins",
    slash_plugins_add: "Plugin aus vertrauenswürdigem Repository installieren",
    slash_plugins_remove: "installiertes Plugin entfernen",
    slash_exit: "beenden",
    slash_mouse: "Maus freigeben/erfassen (native Auswahl vs. Rad)",
    slash_copy: "letzte Agent-Antwort — in die Zwischenablage",
    sys_mouse_on: "Maus erfasst: Rad und Klick-Fokus aktiv; Auswahl — Shift+drag",
    sys_mouse_off: "Maus freigegeben: native Auswahl; Rad nicht verfügbar. Zurück — /mouse",
    sys_copied: "letzte Antwort in Zwischenablage kopiert",
    sys_nothing_to_copy: "Log leer — nichts zu kopieren",
    sys_no_clipboard: "Zwischenablage nicht verfügbar: wl-copy/xclip/xsel/pbcopy nicht gefunden",
    sys_providers_empty: "keine Provider konfiguriert — /models add",
    sys_config_journal: "Journal: ",
    sys_config_mode: "Bestätigungsmodus: ",
    sys_config_providers: "Provider: ",
    sys_config_locale: "Oberflächensprache: ",
    sys_locale_set: "— in lokaler Konfiguration gespeichert",
    sys_locale_unknown: "unbekannte Sprache — verfügbar: ru, en, de, fr, es, zh-CN, ja, ko",
};

const FR: Strings = Strings {
    hint_default: " Enter — envoyer · Alt+Enter — nouvelle ligne · molette/clic — journal · sélection — Shift+drag · / — commandes",
    hint_busy: " l'agent travaille… · Ctrl+C — quitter",
    hint_confirm: " ←→ — choisir · Enter — activer · y/n — direct · Esc — non",
    hint_picker: " ↑↓ — choisir · Space — cocher · Enter — confirmer · Esc — annuler",
    hint_slash: " ↑↓ — choisir · Tab/Enter — insérer · Esc — fermer",
    hint_log_focus: " journal focalisé · ↑↓/PgUp/PgDn/molette — défiler · Esc ou clic sur la saisie — retour",
    hint_mouse_off: " souris libérée — sélection native · /mouse — retrouver la molette · /copy — réponse dans le presse-papiers",
    header_workspace: " espace: ",
    header_models: " modèles: ",
    header_models_empty: "non configurés — /models add",
    header_thinking: " réflexion…",
    confirm_title: " confirmation d'action ",
    confirm_yes: " oui [y] ",
    confirm_yes_hint: "autoriser",
    confirm_session: " session [s] ",
    confirm_session_hint: "jusqu'à la fin de session",
    confirm_project: " projet [p] ",
    confirm_project_hint: "outil — dans .berimor/allow",
    confirm_all: " tout [a] ",
    confirm_all_hint: "TOUT pour le projet",
    confirm_no: " non [n] ",
    confirm_no_hint: "refuser",
    confirm_nav: "  ←→/Tab — choisir, Enter — activer",
    secret_title: " secret ",
    key_label: " Clé API",
    key_hint: " Enter — enregistrer · Esc — ignorer",
    plugin_title: " plugin ",
    plugin_repo_label: " URL du dépôt du plugin :",
    plugin_repo_hint: " Enter — installer (le TUI se met en pause) · Esc — annuler",
    picker_presets: "Préréglages (Space — cocher, Enter — confirmer)",
    picker_provider: "Fournisseur (Enter — choisir)",
    picker_remove: "Supprimer (Enter — confirmer)",
    picker_locale: "Langue de l'interface (Enter — choisir)",
    slash_help: "liste des commandes",
    slash_config: "configuration effective",
    slash_models: "fournisseurs de modèles",
    slash_models_add: "assistant : préréglages → liste de modèles → clé",
    slash_model: "changer de modèle de session (choix dans le fournisseur)",
    slash_tools: "outils disponibles",
    slash_skills: "skills installés",
    slash_skills_add: "installer un skill du catalogue berimor-skills",
    slash_skills_remove: "supprimer un skill installé",
    slash_agents: "sous-agents installés",
    slash_agents_add: "installer un sous-agent du catalogue berimor-agents",
    slash_agents_remove: "supprimer un sous-agent installé",
    slash_plugins: "plugins installés",
    slash_plugins_add: "installer un plugin depuis un dépôt de confiance",
    slash_plugins_remove: "supprimer un plugin installé",
    slash_exit: "quitter",
    slash_mouse: "libérer/capturer la souris (sélection native vs molette)",
    slash_copy: "dernière réponse de l'agent — dans le presse-papiers",
    sys_mouse_on: "souris capturée : molette et focus au clic actifs ; sélection — Shift+drag",
    sys_mouse_off: "souris libérée : sélection native ; molette indisponible. Retour — /mouse",
    sys_copied: "dernière réponse copiée dans le presse-papiers",
    sys_nothing_to_copy: "journal vide — rien à copier",
    sys_no_clipboard: "presse-papiers indisponible : wl-copy/xclip/xsel/pbcopy introuvable",
    sys_providers_empty: "aucun fournisseur configuré — /models add",
    sys_config_journal: "journal : ",
    sys_config_mode: "mode de confirmation : ",
    sys_config_providers: "fournisseurs : ",
    sys_config_locale: "langue de l'interface : ",
    sys_locale_set: "— enregistrée dans la config locale",
    sys_locale_unknown: "langue inconnue — disponibles : ru, en, de, fr, es, zh-CN, ja, ko",
};

const ES: Strings = Strings {
    hint_default: " Enter — enviar · Alt+Enter — nueva línea · rueda/clic — registro · selección — Shift+drag · / — comandos",
    hint_busy: " el agente trabaja… · Ctrl+C — salir",
    hint_confirm: " ←→ — elegir · Enter — activar · y/n — directo · Esc — no",
    hint_picker: " ↑↓ — elegir · Space — marcar · Enter — confirmar · Esc — cancelar",
    hint_slash: " ↑↓ — elegir · Tab/Enter — insertar · Esc — cerrar",
    hint_log_focus: " registro enfocado · ↑↓/PgUp/PgDn/rueda — desplazar · Esc o clic en la entrada — volver",
    hint_mouse_off: " ratón liberado — selección nativa · /mouse — recuperar rueda · /copy — respuesta al portapapeles",
    header_workspace: " área: ",
    header_models: " modelos: ",
    header_models_empty: "no configurados — /models add",
    header_thinking: " pensando…",
    confirm_title: " confirmación de acción ",
    confirm_yes: " sí [y] ",
    confirm_yes_hint: "permitir",
    confirm_session: " sesión [s] ",
    confirm_session_hint: "hasta el fin de sesión",
    confirm_project: " proyecto [p] ",
    confirm_project_hint: "herramienta — a .berimor/allow",
    confirm_all: " todo [a] ",
    confirm_all_hint: "TODO para el proyecto",
    confirm_no: " no [n] ",
    confirm_no_hint: "denegar",
    confirm_nav: "  ←→/Tab — elegir, Enter — activar",
    secret_title: " secreto ",
    key_label: " Clave API",
    key_hint: " Enter — guardar · Esc — omitir",
    plugin_title: " plugin ",
    plugin_repo_label: " URL del repositorio del plugin:",
    plugin_repo_hint: " Enter — instalar (el TUI se pausa durante la instalación) · Esc — cancelar",
    picker_presets: "Presets (Space — marcar, Enter — confirmar)",
    picker_provider: "Proveedor (Enter — elegir)",
    picker_remove: "Eliminar (Enter — confirmar)",
    picker_locale: "Idioma de la interfaz (Enter — elegir)",
    slash_help: "lista de comandos",
    slash_config: "configuración efectiva",
    slash_models: "proveedores de modelos",
    slash_models_add: "asistente: presets → lista de modelos en vivo → clave",
    slash_model: "cambiar modelo de sesión (elegir de la lista del proveedor)",
    slash_tools: "herramientas disponibles",
    slash_skills: "skills instalados",
    slash_skills_add: "instalar skill del catálogo berimor-skills",
    slash_skills_remove: "eliminar skill instalado",
    slash_agents: "subagentes instalados",
    slash_agents_add: "instalar subagente del catálogo berimor-agents",
    slash_agents_remove: "eliminar subagente instalado",
    slash_plugins: "plugins instalados",
    slash_plugins_add: "instalar plugin desde un repositorio de confianza",
    slash_plugins_remove: "eliminar plugin instalado",
    slash_exit: "salir",
    slash_mouse: "liberar/capturar ratón (selección nativa vs rueda)",
    slash_copy: "última respuesta del agente — al portapapeles",
    sys_mouse_on: "ratón capturado: rueda y foco por clic activos; selección — Shift+drag",
    sys_mouse_off: "ratón liberado: selección nativa; rueda no disponible. Volver — /mouse",
    sys_copied: "última respuesta copiada al portapapeles",
    sys_nothing_to_copy: "registro vacío — nada que copiar",
    sys_no_clipboard: "portapapeles no disponible: no se encontró wl-copy/xclip/xsel/pbcopy",
    sys_providers_empty: "sin proveedores configurados — /models add",
    sys_config_journal: "registro: ",
    sys_config_mode: "modo de confirmación: ",
    sys_config_providers: "proveedores: ",
    sys_config_locale: "idioma de la interfaz: ",
    sys_locale_set: "— guardado en la configuración local",
    sys_locale_unknown: "idioma desconocido — disponibles: ru, en, de, fr, es, zh-CN, ja, ko",
};

const ZH_CN: Strings = Strings {
    hint_default:
        " Enter — 发送 · Alt+Enter — 换行 · 滚轮/点击 — 日志 · 选择 — Shift+拖动 · / — 命令",
    hint_busy: " 代理工作中… · Ctrl+C — 退出",
    hint_confirm: " ←→ — 选择 · Enter — 确认 · y/n — 快捷 · Esc — 否",
    hint_picker: " ↑↓ — 选择 · Space — 标记 · Enter — 确认 · Esc — 取消",
    hint_slash: " ↑↓ — 选择 · Tab/Enter — 插入 · Esc — 关闭",
    hint_log_focus: " 日志聚焦中 · ↑↓/PgUp/PgDn/滚轮 — 滚动 · Esc 或点击输入框 — 返回",
    hint_mouse_off: " 鼠标已释放 — 原生选择 · /mouse — 恢复滚轮 · /copy — 回复复制到剪贴板",
    header_workspace: " 目录: ",
    header_models: " 模型: ",
    header_models_empty: "未配置 — /models add",
    header_thinking: " 思考中…",
    confirm_title: " 操作确认 ",
    confirm_yes: " 是 [y] ",
    confirm_yes_hint: "允许一次",
    confirm_session: " 会话 [s] ",
    confirm_session_hint: "本次会话内有效",
    confirm_project: " 项目 [p] ",
    confirm_project_hint: "工具 — 写入 .berimor/allow",
    confirm_all: " 全部 [a] ",
    confirm_all_hint: "项目内全部允许",
    confirm_no: " 否 [n] ",
    confirm_no_hint: "拒绝",
    confirm_nav: "  ←→/Tab — 选择, Enter — 确认",
    secret_title: " 密钥 ",
    key_label: " API 密钥",
    key_hint: " Enter — 保存 · Esc — 跳过",
    plugin_title: " 插件 ",
    plugin_repo_label: " 插件仓库 URL:",
    plugin_repo_hint: " Enter — 安装(安装期间 TUI 暂停) · Esc — 取消",
    picker_presets: "预设(Space — 标记, Enter — 确认)",
    picker_provider: "提供商(Enter — 选择)",
    picker_remove: "删除(Enter — 确认)",
    picker_locale: "界面语言(Enter — 选择)",
    slash_help: "命令列表",
    slash_config: "生效配置",
    slash_models: "模型提供商",
    slash_models_add: "向导:预设 → 在线模型列表 → 密钥",
    slash_model: "切换会话模型(从提供商列表选择)",
    slash_tools: "可用工具列表",
    slash_skills: "已安装技能列表",
    slash_skills_add: "从 berimor-skills 目录安装技能",
    slash_skills_remove: "删除已安装技能",
    slash_agents: "已安装子代理列表",
    slash_agents_add: "从 berimor-agents 目录安装子代理",
    slash_agents_remove: "删除已安装子代理",
    slash_plugins: "已安装插件列表",
    slash_plugins_add: "从受信仓库安装插件",
    slash_plugins_remove: "删除已安装插件",
    slash_exit: "退出",
    slash_mouse: "释放/捕获鼠标(原生选择 vs 滚轮)",
    slash_copy: "最近一条代理回复 — 复制到剪贴板",
    sys_mouse_on: "鼠标已捕获:滚轮与点击聚焦可用;选择 — Shift+拖动",
    sys_mouse_off: "鼠标已释放:原生选择;滚轮不可用。恢复 — /mouse",
    sys_copied: "最近回复已复制到剪贴板",
    sys_nothing_to_copy: "日志为空 — 没有可复制的内容",
    sys_no_clipboard: "剪贴板不可用:未找到 wl-copy/xclip/xsel/pbcopy",
    sys_providers_empty: "未配置提供商 — /models add",
    sys_config_journal: "日志: ",
    sys_config_mode: "确认模式: ",
    sys_config_providers: "提供商数: ",
    sys_config_locale: "界面语言: ",
    sys_locale_set: "— 已保存到本地配置",
    sys_locale_unknown: "未知语言 — 可用: ru, en, de, fr, es, zh-CN, ja, ko",
};

const JA: Strings = Strings {
    hint_default: " Enter — 送信 · Alt+Enter — 改行 · ホイール/クリック — ログ · 選択 — Shift+ドラッグ · / — コマンド",
    hint_busy: " エージェント実行中… · Ctrl+C — 終了",
    hint_confirm: " ←→ — 選択 · Enter — 決定 · y/n — 即答 · Esc — いいえ",
    hint_picker: " ↑↓ — 選択 · Space — マーク · Enter — 確定 · Esc — キャンセル",
    hint_slash: " ↑↓ — 選択 · Tab/Enter — 挿入 · Esc — 閉じる",
    hint_log_focus: " ログにフォーカス · ↑↓/PgUp/PgDn/ホイール — スクロール · Esc または入力欄クリック — 戻る",
    hint_mouse_off: " マウス解放中 — ネイティブ選択 · /mouse — ホイール復帰 · /copy — 返信をクリップボードへ",
    header_workspace: " 領域: ",
    header_models: " モデル: ",
    header_models_empty: "未設定 — /models add",
    header_thinking: " 思考中…",
    confirm_title: " 操作の確認 ",
    confirm_yes: " はい [y] ",
    confirm_yes_hint: "許可",
    confirm_session: " セッション [s] ",
    confirm_session_hint: "セッション中有効",
    confirm_project: " プロジェクト [p] ",
    confirm_project_hint: "ツール — .berimor/allow へ",
    confirm_all: " すべて [a] ",
    confirm_all_hint: "プロジェクト全般に許可",
    confirm_no: " いいえ [n] ",
    confirm_no_hint: "拒否",
    confirm_nav: "  ←→/Tab — 選択, Enter — 決定",
    secret_title: " シークレット ",
    key_label: " API キー",
    key_hint: " Enter — 保存 · Esc — スキップ",
    plugin_title: " プラグイン ",
    plugin_repo_label: " プラグインリポジトリ URL:",
    plugin_repo_hint: " Enter — インストール(完了まで TUI 一時停止) · Esc — キャンセル",
    picker_presets: "プリセット(Space — マーク, Enter — 確定)",
    picker_provider: "プロバイダー(Enter — 選択)",
    picker_remove: "削除(Enter — 確定)",
    picker_locale: "UI 言語(Enter — 選択)",
    slash_help: "コマンド一覧",
    slash_config: "有効な設定",
    slash_models: "モデルプロバイダー",
    slash_models_add: "ウィザード:プリセット → モデル一覧取得 → キー",
    slash_model: "セッションモデル変更(プロバイダー一覧から選択)",
    slash_tools: "利用可能なツール一覧",
    slash_skills: "インストール済みスキル一覧",
    slash_skills_add: "berimor-skills カタログからスキルをインストール",
    slash_skills_remove: "インストール済みスキルを削除",
    slash_agents: "インストール済みサブエージェント一覧",
    slash_agents_add: "berimor-agents カタログからサブエージェントをインストール",
    slash_agents_remove: "インストール済みサブエージェントを削除",
    slash_plugins: "インストール済みプラグイン一覧",
    slash_plugins_add: "信頼済みリポジトリからプラグインをインストール",
    slash_plugins_remove: "インストール済みプラグインを削除",
    slash_exit: "終了",
    slash_mouse: "マウス解放/キャプチャ(ネイティブ選択 vs ホイール)",
    slash_copy: "最新のエージェント返信 — クリップボードへ",
    sys_mouse_on: "マウスをキャプチャ:ホイールとクリックフォーカス有効;選択 — Shift+ドラッグ",
    sys_mouse_off: "マウスを解放:ネイティブ選択;ホイール不可。復帰 — /mouse",
    sys_copied: "最新の返信をクリップボードにコピーしました",
    sys_nothing_to_copy: "ログが空 — コピーするものがありません",
    sys_no_clipboard: "クリップボード不可:wl-copy/xclip/xsel/pbcopy が見つかりません",
    sys_providers_empty: "プロバイダー未設定 — /models add",
    sys_config_journal: "ジャーナル: ",
    sys_config_mode: "確認モード: ",
    sys_config_providers: "プロバイダー数: ",
    sys_config_locale: "UI 言語: ",
    sys_locale_set: "— ローカル設定に保存しました",
    sys_locale_unknown: "不明な言語 — 利用可能: ru, en, de, fr, es, zh-CN, ja, ko",
};

const KO: Strings = Strings {
    hint_default:
        " Enter — 전송 · Alt+Enter — 줄바꿈 · 휠/클릭 — 로그 · 선택 — Shift+드래그 · / — 명령",
    hint_busy: " 에이전트 작업 중… · Ctrl+C — 종료",
    hint_confirm: " ←→ — 선택 · Enter — 확정 · y/n — 바로 · Esc — 아니요",
    hint_picker: " ↑↓ — 선택 · Space — 표시 · Enter — 확인 · Esc — 취소",
    hint_slash: " ↑↓ — 선택 · Tab/Enter — 삽입 · Esc — 닫기",
    hint_log_focus: " 로그 포커스 · ↑↓/PgUp/PgDn/휠 — 스크롤 · Esc 또는 입력창 클릭 — 돌아가기",
    hint_mouse_off: " 마우스 해제됨 — 네이티브 선택 · /mouse — 휠 복원 · /copy — 답변을 클립보드로",
    header_workspace: " 영역: ",
    header_models: " 모델: ",
    header_models_empty: "구성되지 않음 — /models add",
    header_thinking: " 생각 중…",
    confirm_title: " 작업 확인 ",
    confirm_yes: " 예 [y] ",
    confirm_yes_hint: "허용",
    confirm_session: " 세션 [s] ",
    confirm_session_hint: "세션 동안 유효",
    confirm_project: " 프로젝트 [p] ",
    confirm_project_hint: "도구 — .berimor/allow에",
    confirm_all: " 모두 [a] ",
    confirm_all_hint: "프로젝트 전체 허용",
    confirm_no: " 아니요 [n] ",
    confirm_no_hint: "거부",
    confirm_nav: "  ←→/Tab — 선택, Enter — 확정",
    secret_title: " 시크릿 ",
    key_label: " API 키",
    key_hint: " Enter — 저장 · Esc — 건너뛰기",
    plugin_title: " 플러그인 ",
    plugin_repo_label: " 플러그인 저장소 URL:",
    plugin_repo_hint: " Enter — 설치(설치 중 TUI 일시정지) · Esc — 취소",
    picker_presets: "프리셋(Space — 표시, Enter — 확인)",
    picker_provider: "제공자(Enter — 선택)",
    picker_remove: "삭제(Enter — 확인)",
    picker_locale: "인터페이스 언어(Enter — 선택)",
    slash_help: "명령 목록",
    slash_config: "유효 구성",
    slash_models: "모델 제공자",
    slash_models_add: "마법사: 프리셋 → 실시간 모델 목록 → 키",
    slash_model: "세션 모델 변경(제공자 목록에서 선택)",
    slash_tools: "사용 가능한 도구 목록",
    slash_skills: "설치된 스킬 목록",
    slash_skills_add: "berimor-skills 카탈로그에서 스킬 설치",
    slash_skills_remove: "설치된 스킬 삭제",
    slash_agents: "설치된 서브에이전트 목록",
    slash_agents_add: "berimor-agents 카탈로그에서 서브에이전트 설치",
    slash_agents_remove: "설치된 서브에이전트 삭제",
    slash_plugins: "설치된 플러그인 목록",
    slash_plugins_add: "신뢰된 저장소에서 플러그인 설치",
    slash_plugins_remove: "설치된 플러그인 삭제",
    slash_exit: "종료",
    slash_mouse: "마우스 해제/캡처(네이티브 선택 vs 휠)",
    slash_copy: "마지막 에이전트 답변 — 클립보드로",
    sys_mouse_on: "마우스 캡처됨: 휠과 클릭 포커스 사용 가능; 선택 — Shift+드래그",
    sys_mouse_off: "마우스 해제됨: 네이티브 선택; 휠 사용 불가. 복원 — /mouse",
    sys_copied: "마지막 답변을 클립보드에 복사했습니다",
    sys_nothing_to_copy: "로그가 비어 있음 — 복사할 내용 없음",
    sys_no_clipboard: "클립보드 사용 불가: wl-copy/xclip/xsel/pbcopy를 찾을 수 없음",
    sys_providers_empty: "구성된 제공자 없음 — /models add",
    sys_config_journal: "저널: ",
    sys_config_mode: "확인 모드: ",
    sys_config_providers: "제공자 수: ",
    sys_config_locale: "인터페이스 언어: ",
    sys_locale_set: "— 로컬 구성에 저장됨",
    sys_locale_unknown: "알 수 없는 언어 — 사용 가능: ru, en, de, fr, es, zh-CN, ja, ko",
};

/// Таблица строк локали.
pub fn strings(locale: Locale) -> &'static Strings {
    match locale {
        Locale::Ru => &RU,
        Locale::En => &EN,
        Locale::De => &DE,
        Locale::Fr => &FR,
        Locale::Es => &ES,
        Locale::ZhCn => &ZH_CN,
        Locale::Ja => &JA,
        Locale::Ko => &KO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Коды уникальны и ходят туда-обратно; native-имена не пустые.
    #[test]
    fn locale_codes_roundtrip() {
        let mut codes = std::collections::HashSet::new();
        for locale in Locale::ALL {
            assert!(codes.insert(locale.code()), "дубликат кода");
            assert!(!locale.native_name().is_empty());
            assert_eq!(Locale::from_code(locale.code()), Some(locale));
        }
        assert_eq!(Locale::from_code("ZH-cn"), Some(Locale::ZhCn));
        assert_eq!(Locale::from_code("zh"), Some(Locale::ZhCn));
        assert_eq!(Locale::from_code("xx"), None);
    }

    /// Разрешение локали: конфиг сильнее окружения, умолчание — ru.
    #[test]
    fn locale_resolution_prefers_config() {
        assert_eq!(Locale::resolve(Some("en")), Locale::En);
        assert_eq!(Locale::resolve(Some("JA")), Locale::Ja);
        // Мусор в конфиге не роняет: откат на окружение/ru.
        let fallback = Locale::resolve(Some("klingon"));
        assert!(Locale::ALL.contains(&fallback));
    }

    /// Контракт полноты — компиляторный (обязательные поля struct),
    /// здесь страховка от пустых переводов ключевых строк.
    #[test]
    fn all_locales_have_nonempty_chrome() {
        for locale in Locale::ALL {
            let s = strings(locale);
            assert!(!s.hint_default.is_empty(), "{:?}", locale);
            assert!(!s.confirm_yes.is_empty(), "{:?}", locale);
            assert!(!s.slash_help.is_empty(), "{:?}", locale);
            assert!(!s.sys_locale_unknown.is_empty(), "{:?}", locale);
        }
    }
}

//! Резолвер параметров клиентской сессии для подключения к session-manager.
//!
//! Контракт зафиксирован в `v8-client-session-manager` ADR-0020:
//!
//! - `manager_url` — из константы расширения `WebTransportSessionManagerURL`;
//!   если константа пуста — fallback на default `ws://127.0.0.1:4000/sessions`;
//!   значение `manager_url=...` в `/C` ИГНОРИРУЕТСЯ (явно вне контракта).
//! - `client_uid` — генерируется на стороне 1С через `Новый УникальныйИдентификатор()`;
//!   для удобства тестов и harness'а здесь же есть [`fresh_client_uid`].
//! - `kind` — берётся из `/C"kind=..."`, если задан, иначе вычисляется по
//!   `СтрокаЗапуска()` (LaunchString) по эвристике: `/TESTMANAGER` →
//!   `vanessa_manager`, `/TESTCLIENT` → `vanessa_test_client`,
//!   `RunYaXUnit` → `yaxunit_runner`, иначе → `client`.
//! - `correlation_id` — напрямую из `/C"correlation_id=..."`; если нет —
//!   `None`.
//!
//! Парсер `ПараметрЗапуска` (`StartupParameter`) умышленно живёт здесь, в
//! Rust, а не в БСП‑обёртке: это снимает завязку реализации на конкретный
//! релиз БСП и даёт возможность unit‑тестировать всю выборку источников
//! одним прогоном `cargo test`. Со стороны 1С остаётся только тонкий
//! адаптер: прочитать константу, вызвать `СтрокаЗапуска()` и `ПараметрЗапуска`,
//! передать всё в этот резолвер.

use std::collections::HashMap;

use serde::Serialize;

/// Default `manager_url`, совпадает с default‑bind менеджера
/// (см. `v8-client-session-manager` SESSION_MANAGER.md §8.3).
pub const DEFAULT_MANAGER_URL: &str = "ws://127.0.0.1:4000/sessions";

/// Имя константы расширения, в которой хранится `manager_url`.
/// Используется только для документации/логов — чтение идёт на стороне 1С.
pub const MANAGER_URL_CONSTANT: &str = "WebTransportSessionManagerURL";

/// Известные значения `kind`, которые менеджер ожидает в `session.register`.
pub mod kinds {
    pub const CLIENT: &str = "client";
    pub const VANESSA_MANAGER: &str = "vanessa_manager";
    pub const VANESSA_TEST_CLIENT: &str = "vanessa_test_client";
    pub const YAXUNIT_RUNNER: &str = "yaxunit_runner";
}

/// Срез входных данных, которые 1С‑адаптер передаёт в резолвер.
#[derive(Debug, Clone, Default)]
pub struct ResolveInput {
    /// Значение константы `WebTransportSessionManagerURL`. Если константа
    /// пуста / не существует — передавать пустую строку.
    pub constant_url: String,
    /// `СтрокаЗапуска()` — полная команда, например
    /// `"DESIGNER /TESTMANAGER /S\"server/db\""`. Может быть пустой.
    pub launch_string: String,
    /// `ПараметрЗапуска` (`StartupParameter`) — содержимое ключа `/C"..."`
    /// без обрамляющих кавычек, например `"correlation_id=abc kind=foo"`.
    pub startup_param: String,
    /// Уже сгенерированный `client_uid` (через `Новый УникальныйИдентификатор()`
    /// на стороне 1С). Если пусто — резолвер сам сгенерирует через
    /// [`fresh_client_uid`], но это compatibility‑путь для tests/harness'а;
    /// в production 1С обязан передать свой UUID.
    pub client_uid: String,
}

/// Результат резолва.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionParams {
    pub manager_url: String,
    pub client_uid: String,
    pub kind: String,
    /// `None`, если `correlation_id=` не задан в `/C`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl SessionParams {
    /// JSON‑сериализация для возврата 1С‑коду через addin (см. ADR‑0020).
    pub fn to_json(&self) -> String {
        // Поля заведомо валидный UTF‑8 без управляющих символов; serde_json
        // никогда здесь не падает.
        serde_json::to_string(self).expect("SessionParams serialization is infallible")
    }
}

/// Сгенерировать свежий `client_uid`. Используется harness'ом и как
/// fallback в [`resolve`] при пустом `ResolveInput::client_uid`.
pub fn fresh_client_uid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Главный резолвер. Не имеет побочных эффектов и не читает окружение —
/// все источники приходят на вход через [`ResolveInput`].
pub fn resolve(input: &ResolveInput) -> SessionParams {
    let manager_url = if input.constant_url.trim().is_empty() {
        DEFAULT_MANAGER_URL.to_owned()
    } else {
        input.constant_url.trim().to_owned()
    };

    let pairs = parse_startup_param(&input.startup_param);
    let kind_override = pairs.get("kind").map(String::as_str);
    let correlation_id = pairs.get("correlation_id").cloned();

    let kind = match kind_override {
        Some(k) if !k.is_empty() => k.to_owned(),
        _ => infer_kind(&input.launch_string),
    };

    let client_uid = if input.client_uid.is_empty() {
        fresh_client_uid()
    } else {
        input.client_uid.clone()
    };

    SessionParams {
        manager_url,
        client_uid,
        kind,
        correlation_id,
    }
}

/// Парсер строки вида `key=value key=value` (значения без пробелов и кавычек,
/// в стиле `ПараметрЗапуска`). Дубли — побеждает последний.
pub fn parse_startup_param(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for token in raw.split_whitespace() {
        if let Some((k, v)) = token.split_once('=') {
            if !k.is_empty() {
                out.insert(k.to_owned(), v.to_owned());
            }
        }
    }
    out
}

/// Эвристика определения `kind` по `СтрокаЗапуска()`. Регистр не учитывается.
pub fn infer_kind(launch_string: &str) -> String {
    let upper = launch_string.to_ascii_uppercase();
    if upper.contains("/TESTMANAGER") {
        kinds::VANESSA_MANAGER.to_owned()
    } else if upper.contains("/TESTCLIENT") {
        kinds::VANESSA_TEST_CLIENT.to_owned()
    } else if upper.contains("RUNYAXUNIT") {
        kinds::YAXUNIT_RUNNER.to_owned()
    } else {
        kinds::CLIENT.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(constant: &str, launch: &str, startup: &str, uid: &str) -> ResolveInput {
        ResolveInput {
            constant_url: constant.to_owned(),
            launch_string: launch.to_owned(),
            startup_param: startup.to_owned(),
            client_uid: uid.to_owned(),
        }
    }

    #[test]
    fn empty_constant_falls_back_to_default_url() {
        let p = resolve(&input("", "", "", "uid-1"));
        assert_eq!(p.manager_url, DEFAULT_MANAGER_URL);
    }

    #[test]
    fn whitespace_constant_treated_as_empty() {
        let p = resolve(&input("   ", "", "", "uid-1"));
        assert_eq!(p.manager_url, DEFAULT_MANAGER_URL);
    }

    #[test]
    fn explicit_constant_wins() {
        let p = resolve(&input("ws://10.0.0.5:4000/sessions", "", "", "uid-1"));
        assert_eq!(p.manager_url, "ws://10.0.0.5:4000/sessions");
    }

    #[test]
    fn manager_url_in_startup_param_is_ignored() {
        // По ADR‑0020 ключ manager_url=... в /C явно вне контракта.
        let p = resolve(&input("", "", "manager_url=ws://hijack/", "uid-1"));
        assert_eq!(p.manager_url, DEFAULT_MANAGER_URL);
    }

    #[test]
    fn kind_inferred_from_testmanager_flag() {
        let p = resolve(&input("", "DESIGNER /TESTMANAGER", "", "uid-1"));
        assert_eq!(p.kind, kinds::VANESSA_MANAGER);
    }

    #[test]
    fn kind_inferred_from_testclient_flag_case_insensitive() {
        let p = resolve(&input("", "1cv8c /testclient -port=12345", "", "uid-1"));
        assert_eq!(p.kind, kinds::VANESSA_TEST_CLIENT);
    }

    #[test]
    fn kind_inferred_from_runyaxunit_in_command_line() {
        let p = resolve(&input(
            "",
            "1cv8c ENTERPRISE /Sserver /CRunYaXUnit",
            "",
            "uid-1",
        ));
        assert_eq!(p.kind, kinds::YAXUNIT_RUNNER);
    }

    #[test]
    fn kind_defaults_to_client() {
        let p = resolve(&input("", "1cv8c ENTERPRISE", "", "uid-1"));
        assert_eq!(p.kind, kinds::CLIENT);
    }

    #[test]
    fn kind_override_from_startup_param_beats_heuristic() {
        // /TESTMANAGER в launch, но override через /C"kind=..." должен победить.
        let p = resolve(&input(
            "",
            "DESIGNER /TESTMANAGER",
            "kind=yaxunit_runner",
            "uid-1",
        ));
        assert_eq!(p.kind, "yaxunit_runner");
    }

    #[test]
    fn empty_kind_override_keeps_heuristic() {
        let p = resolve(&input("", "DESIGNER /TESTMANAGER", "kind=", "uid-1"));
        assert_eq!(p.kind, kinds::VANESSA_MANAGER);
    }

    #[test]
    fn correlation_id_extracted_from_startup_param() {
        let p = resolve(&input("", "", "correlation_id=trace-42", "uid-1"));
        assert_eq!(p.correlation_id.as_deref(), Some("trace-42"));
    }

    #[test]
    fn correlation_id_absent_yields_none() {
        let p = resolve(&input("", "", "kind=client", "uid-1"));
        assert!(p.correlation_id.is_none());
    }

    #[test]
    fn multiple_startup_pairs_parsed_independently() {
        let p = resolve(&input(
            "",
            "DESIGNER",
            "correlation_id=trace-7 kind=vanessa_manager other=ignored",
            "uid-1",
        ));
        assert_eq!(p.correlation_id.as_deref(), Some("trace-7"));
        assert_eq!(p.kind, "vanessa_manager");
    }

    #[test]
    fn duplicate_keys_last_wins() {
        let pairs = parse_startup_param("kind=a kind=b");
        assert_eq!(pairs.get("kind").map(String::as_str), Some("b"));
    }

    #[test]
    fn parse_skips_tokens_without_equals() {
        let pairs = parse_startup_param("garbage kind=client also-garbage");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs.get("kind").map(String::as_str), Some("client"));
    }

    #[test]
    fn empty_client_uid_generates_fresh_uuid_v4() {
        let p = resolve(&input("", "", "", ""));
        // Формат UUID v4: 36 символов с дефисами в правильных позициях.
        assert_eq!(p.client_uid.len(), 36);
        assert_eq!(p.client_uid.as_bytes()[8], b'-');
        assert_eq!(p.client_uid.as_bytes()[14], b'4'); // version
    }

    #[test]
    fn provided_client_uid_passes_through() {
        let p = resolve(&input("", "", "", "custom-uid"));
        assert_eq!(p.client_uid, "custom-uid");
    }

    #[test]
    fn fresh_client_uid_is_unique_between_calls() {
        let a = fresh_client_uid();
        let b = fresh_client_uid();
        assert_ne!(a, b);
    }

    #[test]
    fn json_serialization_drops_absent_correlation_id() {
        let p = SessionParams {
            manager_url: DEFAULT_MANAGER_URL.to_owned(),
            client_uid: "uid-1".to_owned(),
            kind: "client".to_owned(),
            correlation_id: None,
        };
        let json = p.to_json();
        assert!(!json.contains("correlation_id"));
        assert!(json.contains("\"manager_url\":\"ws://127.0.0.1:4000/sessions\""));
        assert!(json.contains("\"kind\":\"client\""));
    }

    #[test]
    fn json_serialization_includes_correlation_id_when_present() {
        let p = SessionParams {
            manager_url: "ws://x/y".to_owned(),
            client_uid: "uid-2".to_owned(),
            kind: "vanessa_manager".to_owned(),
            correlation_id: Some("trace-9".to_owned()),
        };
        let json = p.to_json();
        assert!(json.contains("\"correlation_id\":\"trace-9\""));
    }

    #[test]
    fn end_to_end_realistic_yaxunit_runner_invocation() {
        // Менеджер спавнит yaxunit_runner: kind override + correlation_id.
        let p = resolve(&input(
            "ws://onec-infra:4000/sessions",
            "1cv8c ENTERPRISE /Sserver /CRunYaXUnit",
            "correlation_id=spawn-42 kind=yaxunit_runner",
            "fixed-uid-7",
        ));
        assert_eq!(p.manager_url, "ws://onec-infra:4000/sessions");
        assert_eq!(p.kind, "yaxunit_runner");
        assert_eq!(p.correlation_id.as_deref(), Some("spawn-42"));
        assert_eq!(p.client_uid, "fixed-uid-7");
    }
}

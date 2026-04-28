//! Абстракция над `addin1c::Connection.external_event` для доставки событий
//! в платформу 1С.
//!
//! Существует две причины для этого слоя:
//!
//! 1. Тестируемость. Логика, которая должна порождать событие в 1С (WS-tunnel
//!    из этапа 5, MCP/HTTP мостики), сейчас жёстко зависит от
//!    `addin1c::Connection`, который без живой 1С создать нельзя. Через
//!    [`AddinHost`] можно подменить хост на in-memory мок и проверить
//!    взаимодействие с session-manager без подключения к информационной базе.
//! 2. Однообразие. На стороне платформы внешнее событие из компоненты — это
//!    тройка `(имя_компоненты, имя_события, payload)`. У нас она встречается в
//!    нескольких местах (MCP server, HTTP server, MCP handler). Единая
//!    точка входа упрощает дальнейшее добавление correlation_id и аудита из
//!    плана этапа 7.
//!
//! Решение зафиксировано в [`docs/decisions/0004-mock-strategy-for-addin-host-tests.md`].
//!
//! Модуль умышленно не трогает существующие call-site'ы (`mcp::server`,
//! `http::server`, `http::mcp_handler`). Это сделано в подзадачах 5.3+ при
//! внедрении WS-tunnel — там же существующие вызовы будут переключены на
//! `AddinHost`. На этапе 5.1 модуль добавляется аддитивно: тип ещё не
//! используется в production-коде, но уже доступен и покрыт unit-тестами.

use std::sync::Mutex;

use addin1c::{name, CString1C, Connection};

/// Имя компоненты, под которым все события приходят в 1С.
pub const COMPONENT_NAME: &str = "WebTransport";

/// Хост, способный доставить внешнее событие 1С.
///
/// Контракт повторяет `addin1c::Connection.external_event`:
/// - `event` — имя события (например, `"MCP_MESSAGE"`, `"WS_INCOMING"`);
/// - `payload` — произвольный UTF-8 текст (как правило, JSON);
/// - возвращает `false`, если очередь событий 1С переполнена.
///
/// Реализация ДОЛЖНА быть `Sync + Send`, чтобы её можно было использовать
/// из tokio-задач и WS-цикла tunnel'а.
pub trait AddinHost: Send + Sync {
    /// Доставить событие платформе. Имя компоненты подставляется реализацией.
    fn external_event(&self, event: &str, payload: &str) -> bool;
}

/// Реализация поверх `addin1c::Connection` — то, что используется внутри
/// загруженной в 1С компоненты.
///
/// `Connection` приходит от платформы как `&'static`, поэтому хост хранит
/// ссылку без владения.
pub struct RealAddinHost {
    connection: &'static Connection,
}

impl RealAddinHost {
    pub fn new(connection: &'static Connection) -> Self {
        Self { connection }
    }
}

impl AddinHost for RealAddinHost {
    fn external_event(&self, event: &str, payload: &str) -> bool {
        let event_c = CString1C::from(event);
        let payload_c = CString1C::from(payload);
        self.connection
            .external_event(name!("WebTransport"), event_c, payload_c)
    }
}

/// In-memory реализация для тестов. Накапливает все вызовы в виде
/// `(event, payload)` и возвращает заранее настроенный признак успеха.
///
/// Для проверки в тесте: см. [`MockAddinHost::events`].
#[derive(Debug, Default)]
pub struct MockAddinHost {
    inner: Mutex<MockState>,
}

#[derive(Debug, Default)]
struct MockState {
    events: Vec<(String, String)>,
    queue_full: bool,
}

impl MockAddinHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Имитировать переполнение очереди — следующие вызовы будут возвращать
    /// `false`. Сбрасывается через [`MockAddinHost::set_queue_full`] с
    /// `false`.
    pub fn set_queue_full(&self, value: bool) {
        let mut guard = self.inner.lock().expect("mock host poisoned");
        guard.queue_full = value;
    }

    /// Снять снапшот текущих событий. Не очищает буфер.
    pub fn events(&self) -> Vec<(String, String)> {
        self.inner
            .lock()
            .expect("mock host poisoned")
            .events
            .clone()
    }

    /// Очистить накопленные события (удобно между фазами теста).
    pub fn clear(&self) {
        self.inner
            .lock()
            .expect("mock host poisoned")
            .events
            .clear();
    }
}

impl AddinHost for MockAddinHost {
    fn external_event(&self, event: &str, payload: &str) -> bool {
        let mut guard = self.inner.lock().expect("mock host poisoned");
        if guard.queue_full {
            return false;
        }
        guard.events.push((event.to_owned(), payload.to_owned()));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_records_events_in_order() {
        let host = MockAddinHost::new();
        assert!(host.external_event("WS_INCOMING", "{\"a\":1}"));
        assert!(host.external_event("MCP_MESSAGE", "{\"b\":2}"));

        let events = host.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, "WS_INCOMING");
        assert_eq!(events[0].1, "{\"a\":1}");
        assert_eq!(events[1].0, "MCP_MESSAGE");
    }

    #[test]
    fn mock_returns_false_when_queue_full() {
        let host = MockAddinHost::new();
        host.set_queue_full(true);
        assert!(!host.external_event("WS_INCOMING", "ignored"));
        assert!(host.events().is_empty());

        host.set_queue_full(false);
        assert!(host.external_event("WS_INCOMING", "{}"));
        assert_eq!(host.events().len(), 1);
    }

    #[test]
    fn mock_clear_drops_buffered_events() {
        let host = MockAddinHost::new();
        host.external_event("E1", "p1");
        host.external_event("E2", "p2");
        host.clear();
        assert!(host.events().is_empty());

        host.external_event("E3", "p3");
        assert_eq!(host.events(), vec![("E3".to_owned(), "p3".to_owned())]);
    }

    #[test]
    fn mock_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MockAddinHost>();
        assert_send_sync::<RealAddinHost>();
    }

    #[test]
    fn dyn_addin_host_dispatch_works() {
        let host: Box<dyn AddinHost> = Box::new(MockAddinHost::new());
        assert!(host.external_event("X", "y"));
    }
}

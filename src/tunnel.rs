//! Двунаправленный pump WS‑tunnel'а между session‑manager'ом и 1С‑клиентом.
//!
//! Обязанности модуля (этап 5.3 backlog'а `v8-client-session-manager`):
//!
//! - читать входящие WS‑фреймы от менеджера (как правило, JSON‑RPC `tool.call`)
//!   и доставлять их 1С‑коду через [`AddinHost::external_event`] под именем
//!   [`EVENT_INCOMING`];
//! - принимать исходящие сообщения от 1С‑кода (ответы на `tool.call`,
//!   `session.bye`, нотификации) и отправлять их в WS;
//! - корректно завершаться по cancel‑токену или по закрытию канала.
//!
//! Транспорт (TCP/TLS, handshake, upgrade) живёт за пределами модуля. Здесь
//! работают только абстрактные [`futures_util::Stream`] и
//! [`futures_util::Sink`]. Это намеренно: единственный способ проверять
//! tunnel без живой 1С — подменить транспорт на mpsc‑каналы (см. unit‑тесты).
//! Адаптер поверх `tokio_tungstenite::WebSocketStream` будет в подзадаче
//! реконнекта (5.4) и в harness'е (5.7).
//!
//! ADR‑0004 фиксирует, почему dispatch идёт через trait `AddinHost`, а не
//! напрямую через `addin1c::Connection.external_event`.

use std::sync::Arc;

use futures_util::{Sink, SinkExt, Stream, StreamExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::addin_host::AddinHost;

/// Имя внешнего события 1С для входящих сообщений из tunnel'а.
pub const EVENT_INCOMING: &str = "WS_INCOMING";

/// Полезный фрейм, который умеет передавать tunnel. Бинарные WS‑фреймы вне
/// контракта менеджера, поэтому отдельная ветка для них не нужна — они
/// игнорируются на уровне адаптера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextOrClose {
    Text(String),
    Close,
}

/// Причина завершения [`run_tunnel`]. Полезна harness'у/реконнекту, чтобы
/// различать «нас закрыли» и «у нас сдох транспорт».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// Cancel‑токен сработал — штатное завершение по запросу владельца.
    Cancelled,
    /// Удалённая сторона прислала `Close` или закрыла Stream.
    Closed,
    /// Ошибка чтения из Stream — потенциально временная, повод для reconnect.
    InboundError,
    /// Outbound‑канал обронил sender (1С‑код больше не может слать) — фатально.
    OutboundDropped,
    /// Ошибка записи в Sink — потенциально временная, повод для reconnect.
    SinkError,
}

/// Доставить одно входящее текстовое сообщение в 1С через хост.
///
/// Возвращает `false`, если очередь внешних событий 1С переполнена. Tunnel
/// в этом случае НЕ отбрасывает соединение: переполнение — кратковременная
/// перегрузка стороны 1С; сами фреймы менеджер пришлёт повторно по таймауту
/// `tool.call`. Логирование оставлено call‑site'у (см. [`run_tunnel`]).
pub fn dispatch_incoming(payload: &str, host: &dyn AddinHost) -> bool {
    host.external_event(EVENT_INCOMING, payload)
}

/// Главный pump tunnel'а.
///
/// - `inbound` — поток фреймов от менеджера (адаптер уже свернул `Ping/Pong`
///   и бинарные фреймы);
/// - `sink` — куда писать исходящие текстовые фреймы;
/// - `outbound` — очередь, в которую 1С‑код кладёт исходящие сообщения через
///   [`OutboundSender`];
/// - `host` — addin‑хост, через который dispatch'ится `WS_INCOMING`;
/// - `cancel` — токен для штатного шатдауна.
pub async fn run_tunnel<R, W, EIn, EOut>(
    mut inbound: R,
    sink: W,
    mut outbound: mpsc::UnboundedReceiver<String>,
    host: Arc<dyn AddinHost>,
    cancel: CancellationToken,
) -> RunOutcome
where
    R: Stream<Item = Result<TextOrClose, EIn>> + Unpin,
    W: Sink<String, Error = EOut> + Unpin,
{
    let mut sink = sink;
    loop {
        tokio::select! {
            biased;

            _ = cancel.cancelled() => {
                let _ = sink.close().await;
                return RunOutcome::Cancelled;
            }

            frame = inbound.next() => {
                match frame {
                    None => return RunOutcome::Closed,
                    Some(Ok(TextOrClose::Close)) => return RunOutcome::Closed,
                    Some(Ok(TextOrClose::Text(payload))) => {
                        // Failure to deliver = переполнение очереди 1С;
                        // продолжаем pump, см. dispatch_incoming.
                        let _ = dispatch_incoming(&payload, host.as_ref());
                    }
                    Some(Err(_)) => return RunOutcome::InboundError,
                }
            }

            outgoing = outbound.recv() => {
                match outgoing {
                    None => return RunOutcome::OutboundDropped,
                    Some(text) => {
                        if sink.send(text).await.is_err() {
                            return RunOutcome::SinkError;
                        }
                    }
                }
            }
        }
    }
}

/// Тонкая обёртка над unbounded sender'ом для исходящих сообщений. Хранится
/// у владельца tunnel'а (в production — у 1С‑addin класса).
#[derive(Clone)]
pub struct OutboundSender(mpsc::UnboundedSender<String>);

impl OutboundSender {
    pub fn new(tx: mpsc::UnboundedSender<String>) -> Self {
        Self(tx)
    }

    /// Отправить текстовый фрейм. `Err` означает, что pump уже завершился.
    pub fn send(&self, text: String) -> Result<(), SendError> {
        self.0.send(text).map_err(|_| SendError::Closed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::convert::Infallible;

    use futures_util::stream;
    use tokio::sync::mpsc::unbounded_channel;
    use tokio::time::{timeout, Duration};

    use crate::addin_host::MockAddinHost;

    /// Sink, который складывает отправленные сообщения в Vec через mpsc.
    struct VecSink(mpsc::UnboundedSender<String>);

    impl Sink<String> for VecSink {
        type Error = Infallible;
        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn start_send(self: std::pin::Pin<&mut Self>, item: String) -> Result<(), Self::Error> {
            let _ = self.0.send(item);
            Ok(())
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    fn host() -> Arc<MockAddinHost> {
        Arc::new(MockAddinHost::new())
    }

    #[test]
    fn dispatch_incoming_emits_ws_incoming_event() {
        let h = MockAddinHost::new();
        assert!(dispatch_incoming("{\"jsonrpc\":\"2.0\"}", &h));
        let evs = h.events();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].0, EVENT_INCOMING);
        assert_eq!(evs[0].1, "{\"jsonrpc\":\"2.0\"}");
    }

    #[test]
    fn dispatch_incoming_returns_false_on_queue_overflow() {
        let h = MockAddinHost::new();
        h.set_queue_full(true);
        assert!(!dispatch_incoming("payload", &h));
    }

    #[tokio::test]
    async fn pump_forwards_text_frames_then_closes() {
        let h = host();
        let frames: Vec<Result<TextOrClose, Infallible>> = vec![
            Ok(TextOrClose::Text("a".to_owned())),
            Ok(TextOrClose::Text("b".to_owned())),
            Ok(TextOrClose::Close),
        ];
        let inbound = stream::iter(frames);
        let (sink_tx, _sink_rx) = unbounded_channel::<String>();
        let sink = VecSink(sink_tx);
        let (_outbound_tx, outbound_rx) = unbounded_channel::<String>();
        let cancel = CancellationToken::new();

        let host_for_pump: Arc<dyn AddinHost> = h.clone();
        let outcome =
            run_tunnel(inbound, sink, outbound_rx, host_for_pump, cancel).await;
        assert_eq!(outcome, RunOutcome::Closed);

        let evs = h.events();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].1, "a");
        assert_eq!(evs[1].1, "b");
    }

    #[tokio::test]
    async fn pump_writes_outbound_messages_to_sink() {
        let h = host();
        // Inbound никогда не присылает Close — гасим через cancel.
        let inbound = stream::pending::<Result<TextOrClose, Infallible>>();
        let (sink_tx, mut sink_rx) = unbounded_channel::<String>();
        let sink = VecSink(sink_tx);
        let (outbound_tx, outbound_rx) = unbounded_channel::<String>();
        let cancel = CancellationToken::new();

        outbound_tx.send("ping".to_owned()).unwrap();
        outbound_tx.send("pong".to_owned()).unwrap();

        let host_for_pump: Arc<dyn AddinHost> = h.clone();
        let cancel_clone = cancel.clone();
        let pump = tokio::spawn(async move {
            run_tunnel(inbound, sink, outbound_rx, host_for_pump, cancel_clone).await
        });

        // Дать pump'у обработать оба сообщения, затем завершить.
        let m1 = timeout(Duration::from_millis(200), sink_rx.recv())
            .await
            .unwrap()
            .unwrap();
        let m2 = timeout(Duration::from_millis(200), sink_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(m1, "ping");
        assert_eq!(m2, "pong");

        cancel.cancel();
        let outcome = pump.await.unwrap();
        assert_eq!(outcome, RunOutcome::Cancelled);
    }

    #[tokio::test]
    async fn pump_returns_inbound_error_on_stream_error() {
        let h = host();
        let frames: Vec<Result<TextOrClose, &'static str>> =
            vec![Ok(TextOrClose::Text("a".to_owned())), Err("boom")];
        let inbound = stream::iter(frames);
        let (sink_tx, _sink_rx) = unbounded_channel::<String>();
        let sink = VecSink(sink_tx);
        let (_outbound_tx, outbound_rx) = unbounded_channel::<String>();
        let cancel = CancellationToken::new();

        let host_for_pump: Arc<dyn AddinHost> = h.clone();
        let outcome =
            run_tunnel(inbound, sink, outbound_rx, host_for_pump, cancel).await;
        assert_eq!(outcome, RunOutcome::InboundError);
        // Первое сообщение должно было успеть пройти.
        assert_eq!(h.events().len(), 1);
    }

    #[tokio::test]
    async fn pump_returns_outbound_dropped_when_sender_closes() {
        let h = host();
        let inbound = stream::pending::<Result<TextOrClose, Infallible>>();
        let (sink_tx, _sink_rx) = unbounded_channel::<String>();
        let sink = VecSink(sink_tx);
        let (outbound_tx, outbound_rx) = unbounded_channel::<String>();
        let cancel = CancellationToken::new();

        drop(outbound_tx);

        let host_for_pump: Arc<dyn AddinHost> = h.clone();
        let outcome =
            run_tunnel(inbound, sink, outbound_rx, host_for_pump, cancel).await;
        assert_eq!(outcome, RunOutcome::OutboundDropped);
    }

    #[tokio::test]
    async fn pump_continues_on_queue_overflow() {
        // Если 1С‑очередь забита, pump не разрывает соединение,
        // просто не доставляет конкретный фрейм.
        let h = host();
        h.set_queue_full(true);
        let frames: Vec<Result<TextOrClose, Infallible>> = vec![
            Ok(TextOrClose::Text("dropped".to_owned())),
            Ok(TextOrClose::Close),
        ];
        let inbound = stream::iter(frames);
        let (sink_tx, _sink_rx) = unbounded_channel::<String>();
        let sink = VecSink(sink_tx);
        let (_outbound_tx, outbound_rx) = unbounded_channel::<String>();
        let cancel = CancellationToken::new();

        let host_for_pump: Arc<dyn AddinHost> = h.clone();
        let outcome =
            run_tunnel(inbound, sink, outbound_rx, host_for_pump, cancel).await;
        assert_eq!(outcome, RunOutcome::Closed);
        assert!(h.events().is_empty());
    }

    #[test]
    fn outbound_sender_send_returns_closed_after_drop() {
        let (tx, rx) = unbounded_channel::<String>();
        let s = OutboundSender::new(tx);
        drop(rx);
        assert_eq!(s.send("x".to_owned()), Err(SendError::Closed));
    }
}

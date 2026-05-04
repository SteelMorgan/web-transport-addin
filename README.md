# WebTransport 1C Addin (Rust)

Внешняя компонента для 1С, объединяющая WebSocket‑клиент и HTTP/SSE сервер с обменом событиями с 1С.

Проект основан на оригинальном репозитории и шаблоне внешней компоненты на Rust:
- Первоисточник: https://github.com/dlyubanevich/websocket1c
- Шаблон компоненты (Rust): https://github.com/medigor/addin1c

## Состав и имена классов

Компонента экспортирует 3 класса (имена для `Новый("AddIn.*")`):
- `ws` — WebSocket‑клиент. См. [docs/ws.md](docs/ws.md).
- `http` — HTTP/SSE сервер с событиями в 1С. См. [docs/http.md](docs/http.md).
- `mcp` — MCP Streamable HTTP сервер (JSON‑only). См. [docs/mcp.md](docs/mcp.md).
- `session` — **(добавлен в форке)** WS‑клиент к `v8-client-session-manager` с auto‑reconnect, correlation‑id, host_id/pid/capabilities и системными методами `addin.spawn` / `addin.kill`.

## Доработки форка SteelMorgan относительно upstream `alkoleft/web-transport-addin`

Форк построен поверх upstream `main` на ревизии 0.6.4 и добавляет 12 коммитов, реализующих транспортный слой для интеграции с **v8-client-session-manager** (см. репозиторий `1C Framework/v8-client-session-manager`). Версия компоненты поднята до **0.7.0** (`Manifest.xml`, `Cargo.toml`).

### Новый класс `AddIn.WebTransport.session` (этап 5–6)

Полный пайплайн «1С‑клиент ↔ session‑manager» через WebSocket. Реализован послойно, каждый слой покрыт unit‑тестами (>=129 lib тестов).

| Файл | Назначение |
|------|-----------|
| `src/addin_host.rs` | trait `AddinHost` + `RealAddinHost` (поверх `addin1c::ExternalEvent`) + `MockAddinHost` для тестов. Изоляция от FFI — позволяет гонять цепочку без живой 1С (ADR‑0004). |
| `src/session_params.rs` | Резолвер параметров сессии (manager_url, kind, client_uid, correlation_id) по контракту ADR‑0020. Эвристика kind по `СтрокаЗапуска()`: TESTMANAGER → `vanessa_manager`, TESTCLIENT → `vanessa_test_client`, RunYaXUnit → `yaxunit_runner`, default → `client`. |
| `src/tunnel.rs` | Generic duplex WS‑pump (`Stream<TextOrClose>` + `Sink<String>`). `RunOutcome { Cancelled / Closed / InboundError / OutboundDropped / SinkError }`. Переполнение очереди 1С НЕ разрывает соединение. |
| `src/reconnect.rs` | Авто‑reconnect с экспоненциальным backoff (`BackoffPolicy { initial, max, multiplier, max_attempts }`). Публикация состояний через `WS_RECONNECT_STATE` (connecting/connected/disconnected/give_up). |
| `src/session_integration.rs` | Высокоуровневый фасад `start/send/shutdown` поверх `tokio_tungstenite`. `WsConnector` + адаптеры `WsStreamAdapter`/`WsSinkAdapter`. Сторона 1С сама шлёт `session.register` после `WS_RECONNECT_STATE=connected` (список tools знает только 1С). |
| `src/session/addin.rs`, `src/session/mod.rs` | FFI‑класс `session` для 1С: методы `start / send / stop / getParams`. |
| `src/system_capability.rs` | JSON‑RPC handlers `addin.spawn` / `addin.kill` поверх `tokio::process::Command` + `nix::signal` (Linux) / `windows-sys` (Windows). Supervisor registry `Arc<Mutex<HashMap<pid, ChildHandle>>>` переживает reconnect, шлёт `addin.child_exited` (ADR‑0027). |
| `src/harness_tests.rs` | End‑to‑end тесты против настоящего WS‑сервера (`tokio_tungstenite::accept_async`): входящие фреймы → `WS_INCOMING`, исходящие через `send()`, `start_correlated`, give‑up по `max_attempts`. |

### Расширение существующих модулей

- `src/lib.rs` — регистрация класса `session`, инициализация `tracing` через `OnceLock` в `GetClassObject` (управляется `WEBTRANSPORT_LOG`, файл по умолчанию `/tmp/web-transport.log`).
- `src/ws_client.rs`, `src/mcp/server.rs` — точечные правки совместимости.
- `Cargo.toml` — добавлены `tokio-tungstenite`, `tracing`, `tracing-subscriber`, `nix` (Linux), `windows-sys` 0.59, `uuid` v4. Bump `windows-sys` потребовал фикса `system_capability.rs:360` (`handle == 0` → `handle.is_null()`).
- `.cargo/config.toml` — настройки кросс‑компиляции (5 целей: Win x32/x64 mingw, Linux x32/x64, macOS x64 zigbuild).

### Системные параметры сессии (ADR‑0029)

`SessionParams` расширен полями `host_id`, `pid`, `capabilities` через trait `HostInfoProvider` (`OsHostInfo`: `V8_HOST_ID` env → `gethostname()` → `"unknown"`). Capabilities `["spawn","kill"]` анонсируются в `session.register` — менеджер использует это для маршрутизации `session.spawn` через данный клиент.

### Override `client_uid` через `/C` (этап 6.6)

Manager‑spawned клиенты получают expected `client_uid` через `/C "client_uid=..."`. Этот override побеждает 1С‑генерируемый UUID в `resolve()` — без него reservation matching по uid в менеджере не работал.

### ADR

- `docs/decisions/0004-mock-strategy-for-addin-host-tests.md` — стратегия мока хоста для unit‑тестов tunnel/reconnect/session_integration.

### Tracing

Структурированное логирование добавлено во все слои: `session::addin` (start/stop/send), `session_integration::WsConnector` (connect_async, статус upgrade), `reconnect` (смена состояния), `addin_host::RealAddinHost` (каждый external_event с preview payload). Уровень — `WEBTRANSPORT_LOG` (default `info`), путь — `WEBTRANSPORT_LOG_FILE`.

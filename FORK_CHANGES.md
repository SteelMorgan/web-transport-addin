# Доработки форка SteelMorgan/web-transport-addin

> Документ описывает доработки относительно upstream-проекта `alkoleft/web-transport-addin`. Форк построен поверх upstream `main` на ревизии 0.6.4 и добавляет 12 коммитов, реализующих транспортный слой для интеграции с **v8-client-session-manager**. Версия компоненты поднята до **0.7.0** (`Manifest.xml`, `Cargo.toml`).

## Назначение доработок

Upstream-компонента предоставляла три класса для 1С: `ws` (WebSocket-клиент), `http` (HTTP/SSE-сервер) и `mcp` (MCP Streamable HTTP-сервер). В форке добавлен четвёртый класс — `session` — реализующий сессионный WS-клиент к внешнему оркестратору `v8-client-session-manager`. Это позволяет агрегировать MCP-tools со многих 1С-клиентов в едином каталоге и публиковать их AI-агенту через единую точку входа.

## Новый класс `AddIn.WebTransport.session`

Полный пайплайн «1С-клиент ↔ session-manager» через WebSocket. Реализован послойно (этапы 5–6), каждый слой покрыт unit-тестами (≥129 lib тестов).

| Файл | Назначение |
|------|-----------|
| `src/addin_host.rs` | trait `AddinHost` + `RealAddinHost` (поверх `addin1c::ExternalEvent`) + `MockAddinHost` для тестов. Изоляция от FFI — позволяет гонять цепочку без живой 1С (ADR-0004). |
| `src/session_params.rs` | Резолвер параметров сессии (`manager_url`, `kind`, `client_uid`, `correlation_id`) по контракту ADR-0020. Эвристика `kind` по `СтрокаЗапуска()`: TESTMANAGER → `vanessa_manager`, TESTCLIENT → `vanessa_test_client`, RunYaXUnit → `yaxunit_runner`, default → `client`. |
| `src/tunnel.rs` | Generic duplex WS-pump (`Stream<TextOrClose>` + `Sink<String>`). `RunOutcome { Cancelled / Closed / InboundError / OutboundDropped / SinkError }`. Переполнение очереди 1С НЕ разрывает соединение. |
| `src/reconnect.rs` | Авто-reconnect с экспоненциальным backoff (`BackoffPolicy { initial, max, multiplier, max_attempts }`). Публикация состояний через `WS_RECONNECT_STATE` (`connecting` / `connected` / `disconnected` / `give_up`). |
| `src/session_integration.rs` | Высокоуровневый фасад `start / send / shutdown` поверх `tokio_tungstenite`. `WsConnector` + адаптеры `WsStreamAdapter` / `WsSinkAdapter`. Сторона 1С сама шлёт `session.register` после `WS_RECONNECT_STATE=connected` (список tools знает только 1С). |
| `src/session/addin.rs`, `src/session/mod.rs` | FFI-класс `session` для 1С: методы `start / send / stop / getParams`. |
| `src/system_capability.rs` | JSON-RPC handlers `addin.spawn` / `addin.kill` поверх `tokio::process::Command` + `nix::signal` (Linux) / `windows-sys` (Windows). Supervisor registry `Arc<Mutex<HashMap<pid, ChildHandle>>>` переживает reconnect, шлёт `addin.child_exited` (ADR-0027). |
| `src/harness_tests.rs` | End-to-end тесты против настоящего WS-сервера (`tokio_tungstenite::accept_async`): входящие фреймы → `WS_INCOMING`, исходящие через `send()`, `start_correlated`, give-up по `max_attempts`. |

## Расширение существующих модулей

- `src/lib.rs` — регистрация класса `session`, инициализация `tracing` через `OnceLock` в `GetClassObject` (управляется `WEBTRANSPORT_LOG`, файл по умолчанию `/tmp/web-transport.log`).
- `src/ws_client.rs`, `src/mcp/server.rs` — точечные правки совместимости.
- `Cargo.toml` — добавлены `tokio-tungstenite`, `tracing`, `tracing-subscriber`, `nix` (Linux), `windows-sys` 0.59, `uuid` v4. Bump `windows-sys` потребовал фикса `system_capability.rs:360` (`handle == 0` → `handle.is_null()`).
- `.cargo/config.toml` — настройки кросс-компиляции (5 целей: Win x32/x64 mingw, Linux x32/x64, macOS x64 zigbuild).

## Системные параметры сессии (ADR-0029)

`SessionParams` расширен полями `host_id`, `pid`, `capabilities` через trait `HostInfoProvider` (`OsHostInfo`: `V8_HOST_ID` env → `gethostname()` → `"unknown"`). Capabilities `["spawn","kill"]` анонсируются в `session.register` — менеджер использует это для маршрутизации `session.spawn` через данный клиент.

## Override `client_uid` через `/C` (этап 6.6)

Manager-spawned клиенты получают expected `client_uid` через `/C "client_uid=..."`. Этот override побеждает 1С-генерируемый UUID в `resolve()` — без него reservation matching по uid в менеджере не работал.

## Tracing

Структурированное логирование добавлено во все слои: `session::addin` (start/stop/send), `session_integration::WsConnector` (connect_async, статус upgrade), `reconnect` (смена состояния), `addin_host::RealAddinHost` (каждый external_event с preview payload). Уровень — `WEBTRANSPORT_LOG` (default `info`), путь — `WEBTRANSPORT_LOG_FILE`.

## ADR

- `docs/decisions/0004-mock-strategy-for-addin-host-tests.md` — стратегия мока хоста для unit-тестов tunnel/reconnect/session_integration.

## Распределение ответственности: установка соединения и пинги

Связано с парным форком `SteelMorgan/onec-client-mcp-devkit` — расширение 1С (BSL) поверх этой компоненты.

### Установка соединения

| Слой | Файл | Что делает |
|------|------|-----------|
| **Rust addin (низ)** | `src/session_integration.rs:145` (`WsConnector::connect`) | `tokio_tungstenite::connect_async(url)` с таймаутом 5 сек, возвращает (Stream, Sink). |
| **Rust addin (orchestrator)** | `src/reconnect.rs` (`run_with_reconnect`) | Auto-reconnect с экспоненциальным backoff. Публикует `WS_RECONNECT_STATE = connecting / connected / disconnected / give_up` в 1С через `AddinHost`. |
| **Rust addin (фасад)** | `src/session_integration.rs:105` (`SessionIntegration::start`) | Высокоуровневый API. Принимает URL → создаёт `WsConnector` → запускает `reconnect`+`tunnel`. |
| **FFI-обёртка** | `src/session/addin.rs:176` | Метод `ЗапуститьСессионнуюИнтеграцию(URL, ClientUID, Kind, CorrelationID)` для 1С. |
| **BSL (точка входа)** | в `onec-client-mcp-devkit/.../Мсп_ТранспортСессионКлиент.Запустить` | Подключает компоненту, создаёт `AddIn.WebTransport.session`, дёргает `Компонента.ЗапуститьСессионнуюИнтеграцию(...)`. Реальное «connected» приходит асинхронно событием `WS_RECONNECT_STATE`. |

### Пинги

Два независимых уровня:

**WebSocket protocol-level ping/pong (RFC 6455):**
- Где: `tokio_tungstenite` обрабатывает их сам внутри `WebSocketStream`.
- Кто инициирует: session-manager (отправляет `Ping` фреймы клиенту по таймеру).
- Кто отвечает: Rust addin → автоматический `Pong` через tokio-tungstenite, в BSL не пробрасывается. См. `session_integration.rs:202-227` (бинарные/ping/pong/frame игнорируются).
- Зачем: keep-alive TCP-соединения, обнаружение разрыва без потери данных.

**Application-level JSON-RPC ping:**
- Где (приём): `onec-client-mcp-devkit/.../Мсп_ТранспортСессионКлиент.Module.bsl:259-262` — обработчик `method = "ping"` отвечает пустым `result`.
- Кто инициирует: session-manager шлёт `{"jsonrpc":"2.0","method":"ping","id":N}`, клиент возвращает пустой `result`.
- Кто отвечает: BSL-расширение (1С), не Rust.
- Зачем: liveness-check на уровне приложения — проверка, что 1С-клиент не завис в обработчике (TCP keep-alive это не покажет).

### session.register после connect

BSL-сторона (`Мсп_ТранспортСессионКлиент.Module.bsl:185-226`) — обработчик `WS_RECONNECT_STATE = connected` сам собирает каталог tools/resources/prompts (snake_case wire format) и шлёт `session.register` менеджеру. Список знает только 1С, поэтому Rust в этом не участвует — комментарий в `session_integration.rs`: *«1С-код сам отвечает за session.register после WS_RECONNECT_STATE=connected»*.

## TL;DR ответственности

- **WS-handshake + auto-reconnect + WS-уровневый ping/pong** → Rust addin (этот репозиторий).
- **Application-level JSON-RPC ping и session.register** → BSL (`SteelMorgan/onec-client-mcp-devkit`).

## Технический долг и план рефакторинга

По итогам архитектурного ревью (диалог 2026-05-04) выявлены нарушения принципа «transport-only» в текущей реализации форка. Полный анализ — в [ADR-0005: Transport-only Rust](docs/decisions/0005-transport-only-rust.md). Парный ADR на стороне прикладного расширения — `onec-client-mcp-devkit/docs/decisions/0003-spawn-tools-in-test-client.md`.

### Зафиксированные нарушения

| # | Нарушение | Где | Серьёзность | План |
|---|-----------|-----|-------------|------|
| 1 | `kind`-эвристика по `СтрокаЗапуска()` (TESTMANAGER → vanessa_manager и т.п.) — прикладной домен в transport-слое | `src/session_params.rs` | средняя — расширяемость | Перенести эвристику в BSL `Мсп_ПараметрыЗапускаКлиент`. В Rust оставить чтение `/C kind=...` без интроспекции. ~45 строк правок. |
| 2 | `addin.spawn` / `addin.kill` JSON-RPC handlers + process supervisor — application capability в transport-компоненте | `src/system_capability.rs`, `src/tunnel.rs::try_dispatch_addin_method` | высокая — концепция | Удалить целиком (~850 строк). Перенести в прикладное расширение `exts/test_client/` репозитория `onec-client-mcp-devkit` как обычные MCP-tools. См. ADR-0005 и парный ADR-0003. |
| 3 | JSON-конверт `correlation_id` в incoming-payload | `src/tunnel.rs::dispatch_incoming_correlated` | низкая | Оставить как есть — необходимый инфраструктурный механизм трассировки реконнектов. |

### Этапы перехода

Поэтапно, синхронизированно с `onec-client-mcp-devkit` и `v8-client-session-manager`:

1. **Этап А (`onec-client-mcp-devkit`):** реализовать `system_spawn_1c_client` / `system_kill_pid` как MCP-tools в `exts/test_client/`. Allow-list + regex-валидация. Старые `addin.spawn` в Rust остаются для обратной совместимости.
2. **Этап Б (`v8-client-session-manager`):** менеджер переключается с `addin.spawn` / `addin.kill` на новые MCP-tools. Жизненный цикл клиента отслеживается heartbeat'ом (timeout на `ping`).
3. **Этап В (этот репозиторий):**
   - Перенести `kind`-эвристику в BSL (нарушение 1) — независимая правка, можно делать раньше.
   - После завершения этапа Б удалить `system_capability.rs`, `try_dispatch_addin_method`, supervisor registry. Зависимости `nix`, части `windows-sys` уйдут.
   - ADR-0027 переводится в `superseded`.
   - ADR-0029 (host_id/pid/capabilities) частично остаётся в силе: `host_id`/`pid` сохраняются в Rust, `capabilities` переходят на сторону BSL.

### Что **не** меняется

- Транспорт (`tunnel`, `reconnect`, `session_integration`) — без изменений.
- FFI-класс `session` (`session/addin.rs`) — без изменений.
- ADR-0004 (mock-strategy) — остаётся в силе.
- correlation_id-конверт (нарушение 3 признано допустимым).

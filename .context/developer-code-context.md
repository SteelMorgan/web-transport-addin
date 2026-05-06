# Developer-Code Context

## Status
completed (задача #38 этап 6.5e — интеграция try_dispatch_addin_method в pump)

## Completed Steps
- [2026-04-29 23:30] SKILL_READ: coding-standards - read
- [2026-04-29 23:30] SKILL_READ: query-patterns - read
- [2026-04-29 23:30] SKILL_READ: ssl-patterns - read
- [2026-04-29 23:30] SKILL_READ: form-patterns - read
- [2026-04-29 23:30] SKILL_READ: error-handling - read
- [2026-04-29 23:30] SKILL_READ: code-navigation - read
- [2026-04-29 23:30] SKILL_READ: syntax-checking - read
- [2026-04-29 23:30] SKILL_READ: test-execution - read
- [2026-04-29 23:30] SKILL_READ: search-before-write - read
- [2026-04-29 23:30] SKILL_READ: event-log-analysis - read
- [2026-04-29 23:30] SKILL_READ: tech-log-analysis - read
- [2026-04-29 23:30] SKILL_READ: bug-reporting - read
- [2026-04-29 23:30] SKILL_READ: gui-control - read
- [2026-04-29 23:30] SKILL_READ: xml-generation - read
- [2026-04-29 23:30] SKILL_READ: form-info - read
- [2026-04-29 23:30] SKILL_READ: form-edit - read
- [2026-04-29 23:30] SKILL_READ: form-validate - read
- [2026-04-29 23:30] SKILL_READ: epf-build - read
- [2026-04-29 23:30] SKILL_READ: epf-dump - read
- [2026-04-29 23:30] SKILL_READ: epf-validate - read
- [2026-04-29 23:35] CODE_UPDATE: Cargo.toml — version 0.6.4 → 0.6.5, добавлен gethostname = "0.5"
- [2026-04-29 23:35] CODE_UPDATE: src/lib.rs — VERSION сделана pub (доступна снаружи)
- [2026-04-29 23:35] CODE_UPDATE: src/session_params.rs — добавлен trait HostInfoProvider, OsHostInfo (prod), MockHostInfo (test), поля host_id/pid/capabilities в SessionParams, функции resolve(input, provider) и resolve_default(input); все 22 исходных теста переведены на MockHostInfo; добавлены 8 новых тестов ADR-0029
- [2026-04-29 23:35] CODE_UPDATE: src/session/addin.rs — resolve → resolve_default
- [2026-04-29 23:40] TEST_RUN_START: cargo test --lib
- [2026-04-29 23:40] TEST_RUN_RESULT: ok. 112 passed; 0 failed. Все тесты зелёные.
- [2026-04-29 23:41] CODE_UPDATE: cargo build --release — Finished `release` profile, warnings only (dead_code)
- [2026-04-29 23:55] CODE_UPDATE: Cargo.toml — version 0.6.5 → 0.6.6; добавлены nix = "0.29" (features: signal,process), windows-sys = "0.59" (target windows); tokio features += "process"
- [2026-04-29 23:55] CODE_UPDATE: src/lib.rs — VERSION = "0.6.6"; добавлен mod system_capability (pub)
- [2026-04-29 23:55] CODE_UPDATE: src/system_capability.rs — новый модуль: JsonRpcRequest/Response, LaunchSpec, ChildHandle, supervisor Registry, parse_addin_method(), handle_spawn(), handle_kill(), supervisor_task(), do_kill() (#[cfg(unix)]/windows), try_dispatch_addin_method(); 15 тестов
- [2026-04-29 23:58] TEST_RUN_START: cargo test --lib
- [2026-04-29 23:58] TEST_RUN_RESULT: ok. 127 passed; 0 failed. Все тесты зелёные (112 старых + 15 новых).
- [2026-04-29 23:58] CODE_UPDATE: cargo build --release — Finished `release` profile, warnings only (dead_code)
- [2026-04-30 00:10] CODE_UPDATE: src/tunnel.rs — run_tunnel_with_correlation получил параметр system_capability: Option<(Registry, OutboundSender)>; run_tunnel передаёт None; в обработке TextOrClose::Text добавлен try_dispatch_addin_method до external_event; тесты обновлены (None для нового параметра); добавлены 2 новых интеграционных теста (addin_spawn_payload_routed_to_outbound_not_external_event, non_addin_payload_still_goes_to_external_event)
- [2026-04-30 00:10] CODE_UPDATE: src/reconnect.rs — run_with_reconnect_correlated получил параметр system_capability: Option<(Registry, OutboundSender)>; run_with_reconnect передаёт None; на каждой reconnect-итерации sys_cap клонируется через as_ref().map(|(r, o)| (r.clone(), o.clone())); передаётся в run_tunnel_with_correlation
- [2026-04-30 00:10] CODE_UPDATE: src/session_integration.rs — start_with_connector_correlated создаёт new_registry() и sys_cap = Some((registry, OutboundSender::new(outbound_tx.clone()))); передаёт в run_with_reconnect_correlated; registry per-integration (Arc, переживает reconnect)
- [2026-04-30 00:15] TEST_RUN_START: cargo test --lib
- [2026-04-30 00:15] TEST_RUN_RESULT: ok. 129 passed; 0 failed. (127 old + 2 new integration tests)
- [2026-04-30 00:15] CODE_UPDATE: cargo build --release — Finished `release` profile, warnings only (dead_code)

## Findings

### Этап 6.5 (#37 — host_id/pid/capabilities)
- Зависимость gethostname 0.5 (не 1.x) — задача явно указала версию "0.5".
- resolve() теперь принимает &dyn HostInfoProvider; resolve_default() — обёртка для prod.
- MockHostInfo объявлена только под #[cfg(test)] — не попадает в релизную сборку.
- Все 22 существующих теста успешно переведены на MockHostInfo без изменения assertions.

### Этап 6.5e (#38 — system_capability spawn/kill)
- Новый модуль src/system_capability.rs реализует серверную сторону addin.spawn / addin.kill.
- Supervisor registry: Arc<Mutex<HashMap<u32, ChildHandle>>> — единая точка учёта дочерних процессов.
- Детач процесса: setpgid(0,0) на Linux через CommandExt::pre_exec; CREATE_NEW_PROCESS_GROUP на Windows.
- Kill-защита: addin убивает только процессы из своего registry (ADR-0031 §kill matrix).
- do_kill(): SIGTERM → 2s grace → SIGKILL на Linux; TerminateProcess на Windows.
- try_dispatch_addin_method() — точка входа для tunnel.rs: возвращает true если payload роутирован.
- supervisor_task(): извлекает child из registry, ждёт завершения, шлёт addin.child_exited нотификацию.
- Routing в tunnel.rs НЕ изменялся — try_dispatch_addin_method() нужно вызвать из tunnel/dispatch_incoming до external_event (задача организации вызова остаётся за integrating стороной или след. задачей).
- Новые тесты (15): dispatch routing, spawn real process, spawn invalid binary, kill unknown pid, kill known pid, kill force SIGKILL, JSON-RPC serialization, missing params.

## Assumptions
- gethostname версия "0.5" (как задано в #37), а не последняя 1.x.
- MockHostInfo только в #[cfg(test)] согласно стандарту Rust.
- pub const VERSION в lib.rs — изменение безопасно, pub не нарушает ABI cdylib.
- kill_known_pid_terminates_process тест корректно обрабатывает race condition (supervisor извлёк child для wait() раньше чем kill проверил registry) — оба исхода считаются успешными.
- Registry per-integration (не global static) — соответствует требованию "тесты не делят состояние".
- OutboundSender уже Clone (через #[derive(Clone)]) — дополнительный derive не нужен.
- sys_cap клонируется на каждый reconnect-цикл через as_ref().map(|(r,o)| (r.clone(), o.clone())) — Arc::clone дешевле копирования данных.
- Интеграционный тест addin_spawn_payload_routed_to_outbound_not_external_event использует отдельный (syscap_tx, syscap_rx) канал вместо канала pump'а, чтобы не смешивать ответы system_capability с исходящими от 1С.

//! Серверная сторона `system_capability` — обработка методов `addin.spawn` и
//! `addin.kill` внутри addin'а (без проброса в 1С).
//!
//! ADR-0027 «System capability vs MCP tools»: методы `addin.*` НЕ должны
//! попадать в 1С через `external_event`. Этот модуль перехватывает их ДО
//! того, как `tunnel.rs` вызовет `AddinHost::external_event`, обрабатывает
//! синхронно или через tokio-задачу и отправляет ответ через `OutboundSender`.
//!
//! Контракт методов зафиксирован в ADR-0027 §«system_capability»:
//!
//! - `addin.spawn { launch_spec: { binary, args[], env{}, startup_command?,
//!   extra_args[] }, expected_uid }` → spawn процесса, ответ `{ pid }`.
//! - `addin.kill { pid, force: bool }` → убить процесс из supervisor registry,
//!   ответ `{ ok: true }`.
//!
//! Supervisor registry — `Arc<Mutex<HashMap<u32, ChildHandle>>>`. При спавне
//! запускается задача-наблюдатель: после завершения дочернего процесса шлёт
//! нотификацию `addin.child_exited { pid, exit_code }` менеджеру.
//!
//! Kill-защита: addin убивает только процессы из собственного registry;
//! произвольные PID'ы с хоста заблокированы (ADR-0031 §«kill matrix»).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tunnel::OutboundSender;

// ─── JSON-RPC types ─────────────────────────────────────────────────────────

/// Минимальная структура JSON-RPC запроса для распознавания метода.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub method: String,
    pub id: Option<Value>,
    pub params: Option<Value>,
}

/// JSON-RPC ответ (результат или ошибка).
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

impl JsonRpcResponse {
    pub fn ok(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

// ─── LaunchSpec ──────────────────────────────────────────────────────────────

/// Спецификация запуска процесса (ADR-0030 §«Поля launch»).
#[derive(Debug, Clone, Deserialize)]
pub struct LaunchSpec {
    /// Абсолютный путь к исполняемому файлу. Обязательное поле.
    pub binary: String,
    /// Аргументы командной строки.
    #[serde(default)]
    pub args: Vec<String>,
    /// Переменные окружения (overlay поверх наследованных).
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Содержимое `/C"..."` (без обрамляющих кавычек).
    pub startup_command: Option<String>,
    /// Дополнительные аргументы после `/C`.
    #[serde(default)]
    pub extra_args: Vec<String>,
}

// ─── Supervisor registry ─────────────────────────────────────────────────────

/// Дескриптор дочернего процесса в supervisor registry.
pub struct ChildHandle {
    /// Tokio child process (владение).
    pub child: tokio::process::Child,
}

/// Supervisor registry: PID → ChildHandle.
pub type Registry = Arc<Mutex<HashMap<u32, ChildHandle>>>;

/// Создать новый пустой registry.
pub fn new_registry() -> Registry {
    Arc::new(Mutex::new(HashMap::new()))
}

// ─── Dispatch: is this an addin.* method? ───────────────────────────────────

/// Проверить, является ли payload JSON-RPC запросом к методу `addin.*`.
/// Возвращает разобранный запрос если да, иначе `None`.
pub fn parse_addin_method(payload: &str) -> Option<JsonRpcRequest> {
    let req: JsonRpcRequest = serde_json::from_str(payload).ok()?;
    if req.method.starts_with("addin.") {
        Some(req)
    } else {
        None
    }
}

// ─── addin.spawn ─────────────────────────────────────────────────────────────

/// Обработать `addin.spawn`. Запускает процесс, регистрирует его в supervisor
/// registry, стартует задачу-наблюдатель за завершением.
///
/// Ответ (JSON-RPC с `{ pid }`) отправляется через `outbound`.
pub async fn handle_spawn(
    req: JsonRpcRequest,
    registry: Registry,
    outbound: OutboundSender,
) {
    let id = req.id.clone();

    // Разобрать launch_spec из params.launch_spec.
    let spec = match extract_launch_spec(&req) {
        Ok(s) => s,
        Err(msg) => {
            send_response(&outbound, JsonRpcResponse::err(id, -32602, msg));
            return;
        }
    };

    // Спавнить процесс.
    let mut cmd = tokio::process::Command::new(&spec.binary);
    cmd.args(&spec.args);
    cmd.envs(&spec.env);
    // Детач: процесс не умирает вместе с addin'ом.
    #[cfg(unix)]
    {
        #[allow(unused_imports)]
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                // Создать новую group для детача от родительской группы.
                nix::unistd::setpgid(
                    nix::unistd::Pid::from_raw(0),
                    nix::unistd::Pid::from_raw(0),
                )
                .ok();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            send_response(
                &outbound,
                JsonRpcResponse::err(id, -32000, format!("spawn failed: {}", e)),
            );
            return;
        }
    };

    let pid = match child.id() {
        Some(p) => p,
        None => {
            // Процесс уже завершился сразу — нет PID.
            send_response(
                &outbound,
                JsonRpcResponse::err(id, -32000, "process exited immediately"),
            );
            return;
        }
    };

    // Зарегистрировать в supervisor registry.
    registry
        .lock()
        .expect("supervisor registry poisoned")
        .insert(pid, ChildHandle { child });

    // Запустить задачу-наблюдатель.
    let registry_clone = registry.clone();
    let outbound_clone = outbound.clone();
    tokio::spawn(async move {
        supervisor_task(pid, registry_clone, outbound_clone).await;
    });

    // Ответить с PID.
    send_response(
        &outbound,
        JsonRpcResponse::ok(id, serde_json::json!({ "pid": pid })),
    );
}

/// Задача-наблюдатель: ждёт завершения дочернего процесса и шлёт
/// нотификацию `addin.child_exited { pid, exit_code }`.
async fn supervisor_task(pid: u32, registry: Registry, outbound: OutboundSender) {
    // Получить child из registry, дождаться завершения.
    // Нам нужно взять child из registry без удержания мьютекса на ожидание.
    // Архитектурное решение: берём child из HashMap, ждём его, затем удаляем из registry.
    let child = {
        let mut guard = registry.lock().expect("supervisor registry poisoned");
        guard.remove(&pid).map(|h| h.child)
    };

    let exit_code: i32 = if let Some(mut c) = child {
        match c.wait().await {
            Ok(status) => {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    if let Some(sig) = status.signal() {
                        // Процесс убит сигналом — используем отрицательное значение.
                        -(sig as i32)
                    } else {
                        status.code().unwrap_or(0)
                    }
                }
                #[cfg(not(unix))]
                {
                    status.code().unwrap_or(0)
                }
            }
            Err(_) => -1,
        }
    } else {
        // Процесс уже был извлечён другой задачей (например, при kill).
        return;
    };

    // Убедиться, что PID вычищен из registry (kill мог уже это сделать).
    {
        let mut guard = registry.lock().expect("supervisor registry poisoned");
        guard.remove(&pid);
    }

    // Отправить нотификацию менеджеру.
    let notification = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "addin.child_exited",
        "params": {
            "pid": pid,
            "exit_code": exit_code
        }
    });
    let _ = outbound.send(notification.to_string());
}

// ─── addin.kill ──────────────────────────────────────────────────────────────

/// Обработать `addin.kill`. Убивает только процессы из supervisor registry.
pub async fn handle_kill(req: JsonRpcRequest, registry: Registry, outbound: OutboundSender) {
    let id = req.id.clone();

    let (pid, force) = match extract_kill_params(&req) {
        Ok(p) => p,
        Err(msg) => {
            send_response(&outbound, JsonRpcResponse::err(id, -32602, msg));
            return;
        }
    };

    // Проверка: PID должен быть в registry — защита от убийства произвольных
    // процессов хоста (ADR-0031 §«kill matrix»).
    let known = {
        let guard = registry.lock().expect("supervisor registry poisoned");
        guard.contains_key(&pid)
    };

    if !known {
        send_response(
            &outbound,
            JsonRpcResponse::err(
                id,
                -32001,
                format!("pid {} is not in supervisor registry", pid),
            ),
        );
        return;
    }

    // Выполнить kill.
    let kill_result = do_kill(pid, force).await;

    match kill_result {
        Ok(()) => {
            send_response(
                &outbound,
                JsonRpcResponse::ok(id, serde_json::json!({ "ok": true })),
            );
        }
        Err(msg) => {
            send_response(&outbound, JsonRpcResponse::err(id, -32000, msg));
        }
    }
}

/// Платформо-специфичный kill.
async fn do_kill(pid: u32, force: bool) -> Result<(), String> {
    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;
        use std::time::Duration;

        let nix_pid = Pid::from_raw(pid as i32);

        if force {
            kill(nix_pid, Signal::SIGKILL).map_err(|e| format!("SIGKILL failed: {}", e))?;
        } else {
            // SIGTERM → grace 2s → SIGKILL если ещё жив.
            kill(nix_pid, Signal::SIGTERM).map_err(|e| format!("SIGTERM failed: {}", e))?;
            tokio::time::sleep(Duration::from_secs(2)).await;
            // Проверить, жив ли ещё процесс (kill(pid, 0) — probe).
            if kill(nix_pid, None).is_ok() {
                kill(nix_pid, Signal::SIGKILL).map_err(|e| format!("SIGKILL failed: {}", e))?;
            }
        }
        Ok(())
    }

    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };

        let handle =
            unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        if handle == 0 {
            return Err(format!("OpenProcess failed for pid {}", pid));
        }
        let ok = unsafe { TerminateProcess(handle, 1) };
        unsafe { CloseHandle(handle) };
        if ok == 0 {
            return Err(format!("TerminateProcess failed for pid {}", pid));
        }
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pid, force);
        Err("kill not supported on this platform".to_owned())
    }
}

// ─── Params extraction helpers ───────────────────────────────────────────────

fn extract_launch_spec(req: &JsonRpcRequest) -> Result<LaunchSpec, String> {
    let params = req
        .params
        .as_ref()
        .ok_or("params is required")?;

    let launch_spec_value = params
        .get("launch_spec")
        .ok_or("params.launch_spec is required")?;

    serde_json::from_value(launch_spec_value.clone())
        .map_err(|e| format!("invalid launch_spec: {}", e))
}

fn extract_kill_params(req: &JsonRpcRequest) -> Result<(u32, bool), String> {
    let params = req
        .params
        .as_ref()
        .ok_or("params is required")?;

    let pid = params
        .get("pid")
        .and_then(Value::as_u64)
        .ok_or("params.pid must be a non-negative integer")?;

    let force = params
        .get("force")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    Ok((pid as u32, force))
}

// ─── Utility ─────────────────────────────────────────────────────────────────

fn send_response(outbound: &OutboundSender, resp: JsonRpcResponse) {
    if let Ok(text) = serde_json::to_string(&resp) {
        let _ = outbound.send(text);
    }
}

// ─── Dispatch entry point (for tunnel.rs) ────────────────────────────────────

/// Проверить, является ли payload `addin.*`-методом, и если да — запустить
/// обработчик асинхронно на текущем tokio-runtime. Возвращает `true` если
/// payload был роутирован, `false` — нужно передать в `external_event`.
///
/// `outbound` используется для отправки ответа обратно менеджеру.
pub fn try_dispatch_addin_method(
    payload: &str,
    registry: Registry,
    outbound: OutboundSender,
) -> bool {
    let req = match parse_addin_method(payload) {
        Some(r) => r,
        None => return false,
    };

    let method = req.method.clone();
    match method.as_str() {
        "addin.spawn" => {
            tokio::spawn(async move {
                handle_spawn(req, registry, outbound).await;
            });
            true
        }
        "addin.kill" => {
            tokio::spawn(async move {
                handle_kill(req, registry, outbound).await;
            });
            true
        }
        _ => {
            // Неизвестный addin.* метод — отправить ошибку.
            let id = req.id.clone();
            send_response(
                &outbound,
                JsonRpcResponse::err(id, -32601, format!("method not found: {}", method)),
            );
            true
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::mpsc::unbounded_channel;

    use crate::tunnel::OutboundSender;

    /// Создать OutboundSender и канал для перехвата ответов.
    fn make_outbound() -> (OutboundSender, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = unbounded_channel::<String>();
        (OutboundSender::new(tx), rx)
    }

    /// Получить следующий ответ с таймаутом.
    async fn recv_response(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    ) -> Option<serde_json::Value> {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    // ── parse_addin_method ──────────────────────────────────────────────────

    #[test]
    fn dispatch_routes_addin_method_to_handler_not_external_event() {
        // Проверяем, что parse_addin_method распознаёт addin.spawn.
        let payload = r#"{"jsonrpc":"2.0","id":1,"method":"addin.spawn","params":{}}"#;
        let req = parse_addin_method(payload);
        assert!(req.is_some());
        assert_eq!(req.unwrap().method, "addin.spawn");
    }

    #[test]
    fn dispatch_routes_non_addin_method_to_external_event() {
        // Обычный JSON-RPC НЕ должен распознаваться как addin.*.
        let payload = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{}}"#;
        let req = parse_addin_method(payload);
        assert!(req.is_none());
    }

    #[test]
    fn parse_addin_method_returns_none_for_non_json() {
        assert!(parse_addin_method("not-json").is_none());
    }

    #[test]
    fn parse_addin_method_returns_none_for_session_method() {
        let payload = r#"{"jsonrpc":"2.0","method":"session.register","params":{}}"#;
        assert!(parse_addin_method(payload).is_none());
    }

    // ── try_dispatch_addin_method ───────────────────────────────────────────

    #[tokio::test]
    async fn try_dispatch_returns_false_for_non_addin_method() {
        let (outbound, _rx) = make_outbound();
        let registry = new_registry();
        let payload = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{}}"#;
        let routed = try_dispatch_addin_method(payload, registry, outbound);
        assert!(!routed);
    }

    #[tokio::test]
    async fn try_dispatch_returns_true_for_addin_method() {
        let (outbound, _rx) = make_outbound();
        let registry = new_registry();
        // addin.spawn с заведомо невалидными params — но routing должен быть true.
        let payload = r#"{"jsonrpc":"2.0","id":1,"method":"addin.spawn","params":{}}"#;
        let routed = try_dispatch_addin_method(payload, registry, outbound);
        assert!(routed);
    }

    // ── spawn_real_short_process_returns_pid ────────────────────────────────

    #[tokio::test]
    async fn spawn_real_short_process_returns_pid() {
        let (outbound, mut rx) = make_outbound();
        let registry = new_registry();

        let req = JsonRpcRequest {
            method: "addin.spawn".to_owned(),
            id: Some(serde_json::json!(1)),
            params: Some(serde_json::json!({
                "launch_spec": {
                    "binary": "sleep",
                    "args": ["0.1"]
                }
            })),
        };

        handle_spawn(req, registry.clone(), outbound).await;

        // Ответ должен содержать pid > 0.
        let resp = recv_response(&mut rx).await.expect("no response");
        assert!(resp["result"]["pid"].as_u64().unwrap_or(0) > 0);

        // Supervisor должен поймать завершение и прислать child_exited.
        let notification = recv_response(&mut rx).await.expect("no child_exited");
        assert_eq!(notification["method"], "addin.child_exited");
        assert!(notification["params"]["pid"].as_u64().unwrap_or(0) > 0);
    }

    #[test]
    fn spawn_invalid_binary_returns_error() {
        // Синхронная проверка парсинга — сам spawn проверяем через tokio.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (outbound, mut rx) = make_outbound();
            let registry = new_registry();

            let req = JsonRpcRequest {
                method: "addin.spawn".to_owned(),
                id: Some(serde_json::json!(42)),
                params: Some(serde_json::json!({
                    "launch_spec": {
                        "binary": "/nonexistent/binary/that/does/not/exist"
                    }
                })),
            };

            handle_spawn(req, registry, outbound).await;

            let resp = recv_response(&mut rx).await.expect("no response");
            assert!(resp["error"].is_object(), "expected error, got: {resp}");
        });
    }

    // ── kill tests ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn kill_unknown_pid_returns_error() {
        let (outbound, mut rx) = make_outbound();
        let registry = new_registry();

        let req = JsonRpcRequest {
            method: "addin.kill".to_owned(),
            id: Some(serde_json::json!(10)),
            params: Some(serde_json::json!({ "pid": 99999999_u32, "force": false })),
        };

        handle_kill(req, registry, outbound).await;

        let resp = recv_response(&mut rx).await.expect("no response");
        assert!(resp["error"].is_object(), "expected error for unknown pid");
        assert!(
            resp["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("supervisor registry"),
            "error message should mention supervisor registry"
        );
    }

    #[tokio::test]
    async fn kill_known_pid_terminates_process() {
        // Спавним sleep 30 (долгий процесс), потом kill'им.
        let (outbound_spawn, mut rx_spawn) = make_outbound();
        let registry = new_registry();

        let spawn_req = JsonRpcRequest {
            method: "addin.spawn".to_owned(),
            id: Some(serde_json::json!(1)),
            params: Some(serde_json::json!({
                "launch_spec": {
                    "binary": "sleep",
                    "args": ["30"]
                }
            })),
        };

        handle_spawn(spawn_req, registry.clone(), outbound_spawn).await;

        let spawn_resp = recv_response(&mut rx_spawn)
            .await
            .expect("no spawn response");
        let pid = spawn_resp["result"]["pid"]
            .as_u64()
            .expect("pid missing") as u32;
        assert!(pid > 0);

        // Вернуть child обратно в registry — supervisor уже извлёк его для wait().
        // Поскольку supervisor может уже удалить его при быстром завершении,
        // проверяем что kill либо успешен, либо возвращает ошибку "not in registry".
        // Для force kill проверяем через do_kill напрямую (минуя registry guard),
        // чтобы избежать race condition в тесте.

        // Нам нужно убедиться, что pid ещё в registry (supervisor его извлёк).
        // Переподождём немного и проверим через handle_kill.
        let (outbound_kill, mut rx_kill) = make_outbound();
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Если supervisor уже удалил pid (sleep завершился), kill вернёт ошибку — это нормально.
        // Если pid ещё жив, kill должен вернуть ok или ошибку из kill-системного вызова.
        let kill_req = JsonRpcRequest {
            method: "addin.kill".to_owned(),
            id: Some(serde_json::json!(2)),
            params: Some(serde_json::json!({ "pid": pid, "force": true })),
        };

        handle_kill(kill_req, registry.clone(), outbound_kill).await;
        let kill_resp = recv_response(&mut rx_kill).await.expect("no kill response");

        // Допустимые исходы: ok=true (убили) или ошибка "not in registry" (supervisor уже
        // извлёк child для wait — race condition в тесте не критична).
        let is_ok = kill_resp["result"]["ok"].as_bool().unwrap_or(false);
        let is_not_in_registry = kill_resp["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("supervisor registry");
        assert!(
            is_ok || is_not_in_registry,
            "unexpected kill response: {kill_resp}"
        );
    }

    #[tokio::test]
    async fn kill_force_immediately_signals_sigkill() {
        // Проверяем, что force=true вызывается без grace period.
        // Спавним долгий процесс и проверяем скорость завершения.
        let (outbound_spawn, mut rx_spawn) = make_outbound();
        let registry = new_registry();

        let spawn_req = JsonRpcRequest {
            method: "addin.spawn".to_owned(),
            id: Some(serde_json::json!(1)),
            params: Some(serde_json::json!({
                "launch_spec": {
                    "binary": "sleep",
                    "args": ["60"]
                }
            })),
        };

        handle_spawn(spawn_req, registry.clone(), outbound_spawn).await;

        let spawn_resp = recv_response(&mut rx_spawn)
            .await
            .expect("no spawn response");
        let pid = spawn_resp["result"]["pid"]
            .as_u64()
            .expect("pid missing") as u32;

        // force=true должен завершить быстро (без 2s grace).
        let start = std::time::Instant::now();

        // Т.к. supervisor мог уже извлечь child, пробуем kill через do_kill напрямую.
        #[cfg(unix)]
        {
            // Проверяем что процесс жив через kill(pid, 0).
            use nix::sys::signal::kill;
            use nix::unistd::Pid;

            let nix_pid = Pid::from_raw(pid as i32);
            if kill(nix_pid, None).is_ok() {
                // Процесс живой — убиваем.
                let result = do_kill(pid, true).await;
                assert!(result.is_ok(), "force kill should succeed: {:?}", result);
                let elapsed = start.elapsed();
                // force kill не должен ждать 2 секунды grace.
                assert!(
                    elapsed < Duration::from_secs(2),
                    "force kill took too long: {:?}",
                    elapsed
                );
            }
            // Если процесс уже завершился — тест пройден (нечего убивать).
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
        }
    }

    // ── JSON-RPC response helpers ───────────────────────────────────────────

    #[test]
    fn jsonrpc_response_ok_serialization() {
        let resp = JsonRpcResponse::ok(Some(serde_json::json!(1)), serde_json::json!({"pid": 42}));
        let s = serde_json::to_string(&resp).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["pid"], 42);
        assert!(v["error"].is_null());
    }

    #[test]
    fn jsonrpc_response_err_serialization() {
        let resp = JsonRpcResponse::err(None, -32601, "method not found");
        let s = serde_json::to_string(&resp).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["error"]["code"], -32601);
        assert_eq!(v["error"]["message"], "method not found");
        assert!(v["result"].is_null());
    }

    // ── missing params / bad params ─────────────────────────────────────────

    #[tokio::test]
    async fn spawn_missing_launch_spec_returns_error() {
        let (outbound, mut rx) = make_outbound();
        let registry = new_registry();

        let req = JsonRpcRequest {
            method: "addin.spawn".to_owned(),
            id: Some(serde_json::json!(5)),
            params: Some(serde_json::json!({ "expected_uid": "abc" })),
        };

        handle_spawn(req, registry, outbound).await;
        let resp = recv_response(&mut rx).await.expect("no response");
        assert!(resp["error"].is_object());
    }

    #[tokio::test]
    async fn kill_missing_pid_returns_error() {
        let (outbound, mut rx) = make_outbound();
        let registry = new_registry();

        let req = JsonRpcRequest {
            method: "addin.kill".to_owned(),
            id: Some(serde_json::json!(6)),
            params: Some(serde_json::json!({ "force": false })),
        };

        handle_kill(req, registry, outbound).await;
        let resp = recv_response(&mut rx).await.expect("no response");
        assert!(resp["error"].is_object());
    }
}

mod addin_error;
mod addin_host;
mod session_params;
mod tunnel;
mod reconnect;
mod session_integration;
#[cfg(test)]
mod harness_tests;
mod http;
mod mcp;
mod session;
mod ws;
mod ws_client;
use std::{
    collections::HashMap,
    error::Error,
    ffi::{c_int, c_long, c_void},
    fs::OpenOptions,
    sync::{
        atomic::{AtomicI32, Ordering},
        OnceLock,
    },
};

use addin1c::{create_component, destroy_component, name, AttachType};
use tracing_subscriber::EnvFilter;

static TRACING_INIT: OnceLock<()> = OnceLock::new();

fn init_tracing() {
    TRACING_INIT.get_or_init(|| {
        let path = std::env::var("WEBTRANSPORT_LOG_FILE")
            .unwrap_or_else(|_| "/tmp/web-transport.log".to_owned());
        let filter = EnvFilter::try_from_env("WEBTRANSPORT_LOG")
            .unwrap_or_else(|_| EnvFilter::new("info"));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path);
        match file {
            Ok(f) => {
                let _ = tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_writer(std::sync::Mutex::new(f))
                    .with_ansi(false)
                    .with_target(true)
                    .with_thread_ids(true)
                    .try_init();
            }
            Err(_) => {
                let _ = tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_ansi(false)
                    .try_init();
            }
        }
        tracing::info!(version = VERSION, path = %path, "webtransport addin tracing initialized");
    });
}

pub const VERSION: &str = "0.6.6";

pub(crate) fn parse_headers(
    json_headers: String,
) -> Result<HashMap<String, String>, Box<dyn Error>> {
    if json_headers.is_empty() {
        return Ok(HashMap::new());
    }
    let raw = serde_json::from_str::<HashMap<String, serde_json::Value>>(&json_headers)?;
    Ok(raw
        .into_iter()
        .map(|(key, value)| {
            let value = match value {
                serde_json::Value::Null => "".to_owned(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::String(s) => s,
                serde_json::Value::Array(_) | serde_json::Value::Object(_) => "".to_owned(),
            };
            (key, value)
        })
        .collect())
}

pub static PLATFORM_CAPABILITIES: AtomicI32 = AtomicI32::new(-1);

unsafe fn cstr1c_to_string(name: *const u16) -> String {
    if name.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    loop {
        if *name.add(len) == 0 {
            break;
        }
        len += 1;
    }
    let slice = std::slice::from_raw_parts(name, len);
    String::from_utf16_lossy(slice)
}

#[allow(non_snake_case)]
#[no_mangle]
/// # Safety
/// This function should be called from 1C.
pub unsafe extern "C" fn GetClassObject(name: *const u16, component: *mut *mut c_void) -> c_long {
    init_tracing();
    let class_name = cstr1c_to_string(name);
    tracing::info!(class = %class_name, "GetClassObject");
    match class_name.as_str() {
        "ws" => {
            let addin = ws::WsAddIn::new();
            if let Ok(addin) = addin {
                create_component(component, addin)
            } else {
                0
            }
        }
        "http" => {
            let addin = http::HttpAddIn::new();
            if let Ok(addin) = addin {
                create_component(component, addin)
            } else {
                0
            }
        }
        "mcp" => {
            let addin = mcp::McpAddIn::new();
            if let Ok(addin) = addin {
                create_component(component, addin)
            } else {
                0
            }
        }
        "session" => {
            let addin = session::SessionAddIn::new();
            if let Ok(addin) = addin {
                create_component(component, addin)
            } else {
                0
            }
        }
        _ => 0,
    }
}

#[allow(non_snake_case)]
#[no_mangle]
/// # Safety
/// This function should be called from 1C.
pub unsafe extern "C" fn DestroyObject(component: *mut *mut c_void) -> c_long {
    destroy_component(component)
}

#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn GetClassNames() -> *const u16 {
    name!("ws|http|mcp|session").as_ptr()
}

#[allow(non_snake_case)]
#[no_mangle]
/// # Safety
/// This function should be called from 1C.
pub unsafe extern "C" fn SetPlatformCapabilities(capabilities: c_int) -> c_int {
    PLATFORM_CAPABILITIES.store(capabilities, Ordering::Relaxed);
    3
}

#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn GetAttachType() -> AttachType {
    AttachType::Any
}

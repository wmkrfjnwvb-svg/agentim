//! The C ABI (`include/agentim.h`): JSON in, JSON out, one callback.
//! Every entry point catches panics; nothing unwinds into the caller.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use agentim_client_core::Config;
use serde_json::Value;

use crate::runtime::Client;

/// Called with each event (a NUL-terminated JSON object), on the runtime's
/// thread; the string is only valid during the call.
pub type EventCallback = extern "C" fn(ctx: *mut c_void, json: *const c_char);

struct Ctx(*mut c_void);
// The caller promises the context may be used from the runtime's thread.
unsafe impl Send for Ctx {}

fn str_arg<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(p) }.to_str().ok()
}

/// Starts a client. `config_json`: `{"url", "device_id", "device_name"?,
/// "platform"?, "client_name"?, "client_version"?}`; `store_dir`: where the
/// cache lives (NULL: in memory). Returns NULL on bad arguments.
#[unsafe(no_mangle)]
pub extern "C" fn agentim_client_new(
    config_json: *const c_char,
    store_dir: *const c_char,
    callback: EventCallback,
    ctx: *mut c_void,
) -> *mut Client {
    catch_unwind(|| {
        let cfg: Config = serde_json::from_str(str_arg(config_json)?).ok()?;
        let dir = str_arg(store_dir).map(PathBuf::from);
        let ctx = Ctx(ctx);
        let client = Client::start(
            cfg,
            dir,
            Box::new(move |v: &Value| {
                let ctx = &ctx;
                if let Ok(s) = CString::new(v.to_string()) {
                    callback(ctx.0, s.as_ptr());
                }
            }),
        );
        Some(Box::into_raw(Box::new(client)))
    })
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// Sends a command (`{"cmd": …}`); the answer is an event
/// `{"type": "reply", "id": id, "ok": …}`. Returns 0, or -1 on bad JSON.
#[unsafe(no_mangle)]
pub extern "C" fn agentim_client_command(
    c: *mut Client,
    id: u64,
    cmd_json: *const c_char,
) -> c_int {
    catch_unwind(AssertUnwindSafe(|| {
        let (Some(c), Some(s)) = (unsafe { c.as_ref() }, str_arg(cmd_json)) else {
            return -1;
        };
        match serde_json::from_str::<Value>(s) {
            Ok(v) if v.is_object() => {
                c.command(id, v);
                0
            }
            _ => -1,
        }
    }))
    .unwrap_or(-1)
}

/// The answer to a `need_token` event: a token (`refused` 0), or NULL with
/// `refused` 1 (the backend says no: stop) or 0 (it failed: try later).
#[unsafe(no_mangle)]
pub extern "C" fn agentim_client_token(c: *mut Client, token: *const c_char, refused: c_int) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(c) = unsafe { c.as_ref() } {
            c.token(match str_arg(token) {
                Some(t) => Ok(t.to_string()),
                None => Err(refused != 0),
            });
        }
    }));
}

/// The platform's view of the network: up or not, and a number that
/// changes whenever the route may have changed.
#[unsafe(no_mangle)]
pub extern "C" fn agentim_client_network(c: *mut Client, online: c_int, generation: u64) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(c) = unsafe { c.as_ref() } {
            c.network(online != 0, generation);
        }
    }));
}

#[unsafe(no_mangle)]
pub extern "C" fn agentim_client_foreground(c: *mut Client, foreground: c_int) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(c) = unsafe { c.as_ref() } {
            c.foreground(foreground != 0);
        }
    }));
}

/// Stops the client and frees it; no callback runs after it returns.
#[unsafe(no_mangle)]
pub extern "C" fn agentim_client_free(c: *mut Client) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !c.is_null() {
            drop(unsafe { Box::from_raw(c) });
        }
    }));
}

/// The SDK version (static string).
#[unsafe(no_mangle)]
pub extern "C" fn agentim_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

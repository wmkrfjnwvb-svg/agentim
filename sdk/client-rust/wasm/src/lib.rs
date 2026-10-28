//! The core for the browser (docs/12 §2.12): a plain wasm ABI, JSON in and
//! out through linear memory, driven by `sdk/client-web`. No bindgen: the
//! glue is a few lines of TypeScript and the module stays small.
//!
//! - `agentim_alloc(len) -> ptr`, `agentim_dealloc(ptr, len)`;
//! - `agentim_new(cfg_ptr, cfg_len) -> handle` (0: bad config);
//! - `agentim_handle(handle, input_ptr, input_len, now_ms) -> u64`: the
//!   effects (a JSON array, see `agentim_client_core::wire`) as
//!   `ptr << 32 | len`, to be freed with `agentim_dealloc`;
//! - `agentim_next_deadline(handle) -> f64` (ms since 1970, or -1);
//! - `agentim_free(handle)`;
//! - `agentim_strip_metadata(ptr, len) -> u64`: the image without EXIF and
//!   XMP (`media::strip_metadata`) as `ptr << 32 | len`, or 0 when there
//!   was nothing to remove.

use std::cell::RefCell;

use agentim_client_core::{Config, Core, media, wire};
use serde_json::Value;

thread_local! {
    static CORES: RefCell<Vec<Option<Core>>> = const { RefCell::new(Vec::new()) };
}

#[unsafe(no_mangle)]
pub extern "C" fn agentim_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// # Safety
/// `ptr` and `len` come from `agentim_alloc` or an effects result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentim_dealloc(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        drop(unsafe { Vec::from_raw_parts(ptr, 0, len.max(1)) });
    }
}

fn read(ptr: *const u8, len: usize) -> &'static [u8] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

fn give(bytes: Vec<u8>) -> u64 {
    let mut b = bytes.into_boxed_slice();
    let (p, l) = (b.as_mut_ptr(), b.len());
    std::mem::forget(b);
    ((p as u64) << 32) | l as u64
}

#[unsafe(no_mangle)]
pub extern "C" fn agentim_new(ptr: *const u8, len: usize) -> u32 {
    let Ok(cfg) = serde_json::from_slice::<Config>(read(ptr, len)) else {
        return 0;
    };
    CORES.with(|c| {
        let mut c = c.borrow_mut();
        c.push(Some(Core::new(cfg)));
        c.len() as u32
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn agentim_handle(h: u32, ptr: *const u8, len: usize, now_ms: f64) -> u64 {
    let input = serde_json::from_slice::<Value>(read(ptr, len))
        .ok()
        .and_then(|v| wire::input_from_json(&v));
    let out = CORES.with(|c| {
        let mut c = c.borrow_mut();
        let Some(Some(core)) = c.get_mut((h as usize).wrapping_sub(1)) else {
            return Value::Array(vec![]);
        };
        match input {
            Some(i) => Value::Array(
                core.handle(i, now_ms as i64)
                    .iter()
                    .map(wire::effect_to_json)
                    .collect(),
            ),
            None => Value::Array(vec![]),
        }
    });
    give(out.to_string().into_bytes())
}

#[unsafe(no_mangle)]
pub extern "C" fn agentim_next_deadline(h: u32) -> f64 {
    CORES.with(|c| {
        c.borrow()
            .get((h as usize).wrapping_sub(1))
            .and_then(|c| c.as_ref())
            .and_then(|c| c.next_deadline())
            .map(|d| d as f64)
            .unwrap_or(-1.0)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn agentim_free(h: u32) {
    CORES.with(|c| {
        if let Some(slot) = c.borrow_mut().get_mut((h as usize).wrapping_sub(1)) {
            *slot = None;
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn agentim_strip_metadata(ptr: *const u8, len: usize) -> u64 {
    match media::strip_metadata(read(ptr, len)) {
        Some(b) if !b.is_empty() => give(b),
        _ => 0,
    }
}

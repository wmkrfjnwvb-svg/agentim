//! AgentIM client SDK, native runtime (docs/12 §2): the core's rules with
//! real IO — WebSocket over rustls, the SQLite cache — usable from Rust
//! ([`Client`]) and from any language through the C ABI (`include/agentim.h`).

mod ffi;
pub mod files;
mod runtime;
mod store;

pub use agentim_client_core::{ClientError, Config};
pub use runtime::{Client, Listener, now_ms};

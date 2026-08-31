//! Route-group handlers (R-SRV-010). One module per group; each returns a
//! `JsonResp`/`Reply` that the router turns into a `tiny_http` response. SSE is in
//! `crate::sse` because it hijacks the socket.

pub mod blob;
pub mod cost;
pub mod models;
pub mod plugin;
pub mod policy;
pub mod pref;
pub mod session;
pub mod tools;

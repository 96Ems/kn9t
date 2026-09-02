//! kn9t-store — R-STOR-010..R-STOR-180
//! SQLite-backed durable store implementing `kn9t_core::traits::Store`.
#![deny(warnings)]

mod db;
pub mod err;
mod project;
mod blob;
mod plan;
mod fork;
mod session;
mod session_delete;
mod live;
mod cost;
pub mod reproject;

pub use db::SqliteStore;
pub use cost::CostRollup;
pub use fork::fork_session;
pub use fork::fork_session_empty;
pub use session::create_session;
pub use plan::{
    close_orphan_tool_calls, close_orphan_tool_calls_with, compact_span, has_orphan_tool_call,
};

//! kn9t-store — R-STOR-010..R-STOR-180
//! SQLite-backed durable store implementing `kn9t_core::traits::Store`.
#![deny(warnings)]

mod blob;
mod cost;
mod db;
pub mod err;
mod fork;
mod live;
mod plan;
mod project;
pub mod reproject;
mod session;
mod session_delete;

pub use cost::CostRollup;
pub use db::SqliteStore;
pub use fork::fork_session;
pub use fork::fork_session_empty;
pub use plan::{
    close_orphan_tool_calls, close_orphan_tool_calls_with, compact_span, has_orphan_tool_call,
};
pub use session::create_session;

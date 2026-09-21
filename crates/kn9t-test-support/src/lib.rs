//! Shared test scaffolding for the kn9t workspace.
//!
//! This crate provides:
//! - `RecordingBus` — an `EventSink` that records all emitted events for assertions
//! - `StubStore` — an in-memory `Store` with scripted `plan_request` responses
//! - `AllowAll` / `DenyAll` — `Approver` test doubles
//! - Fixture helpers for building `ReplayProvider` test data
//! - `spawn_tools_registry` — spawns the real `kn9t-tools` plugin for integration tests
//!
//! # Usage
//!
//! Add as a dev-dependency in your crate's `Cargo.toml`:
//!
//! ```toml
//! [dev-dependencies]
//! kn9t-test-support = { path = "../kn9t-test-support" }
//! ```
//!
//! Then import what you need:
//!
//! ```ignore
//! use kn9t_test_support::{RecordingBus, StubStore, AllowAll, fixture_from_body};
//! ```
#![allow(clippy::unwrap_used)] // Test code is allowed to panic on failure
#![allow(dead_code)] // A shared toolbox; not every helper is used by every test

mod approver;
mod bus;
mod fixtures;
mod store;
mod tags;
mod tools;

pub use approver::{AllowAll, DenyAll};
pub use bus::RecordingBus;
pub use fixtures::{
    empty_read_map, fixture_from_body, fixture_with_terminal, replay, test_model_ref,
    test_model_spec, ScriptedProvider, StreamScript,
};
pub use store::{PlanScript, StubStore};
pub use tags::{event_tag, live_event_tag};
pub use tools::{get_tool, spawn_tools_registry};

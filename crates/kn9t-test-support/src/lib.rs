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

mod bus;
mod store;
mod approver;
mod fixtures;
mod tools;
mod tags;

pub use bus::RecordingBus;
pub use store::{StubStore, PlanScript};
pub use approver::{AllowAll, DenyAll};
pub use fixtures::{
    fixture_from_body, fixture_with_terminal, replay, empty_read_map,
    StreamScript, ScriptedProvider,
    test_model_ref, test_model_spec,
};
pub use tools::{spawn_tools_registry, get_tool};
pub use tags::{event_tag, live_event_tag};

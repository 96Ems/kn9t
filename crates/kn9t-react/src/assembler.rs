//! `assemble` — thin delegation to `kn9t_provider_core::assemble` (DB-02 resolution).
//!
//! Previously this module contained a duplicate implementation. Now that kn9t-react
//! depends on kn9t-provider-core (GI-1 still holds: one workspace dep), we delegate
//! to the canonical implementation in pcore. The `Assembled` type is re-exported as
//! a type alias so all call sites in the loop remain unchanged.

pub use kn9t_provider_core::{assemble, AssembleResult as Assembled};

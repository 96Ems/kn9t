//! Test doubles for the Approver trait.

use std::path::Path;
use kn9t_core::{Approver, Decision, ToolCall};
use kn9t_provider_core::ApprovalCtx;

/// Approver that approves whatever it is asked (ADR-0008).
pub struct AllowAll;

impl Approver for AllowAll {
    fn request(
        &self,
        _call: &ToolCall,
        _cwd: &Path,
        _reason: &str,
        _ctx: &ApprovalCtx,
    ) -> Decision {
        Decision::Allow
    }
}

/// Approver that refuses whatever it is asked, with a fixed reason.
pub struct DenyAll(pub String);

impl Approver for DenyAll {
    fn request(
        &self,
        _call: &ToolCall,
        _cwd: &Path,
        _reason: &str,
        _ctx: &ApprovalCtx,
    ) -> Decision {
        Decision::Deny {
            reason: self.0.clone(),
        }
    }
}

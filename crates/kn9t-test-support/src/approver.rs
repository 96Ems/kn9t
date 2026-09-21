//! Test approver implementations: AllowAll and DenyAll.

use kn9t_core::{Approver, Decision, ToolCall};
use kn9t_provider_core::ApprovalCtx;
use std::path::Path;

/// Approver that approves all requests.
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

/// Approver that denies all requests.
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

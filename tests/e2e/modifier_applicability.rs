//! Diagnostics for a modifier written where kotlinc's modifier checker does not apply it.

pub(super) use super::common;

#[path = "../external_modifier_targets_e2e.rs"]
mod external_modifier_targets_e2e;
#[path = "../modifier_containing_declarations_e2e.rs"]
mod modifier_containing_declarations_e2e;

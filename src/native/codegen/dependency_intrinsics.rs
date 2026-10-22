//! Positive declaration capabilities shared by dependency planning and Native emission.

use crate::libraries::{CompilerIntrinsic, ListStorageConstruction};

pub(super) fn list_storage_plan(operation: CompilerIntrinsic) -> Option<ListStorageConstruction> {
    match operation {
        CompilerIntrinsic::ListStorageConstruction(plan) => Some(plan),
        _ => None,
    }
}

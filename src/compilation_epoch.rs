//! Compilation numbers for declaration-owned type parameters.
//!
//! Those identities are interned process-wide (`\0tp:{compilation}:…`, and the call-site and
//! flow-intersection keys built from them). A monotonic number keeps two live compilations from
//! aliasing when they share a file offset, and it also leaks a fresh string on every edit. An
//! epoch starts the numbers over once the previous compilation is gone, so the next edit hits the
//! strings already interned.

use std::sync::Mutex;

static NEXT_COMPILATION_ID: Mutex<u64> = Mutex::new(1);

pub(crate) fn next_compilation_id() -> u64 {
    let mut next = NEXT_COMPILATION_ID
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let id = *next;
    *next = next.saturating_add(1);
    id
}

/// Number the next compilation from 1. Call only when no compilation is running in this process.
/// The LSP worker does this before each request: that request is the only compilation, and the
/// previous request's tables are already dropped, so the new numbers name the same declarations
/// and the interner reuses their strings.
pub fn begin_compilation_epoch() {
    let mut next = NEXT_COMPILATION_ID
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *next = 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_epoch_reissues_compilation_ids() {
        let mut next = NEXT_COMPILATION_ID
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = *next;
        *next = 1;
        let first = *next;
        *next = next.saturating_add(1);
        *next = 1;
        let second = *next;
        *next = saved;
        assert_eq!(first, second);
    }

    #[test]
    fn repeated_declaration_coordinates_reuse_one_interned_identity() {
        let first = crate::types::declaration_type_parameter(u64::MAX, 3, 4, 1, "T");
        let again = crate::types::declaration_type_parameter(u64::MAX, 3, 4, 1, "T");
        let other = crate::types::declaration_type_parameter(u64::MAX - 1, 3, 4, 1, "T");
        assert!(std::ptr::eq(first, again));
        assert!(!std::ptr::eq(first, other));
    }
}

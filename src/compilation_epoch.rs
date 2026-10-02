//! Compilation numbers for declaration-owned type parameters.
//!
//! Those identities are interned process-wide (`\0tp:{compilation}:…`, and the call-site and
//! flow-intersection keys built from them). A monotonic number keeps two live compilations from
//! aliasing when they share a file offset, and it also leaks a fresh string on every edit. An
//! epoch starts the numbers over once every compilation lease has been dropped, so the next edit
//! hits the strings already interned.

use std::sync::Mutex;

struct EpochState {
    next: u64,
    live: usize,
}

static EPOCH: Mutex<EpochState> = Mutex::new(EpochState { next: 1, live: 0 });

fn lock_epoch() -> std::sync::MutexGuard<'static, EpochState> {
    EPOCH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Counts one live compilation. Dropped with the semantic owner (`SymbolTable`, then the
/// pass-two symbols that consume it). Resetting the epoch while a lease is alive is refused.
pub(crate) struct CompilationLease {
    _private: (),
}

pub(crate) fn next_compilation_id() -> (u64, CompilationLease) {
    let mut epoch = lock_epoch();
    let id = epoch.next;
    epoch.next = epoch.next.saturating_add(1);
    epoch.live = epoch.live.saturating_add(1);
    (id, CompilationLease { _private: () })
}

impl Drop for CompilationLease {
    fn drop(&mut self) {
        let mut epoch = lock_epoch();
        epoch.live = epoch.live.saturating_sub(1);
    }
}

/// Number the next compilation from 1.
///
/// Returns false, and leaves the counter unchanged, when any compilation lease is still alive.
/// The analysis worker calls this before a request, after the previous request's tables have been
/// dropped. A caller that resets between two live compilations does not alias their declaration
/// identities.
pub fn begin_compilation_epoch() -> bool {
    let mut epoch = lock_epoch();
    if epoch.live != 0 {
        return false;
    }
    epoch.next = 1;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_compilation_refuses_the_epoch_reset() {
        let table = crate::resolve::SymbolTable::default();
        assert!(
            !begin_compilation_epoch(),
            "a live symbol table must keep its compilation id"
        );
        drop(table);
    }

    #[test]
    fn sequential_epochs_reuse_one_declaration_identity() {
        if std::env::var_os("KRUSTY_EPOCH_ISOLATED").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "compilation_epoch::tests::sequential_epochs_reuse_one_declaration_identity",
                    "--test-threads=1",
                ])
                .env("KRUSTY_EPOCH_ISOLATED", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "isolated epoch reuse failed\n{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let mut grown = Vec::new();
        for _ in 0..32 {
            let table = crate::resolve::SymbolTable::default();
            grown.push(crate::types::declaration_type_parameter(
                table.compilation_id(),
                0,
                0,
                0,
                "T",
            ));
        }
        for (index, identity) in grown.iter().enumerate() {
            assert!(
                grown[..index]
                    .iter()
                    .all(|earlier| !std::ptr::eq(*earlier, *identity)),
                "without an epoch reset each edit interns a new declaration identity"
            );
        }
        assert!(begin_compilation_epoch());

        let mut seen = Vec::new();
        for _ in 0..32 {
            let table = crate::resolve::SymbolTable::default();
            let identity =
                crate::types::declaration_type_parameter(table.compilation_id(), 0, 0, 0, "T");
            let other = crate::resolve::SymbolTable::default();
            assert_ne!(table.compilation_id(), other.compilation_id());
            assert!(!begin_compilation_epoch());
            drop(table);
            drop(other);
            assert!(begin_compilation_epoch());
            seen.push(identity);
        }
        assert!(
            seen.windows(2).all(|pair| std::ptr::eq(pair[0], pair[1])),
            "32 edits must reuse the interned declaration identity instead of allocating one per edit"
        );
        assert!(
            std::ptr::eq(seen[0], grown[0]),
            "the reset must hit the identity already interned for compilation 1"
        );
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

//! The exceptional edge into a catch: which lexical bindings a try body writes.
//!
//! A catch can begin after any point of its try body, so it keeps the try's entry facts minus the
//! narrowings of every binding the body writes. Writes are recorded by resolved binding identity
//! while the body is checked, never by spelling: a same-named inner declaration is another value.

use crate::fx_hash::HashSet;

use crate::types::Ty;

use super::lexical_bindings::BindingIdentity;
use super::scope::PathRoot;
use super::{Checker, CheckerScope};

/// Exact outer bindings written by one checked control-flow region. Every write invalidates
/// ordinary path/local flow at an exceptional successor. Saved Boolean implications distinguish
/// direct/replacement writes from in-place compound calls.
#[derive(Default)]
pub(super) struct TryBodyWrites {
    all: HashSet<BindingIdentity>,
    implication_replacements: HashSet<BindingIdentity>,
}

impl TryBodyWrites {
    pub(super) fn extend(&mut self, other: Self) {
        self.all.extend(other.all);
        self.implication_replacements
            .extend(other.implication_replacements);
    }
}

impl Checker<'_> {
    /// Record the flow-narrowed read type a write leaves on the innermost `name` binding, and note
    /// that binding as written by every try body being checked. The write replaces the value, so
    /// every path fact rooted at the old one goes.
    pub(super) fn set_local_narrow(
        &mut self,
        scope: &CheckerScope<'_>,
        name: &str,
        narrowed: Option<Ty>,
    ) {
        self.set_local_narrow_with_implications(scope, name, narrowed, true);
    }

    /// Commit a compound replacement's ordinary read type while retaining Kotlin's saved Boolean
    /// implication on normal flow. An exception from the selected `op` call can enter a catch
    /// without completing the write, so that exceptional edge still drops the implication.
    pub(super) fn set_local_narrow_preserving_implications(
        &mut self,
        scope: &CheckerScope<'_>,
        name: &str,
        narrowed: Option<Ty>,
    ) {
        self.set_local_narrow_with_implications(scope, name, narrowed, false);
    }

    fn set_local_narrow_with_implications(
        &mut self,
        scope: &CheckerScope<'_>,
        name: &str,
        narrowed: Option<Ty>,
        forget_on_normal_flow: bool,
    ) {
        // A statement after `throw` or `return` is still type-checked, then erased. Recording its
        // write would publish a value that does not run into a later `finally`.
        if self.unreachable_statement_depth != 0 {
            return;
        }
        if let Some(local) = self.lookup(scope, name) {
            scope.forget_paths_rooted_at(&PathRoot::Value(local.flow_identity));
            if forget_on_normal_flow {
                self.forget_implied_facts(scope, local.flow_identity);
            }
            if let Some(identity) = local.lexical_capture_identity {
                for written in &mut self.try_body_writes {
                    let identity = BindingIdentity::new(identity);
                    written.all.insert(identity);
                    written.implication_replacements.insert(identity);
                }
            }
        }
        scope.narrow_local(name, narrowed);
    }

    /// Check a try body and return the bindings it writes, including writes in nested trys.
    pub(super) fn try_body_written_bindings(
        &mut self,
        check_body: impl FnOnce(&mut Self) -> Ty,
    ) -> (Ty, TryBodyWrites) {
        self.try_body_writes.push(TryBodyWrites::default());
        let ty = check_body(self);
        let written = self
            .try_body_writes
            .pop()
            .expect("the try body's write set is still open");
        (ty, written)
    }

    /// Drop the narrowings of the written bindings still visible at a catch.
    pub(super) fn clear_narrowings_a_try_body_writes(
        &mut self,
        scope: &CheckerScope<'_>,
        written: &TryBodyWrites,
    ) {
        for &identity in &written.all {
            if let Some((name, local)) = self.visible_local_binding(scope, identity) {
                scope.forget_paths_rooted_at(&PathRoot::Value(local.flow_identity));
                if written.implication_replacements.contains(&identity) {
                    self.forget_implied_facts(scope, local.flow_identity);
                }
                scope.narrow_local(&name, None);
            }
        }
    }

    /// The final read fact left by each binding a finalizer writes. Capture this before restoring
    /// the normal try continuation: when the conservative finalizer entry and exit happen to have
    /// the same map delta, the finalizer's assignment must still dominate that continuation.
    pub(super) fn written_local_flow(
        &self,
        scope: &CheckerScope<'_>,
        written: &TryBodyWrites,
    ) -> Vec<(String, Option<Ty>)> {
        written
            .all
            .iter()
            .filter_map(|&identity| {
                let (name, _) = self.visible_local_binding(scope, identity)?;
                Some((name.clone(), scope.local_narrowing(&name)))
            })
            .collect()
    }
}

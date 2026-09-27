//! The exceptional edge into a catch: which lexical bindings a try body writes.
//!
//! A catch can begin after any point of its try body, so it keeps the try's entry facts minus the
//! narrowings of every binding the body writes. Writes are recorded by resolved binding identity
//! while the body is checked, never by spelling: a same-named inner declaration is another value.

use std::collections::HashSet;

use crate::types::Ty;

use super::lexical_bindings::BindingIdentity;
use super::scope::PathRoot;
use super::{Checker, CheckerScope};

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
        if let Some(local) = self.lookup(scope, name) {
            scope.forget_paths_rooted_at(&PathRoot::Value(local.flow_identity));
            if let Some(identity) = local.lexical_capture_identity {
                for written in &mut self.try_body_writes {
                    written.insert(BindingIdentity::new(identity));
                }
            }
        }
        scope.narrow_local(name, narrowed);
    }

    /// Check a try body and return the bindings it writes, including writes in nested trys.
    pub(super) fn try_body_written_bindings(
        &mut self,
        check_body: impl FnOnce(&mut Self) -> Ty,
    ) -> (Ty, HashSet<BindingIdentity>) {
        self.try_body_writes.push(HashSet::new());
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
        written: &HashSet<BindingIdentity>,
    ) {
        for &identity in written {
            if let Some((name, local)) = self.visible_local_binding(scope, identity) {
                scope.forget_paths_rooted_at(&PathRoot::Value(local.flow_identity));
                scope.narrow_local(&name, None);
            }
        }
    }
}

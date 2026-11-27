//! What proving a stable value non-null or null proves.
//!
//! A null check on a stable value applies the value's own proof and the implications recorded
//! where the value was bound (`value_implications`): after `val result = origin?.selector`,
//! proving `result` non-null proves `origin` non-null, and after `val selector = selectorFor(s)`
//! it proves the conclusion of the call's `returnsNotNull()` effect. Every consumer of a non-null
//! proof (`!= null`, `!!`, a `?.` receiver, `?: return`, `requireNotNull`) goes through
//! [`Checker::null_proof_or_decline`], and every consumer of a null proof through
//! [`Checker::implied_null_narrowings`].

use crate::diag::Span;
use crate::types::{Ty, TyKind};

use super::scope::{ImplyingQuestion, NarrowPath};
use super::{Checker, CheckerScope, Local};

impl Checker<'_> {
    /// The facts recorded for the stable value at `path` that its being non-null (`non_null`) or
    /// null proves, each passing the stability gate at this proof.
    fn implied_null_check_facts(
        &self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
        non_null: bool,
        out: &mut Vec<(NarrowPath, Ty)>,
        declined: &mut Vec<(String, Ty)>,
        site: Span,
    ) {
        let Some(implications) = scope
            .value_implications(path)
            .filter(|implications| implications.question == ImplyingQuestion::NonNull)
        else {
            return;
        };
        for (implied, ty) in &implications.facts(non_null).narrowings {
            if self.is_proof_stable_or_decline(scope, implied, *ty, declined, site) {
                out.push((implied.clone(), *ty));
            }
        }
    }

    /// What proving the stable value at `path` null proves through its recorded implications
    /// (a `returns(null) implies …` effect of the call it holds).
    pub(super) fn implied_null_narrowings(
        &self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
        out: &mut Vec<(NarrowPath, Ty)>,
        declined: &mut Vec<(String, Ty)>,
        site: Span,
    ) {
        self.implied_null_check_facts(scope, path, false, out, declined, site);
    }

    /// Narrow what `path`'s implications prove on a branch that has already proven `path`
    /// non-null itself: the selector of a `?.` whose receiver is `path`.
    pub(super) fn apply_implied_non_null_narrowing(
        &mut self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
        site: Span,
    ) {
        let mut implied = Vec::new();
        self.implied_null_check_facts(scope, path, true, &mut implied, &mut Vec::new(), site);
        for (path, ty) in implied {
            self.apply_narrowing_unchecked(scope, &path, ty);
        }
    }

    pub(super) fn null_proof_or_decline(
        &self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
        out: &mut Vec<(NarrowPath, Ty)>,
        declined: &mut Vec<(String, Ty)>,
        site: Span,
    ) {
        match self
            .stable_path_ty(scope, path, site)
            .and_then(proven_non_null)
        {
            Some(proven) => out.push((path.clone(), proven)),
            None => {
                if let Some((name, mutated)) = self.closure_mutated_decline(scope, path, site) {
                    if let Some(proven) = proven_non_null(mutated) {
                        if self.closure_writes_conform(&name, site, proven) {
                            out.push((path.clone(), proven));
                        } else {
                            declined.push((name, proven));
                        }
                    }
                }
            }
        }
        self.implied_null_check_facts(scope, path, true, out, declined, site);
    }
}

/// The type a value of type `ty` is proven to have once it is known not to be null: `T?` is `T`,
/// and a type parameter whose upper bound admits null (`V : S?`) is its definitely-non-null
/// `V & Any`. A type that already excludes null proves nothing new.
fn proven_non_null(ty: Ty) -> Option<Ty> {
    match ty.kind() {
        TyKind::Nullable(inner) => Some(inner),
        TyKind::TyParam(..) if ty.upper_bound_admits_null() => Some(ty.definitely_non_null()),
        _ => None,
    }
}

impl Local {
    pub(super) const fn has_unstable_delegated_read(self) -> bool {
        self.delegate_storage_ty.is_some()
    }
}

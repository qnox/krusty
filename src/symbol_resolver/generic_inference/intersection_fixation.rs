//! The upper type an intersection solution keeps for approximation.
//!
//! kotlinc fixes a type variable whose lower constraints meet only in an intersection to that
//! intersection, and when the intersection's public approximation is not a subtype of the
//! variable's upper type, it records that upper type on the intersection
//! (`ResultTypeResolver.specialResultForIntersectionType`). Checking keeps reading the
//! components, but wherever a supertype is wanted the intersection approximates to its upper type:
//! `fun p() = pick(StringBuilder(), "a")` with `fun <T : CharSequence> pick(vararg values: T): T`
//! publishes `CharSequence`, not `Any`, and `List<T>` publishes `List<CharSequence>`.

use std::borrow::Cow;

use crate::libraries::GenericSig;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TyKind};

use super::{published_declaration_type, GSigBinds};

/// `bindings` with each intersection solution of a `signature` formal recording the formal's
/// declared upper type, when the intersection's public approximation does not reach it. An
/// approximation an earlier call recorded is kept when it already reaches the new bound and is
/// replaced by the new bound otherwise; the components are preserved either way.
pub(crate) fn with_intersection_upper_types<'a>(
    source: &dyn SymbolSource,
    signature: &GenericSig,
    bindings: &'a GSigBinds,
) -> Cow<'a, GSigBinds> {
    let mut fixed = Cow::Borrowed(bindings);
    for (formal, bounds) in signature.formals.iter().zip(&signature.formal_bounds) {
        let Some(&solution) = bindings.get(formal) else {
            continue;
        };
        if !matches!(solution.non_null().kind(), TyKind::Intersection(_)) {
            continue;
        }
        let Some(upper) = declared_upper_type(signature, bounds, bindings) else {
            continue;
        };
        if crate::symbol_resolver::resolution_subtype(
            source,
            published_declaration_type(source, solution),
            published_declaration_type(source, upper),
        ) {
            continue;
        }
        fixed
            .to_mut()
            .insert(*formal, solution.with_intersection_approximation(upper));
    }
    fixed
}

/// The formal's declared upper bounds as one type, applied to the call's solutions. The implicit
/// `Any?` bound constrains nothing.
fn declared_upper_type(signature: &GenericSig, bounds: &[Ty], bindings: &GSigBinds) -> Option<Ty> {
    let implicit = Ty::nullable(Ty::obj("kotlin/Any"));
    let declared = bounds
        .iter()
        .filter(|bound| **bound != implicit)
        .map(|bound| crate::types::ty_subst_formals(*bound, &signature.formals, bindings))
        .collect::<Vec<_>>();
    (!declared.is_empty()).then(|| Ty::intersection(&declared))
}

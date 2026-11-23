use super::*;

/// Least-upper-bound over the class hierarchy. This is pure TYPE ALGEBRA over the symbol table: it
/// needs no scope, no diagnostics, and no checker state, so it lives outside `Checker` and is
/// callable from signature evaluation without constructing one.
/// Least-upper-bound over the class hierarchy, parameterized on its LOOKUP CONTEXT.
///
/// The algebra is shared, the context is not: the checker resolves classifiers through its own
/// per-file module view and answers assignability as itself, while signature evaluation supplies the
/// module view for the file whose signature is being solved. Relocating this without the parameters
/// silently changes which classifiers resolve — intersection joins regress to `unresolved reference`.
pub(crate) fn semantic_common_supertype(
    source: &dyn SymbolSource,
    oracle: &dyn crate::assignable::TypeOracle,
    a: Ty,
    b: Ty,
) -> Option<Ty> {
    semantic_common_supertype_inner(
        source,
        oracle,
        a,
        b,
        &mut crate::fx_hash::HashSet::default(),
    )
}

/// Semantic non-null form used by `!!`, Elvis, and signature constraints. A bare type parameter
/// needs a definitely-non-null occurrence (`T & Any` in Kotlin's type system); `Ty` represents that
/// compactly by retaining the parameter identity with a non-null upper bound.
pub(crate) fn definitely_non_null_ty(ty: Ty) -> Ty {
    ty.definitely_non_null()
}

pub(crate) fn semantic_common_supertype_inner(
    source: &dyn SymbolSource,
    oracle: &dyn crate::assignable::TypeOracle,
    a: Ty,
    b: Ty,
    visiting: &mut crate::fx_hash::HashSet<(Ty, Ty)>,
) -> Option<Ty> {
    if a == b {
        return Some(a);
    }
    // An error type is a subtype of every type and has no inhabitant of its own. The common
    // supertype of a real branch and an unresolved one is the real branch (`if (c) "ok" else
    // Missing.x` is `String`; `if (c) map else Missing.create()` keeps `map`'s members). Folding
    // the error in as the result discarded that type, so a later call was resolved on the error
    // and could select an unrelated generic extension. Two errors compare equal above and stay an
    // error, so a join of two unresolved branches does not invent a mismatch against the expected
    // type. A bare `null` beside an error generalizes to `Nothing?`, the same type `null` has on
    // its own (`if (c) Missing.x else null`).
    if a == Ty::Error {
        return Some(if b == Ty::Null {
            Ty::nullable(Ty::Nothing)
        } else {
            b
        });
    }
    if b == Ty::Error {
        return Some(if a == Ty::Null {
            Ty::nullable(Ty::Nothing)
        } else {
            a
        });
    }
    if let (TyKind::TyParam(left, _), TyKind::TyParam(right, _))
    | (TyKind::Var(left, _), TyKind::Var(right, _)) = (a.kind(), b.kind())
    {
        if left == right {
            // Elvis/`!!` represents `T & Any` as the same parameter identity with a non-null
            // bound. Joining it with an ordinary occurrence of T recovers T; widening both to the
            // upper bound would turn `fun <T> select(x: T, y: T): T = x ?: y` into `Any?`.
            return Some(if a.upper_bound_admits_null() {
                a
            } else if b.upper_bound_admits_null() {
                b
            } else {
                a
            });
        }
    }
    // `Nothing` is the bottom type: a diverging branch contributes no value, so the join is the
    // other branch (`if (c) x else throw e` has the type of `x`).
    if a == Ty::Nothing {
        return Some(b);
    }
    if b == Ty::Nothing {
        return Some(a);
    }
    // `Nothing?` is the type a bare `null` GENERALIZES to once it has been inferred into a type
    // argument: `Pair(x, null)` is `Pair<T, Nothing?>`, not `Pair<T, Null>`. Its only inhabitant is
    // `null`, so it joins exactly as the literal does — contributing nullability and nothing else.
    // Without this the join failed outright, and a covariant argument fell back to the declared
    // bound: `if (c) Pair(s, s) else Pair(s, null)` came out `Pair<String, out Any?>` instead of
    // `Pair<String, String?>`.
    let nothing_nullable = Ty::nullable(Ty::Nothing);
    let a = if a == nothing_nullable { Ty::Null } else { a };
    let b = if b == nothing_nullable { Ty::Null } else { b };
    if a == b {
        return Some(a);
    }
    // Nullability changes a TYPE-PARAMETER OCCURRENCE, not its declared upper bound. `T` joined
    // with `null` is `T?`; widening through `T`'s bound first would turn `T : Enum<T>` into
    // `Enum<T>?` and reject the ordinary reified `enumValueOf<T>` safe-wrapper idiom.
    if a == Ty::Null && b.is_ty_param() {
        return Some(Ty::nullable(b));
    }
    if b == Ty::Null && a.is_ty_param() {
        return Some(Ty::nullable(a));
    }
    // Different occurrences involving a type parameter join through its declared upper bound.
    // A definitely-non-null occurrence keeps a non-null bound (see `definitely_non_null_ty`), while
    // an ordinary `<T : Number?>` occurrence correctly contributes nullable `Number?`.
    if let Some(bound) = a.ty_param_bound() {
        return semantic_common_supertype_inner(source, oracle, bound, b, visiting);
    }
    if let Some(bound) = b.ty_param_bound() {
        return semantic_common_supertype_inner(source, oracle, a, bound, visiting);
    }
    if a == Ty::Null && b.is_reference() {
        return Some(Ty::nullable(b));
    }
    if b == Ty::Null && a.is_reference() {
        return Some(Ty::nullable(a));
    }
    if (a == Ty::Unit && b == Ty::Null) || (b == Ty::Unit && a == Ty::Null) {
        return Some(Ty::nullable(Ty::Unit));
    }
    // A primitive joins with `null` as its boxed (nullable) form — `if (c) true else null` is a
    // `Boolean?`, the primitive branch boxed at the merge.
    if a == Ty::Null {
        if let Some(nb) = b.nullable_boxed() {
            return Some(nb);
        }
    }
    if b == Ty::Null {
        if let Some(nb) = a.nullable_boxed() {
            return Some(nb);
        }
    }
    // Kotlin function types are contravariant in their inputs and covariant in their result. With
    // identical input shapes, the common supertype therefore retains those inputs and joins only
    // the result (`if (c) ({ First() }) else ({ Second() })` is `() -> Result`, not an
    // uncallable `Any`). A differing input shape needs a genuine type meet, which `Ty` cannot yet
    // publish, so it continues through the nominal common-supertype path below.
    if let (TyKind::Fun(left), TyKind::Fun(right)) = (a.non_null().kind(), b.non_null().kind()) {
        if left.params == right.params
            && left.context_count == right.context_count
            && left.has_receiver == right.has_receiver
            && left.suspend == right.suspend
        {
            if let Some(result) =
                semantic_common_supertype_inner(source, oracle, left.ret, right.ret, visiting)
            {
                let function = Ty::fun_with_shape(
                    left.params.clone(),
                    result,
                    left.context_count,
                    left.has_receiver,
                    left.suspend,
                );
                return Some(common_result_nullability(a, b, function));
            }
        }
    }
    // One semantic SAME-CLASS path covers source, module, and classpath spellings. It preserves
    // actual nullability and canonical builtin/function shapes. Parameterized classifiers are
    // handled below from their declaration metadata rather than erased here.
    if let Some(joined) = same_type_class_join(a, b) {
        return Some(joined);
    }
    // A function reference has a nominal reflection type (`KFunctionN<…>`) and also implements its
    // exact function interface. Contextual typing can expose either view in adjacent branches — for
    // example the right side of `::f ?: ::f` may already be shaped as `(A) -> R` while the non-null
    // left side retains `KFunction1<A, R>`. Their common value type is the function shape. Keep this
    // bridge deliberately narrower than "any callable classifier": a fun interface or an arbitrary
    // class with `invoke` is nominal and does not become a Kotlin function type at a join.
    let reflective_function_shape = |ty: Ty| {
        crate::assignable::applied_supertype(
            oracle,
            ty.non_null(),
            Ty::obj(crate::types::KFUNCTION_INTERNAL),
        )?;
        let shape = crate::symbol_resolver::classifier_callable_signature(source, ty.non_null())?;
        Some(common_result_nullability(ty, ty, shape))
    };
    match (a.non_null().kind(), b.non_null().kind()) {
        (TyKind::Fun(_), TyKind::Obj(..)) => {
            if let Some(right) = reflective_function_shape(b) {
                return semantic_common_supertype_inner(source, oracle, a, right, visiting);
            }
        }
        (TyKind::Obj(..), TyKind::Fun(_)) => {
            if let Some(left) = reflective_function_shape(a) {
                return semantic_common_supertype_inner(source, oracle, left, b, visiting);
            }
        }
        _ => {}
    }
    if let Some(joined) = applied_common_supertype_inner(source, oracle, a, b, visiting) {
        return Some(joined);
    }
    // Kotlin's universal reference upper bound is the result when no single more-specific common
    // classifier can represent the result. Preserve source/flexible nullability: `String?` and
    // `Marker` produce `Any?`, never the unsound non-null `Any`.
    let has_value_classifier =
        |ty: Ty| ty.non_null() == Ty::Unit || ty.kotlin_class_internal().is_some();
    if has_value_classifier(a) && has_value_classifier(b) {
        return Some(common_result_nullability(a, b, Ty::obj("kotlin/Any")));
    }
    None
}

/// The unique most-specific common classifier, with its type arguments as reached from both operands.
/// `MutableList<Int>` and `List<Int>` therefore join as `List<Int>` rather than losing the
/// declaration graph and collapsing to `Any`. If the two paths reach different applications of
/// the same generic classifier, reconstruct projections from declaration-site variance; an
/// intersection of multiple unrelated most-specific interfaces is not representable as one [`Ty`].
#[cfg(test)]
pub(crate) fn applied_common_supertype(
    source: &dyn SymbolSource,
    oracle: &dyn crate::assignable::TypeOracle,
    left: Ty,
    right: Ty,
) -> Option<Ty> {
    applied_common_supertype_inner(
        source,
        oracle,
        left,
        right,
        &mut crate::fx_hash::HashSet::default(),
    )
}

pub(crate) fn applied_common_supertype_inner(
    source: &dyn SymbolSource,
    oracle: &dyn crate::assignable::TypeOracle,
    left: Ty,
    right: Ty,
    visiting: &mut crate::fx_hash::HashSet<(Ty, Ty)>,
) -> Option<Ty> {
    if !visiting.insert((left, right)) {
        return None;
    }
    let result = applied_common_supertype_unguarded(source, oracle, left, right, visiting);
    visiting.remove(&(left, right));
    result
}

pub(crate) fn applied_common_supertype_unguarded(
    source: &dyn SymbolSource,
    oracle: &dyn crate::assignable::TypeOracle,
    left: Ty,
    right: Ty,
    visiting: &mut crate::fx_hash::HashSet<(Ty, Ty)>,
) -> Option<Ty> {
    // Nullability does not change a classifier's hierarchy. Restore the operands' result
    // nullability after finding their common classifier.
    let left_hierarchy = crate::symbol_resolver::applied_hierarchy(source, left.non_null());
    let right_hierarchy = crate::symbol_resolver::applied_hierarchy(source, right.non_null());
    let right_by_owner = right_hierarchy
        .into_iter()
        .map(|(owner, applied, _)| (owner, applied))
        .collect::<HashMap<_, _>>();
    let common = left_hierarchy
        .into_iter()
        .filter_map(|(owner, left_applied, _)| {
            right_by_owner
                .get(&owner)
                .copied()
                .map(|right_applied| (owner, left_applied, right_applied))
        })
        .collect::<Vec<_>>();
    let dominated = common
        .iter()
        .flat_map(|(owner, _, _)| {
            crate::symbol_resolver::applied_hierarchy(source, Ty::obj_name(*owner))
                .into_iter()
                .skip(1)
                .map(|(ancestor, _, _)| ancestor)
        })
        .collect::<crate::fx_hash::HashSet<_>>();
    let most_specific = common
        .iter()
        .copied()
        .filter(|(candidate, _, _)| !dominated.contains(candidate))
        .collect::<Vec<_>>();
    let [(owner, left_applied, right_applied)] = most_specific.as_slice() else {
        return None;
    };
    let joined = join_applied_classifier(
        source,
        oracle,
        *owner,
        *left_applied,
        *right_applied,
        visiting,
    )?;
    Some(common_result_nullability(left, right, joined))
}

pub(crate) fn join_applied_classifier(
    source: &dyn SymbolSource,
    oracle: &dyn crate::assignable::TypeOracle,
    owner: TypeName,
    left: Ty,
    right: Ty,
    visiting: &mut crate::fx_hash::HashSet<(Ty, Ty)>,
) -> Option<Ty> {
    if left == right {
        return Some(left.non_null());
    }
    // A pair already being joined further up is recursive: its argument declines, and the
    // classifier join falls back to the parameter's star bound.
    let mut element_join = |types: &[Ty]| match types {
        [left, right]
            if visiting.contains(&(*left, *right)) || visiting.contains(&(*right, *left)) =>
        {
            None
        }
        [first, rest @ ..] => rest.iter().try_fold(*first, |joined, ty| {
            semantic_common_supertype_inner(source, oracle, joined, *ty, visiting)
        }),
        [] => None,
    };
    crate::symbol_resolver::join_classifier_applications(
        source,
        oracle,
        owner,
        &[left, right],
        &mut element_join,
    )
    .ok()
}

#[cfg(test)]
mod tests;

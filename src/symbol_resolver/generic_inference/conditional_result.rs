//! Expected types for under-constrained calls used as conditional branches.

use super::{
    infer_generic_return_bindings, infer_generic_return_bindings_from_symbols, receiver_hierarchy,
    GenericSig, SymbolSource, Ty,
};

/// An expectation that instantiates `signature`'s return-only formals from a conditional sibling.
///
/// The returned type is what the call is rechecked against. It is not the conditional's own join.
/// Equal constructors bind directly (`emptyList()` beside `listOf("a")`). A result that is a
/// subtype of the sibling binds through that sibling (`linkedSetOf(): LinkedHashSet<T>` beside
/// `hashSetOf<E>(): HashSet<E>`, since `LinkedHashSet<T> <: HashSet<T>`). A sibling that is a
/// subtype of the result binds through the result's applied supertype (`emptyList(): List<T>`
/// beside `mutableListOf("a")`). Otherwise the two meet at their unique nearest generic
/// supertype (`linkedSetOf()` beside `arrayListOf<E>()` meet at a `MutableCollection<E>` face).
/// Equally-near unrelated faces are ambiguous and contribute no expectation; declaration order is
/// never a type-system tie breaker. A nullable type and that same type without null are one face,
/// and the kept expectation follows the declared return's nullability.
pub(crate) fn generic_return_expectation_from_sibling(
    source: &dyn SymbolSource,
    signature: &GenericSig,
    sibling: Ty,
    mut admits: impl FnMut(Ty, Ty) -> bool,
) -> Option<Ty> {
    fn binds(
        source: &dyn SymbolSource,
        signature: &GenericSig,
        expected: Ty,
        admits: &mut impl FnMut(Ty, Ty) -> bool,
    ) -> bool {
        infer_generic_return_bindings(signature, expected, &mut *admits).is_some()
            || infer_generic_return_bindings_from_symbols(source, signature, expected, &mut *admits)
                .is_some()
    }

    #[derive(Clone, Copy)]
    struct Candidate {
        expectation: Ty,
        distance: u32,
    }

    let sibling_hierarchy = receiver_hierarchy(source, sibling.non_null());
    let mut candidates = Vec::new();
    if binds(source, signature, sibling, &mut admits) {
        candidates.push(Candidate {
            expectation: sibling,
            distance: 0,
        });
    }
    for (declared_applied, declared_depth) in receiver_hierarchy(source, signature.ret.non_null()) {
        let Some(owner) = declared_applied.kotlin_class_internal() else {
            continue;
        };
        if declared_applied.is_erased_top() {
            continue;
        }
        for (sibling_face, sibling_depth) in sibling_hierarchy
            .iter()
            .copied()
            .filter(|(ty, _)| ty.kotlin_class_internal() == Some(owner) && !ty.is_erased_top())
        {
            if binds(source, signature, sibling_face, &mut admits) {
                candidates.push(Candidate {
                    expectation: sibling_face,
                    distance: declared_depth + sibling_depth,
                });
            }
        }
    }

    let nearest = candidates
        .iter()
        .map(|candidate| candidate.distance)
        .min()?;
    candidates.retain(|candidate| candidate.distance == nearest);
    // `Result<Int>?` and `Result<Int>` are one face. Keeping both made a nullable sibling look
    // ambiguous beside the same non-null constructor, so `Result.failure()` next to `n: Result<Int>?`
    // never rebound. Unrelated constructors at this distance stay ambiguous.
    let declared_nullable = signature.ret.is_nullable();
    let mut unique: Vec<Candidate> = Vec::new();
    for candidate in candidates {
        if let Some(existing) = unique
            .iter_mut()
            .find(|existing| existing.expectation.non_null() == candidate.expectation.non_null())
        {
            let existing_matches = existing.expectation.is_nullable() == declared_nullable;
            let candidate_matches = candidate.expectation.is_nullable() == declared_nullable;
            if candidate_matches && !existing_matches {
                *existing = candidate;
            }
            continue;
        }
        unique.push(candidate);
    }
    let [candidate] = unique.as_slice() else {
        return None;
    };
    Some(candidate.expectation)
}

//! Expected types for under-constrained calls used as conditional branches.

use super::{
    infer_generic_return_bindings, infer_generic_return_bindings_from_symbols, receiver_hierarchy,
    GenericSig, SourceOracle, SymbolSource, Ty,
};

/// An expectation that instantiates `signature`'s return-only formals from a conditional sibling.
///
/// The returned type is what the call is rechecked against. It is not the conditional's own join.
/// Equal constructors bind directly (`emptyList()` beside `listOf("a")`). A result that is a
/// subtype of the sibling binds through that sibling (`linkedSetOf(): LinkedHashSet<T>` beside
/// `hashSetOf<E>(): HashSet<E>`, since `LinkedHashSet<T> <: HashSet<T>`). A sibling that is a
/// subtype of the result binds through the result's applied supertype (`emptyList(): List<T>`
/// beside `mutableListOf("a")`). Otherwise the two meet at a shared generic supertype
/// (`linkedSetOf()` beside `arrayListOf<E>()` meet at a `MutableCollection<E>` face).
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

    if binds(source, signature, sibling, &mut admits) {
        return Some(sibling);
    }
    if let Some(applied) =
        crate::assignable::applied_supertype(&SourceOracle(source), sibling, signature.ret)
    {
        if binds(source, signature, applied, &mut admits) {
            return Some(applied);
        }
    }
    let sibling_hierarchy = receiver_hierarchy(source, sibling.non_null());
    let declared_owner = signature.ret.non_null().kotlin_class_internal();
    for (declared_applied, _) in receiver_hierarchy(source, signature.ret.non_null()) {
        let Some(owner) = declared_applied.kotlin_class_internal() else {
            continue;
        };
        if Some(owner) == declared_owner || declared_applied.is_erased_top() {
            continue;
        }
        let Some((sibling_face, _)) = sibling_hierarchy
            .iter()
            .find(|(ty, _)| ty.kotlin_class_internal() == Some(owner) && !ty.is_erased_top())
        else {
            continue;
        };
        if binds(source, signature, *sibling_face, &mut admits) {
            return Some(*sibling_face);
        }
    }
    None
}

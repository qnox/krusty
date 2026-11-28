//! The literal and vararg facts a call that leaves defaulted parameters to a `$default` callable
//! records on the selected callable.

use crate::libraries::LibraryCallable;
use crate::types::Ty;

/// The Kotlin type a default literal has. `null` has none — see [`super::SymbolResolver::default_literal_fits`].
pub(super) fn default_literal_ty(value: &crate::libraries::DefaultValue) -> Option<Ty> {
    use crate::libraries::DefaultValue;
    Some(match value {
        DefaultValue::Int(_) => Ty::Int,
        DefaultValue::Long(_) => Ty::Long,
        DefaultValue::Double(_) => Ty::Double,
        DefaultValue::Float(_) => Ty::Float,
        DefaultValue::Bool(_) => Ty::Boolean,
        DefaultValue::Char(_) => Ty::Char,
        DefaultValue::Str(_) => Ty::String,
        _ => return None,
    })
}

/// Record the vararg slot/element on a `$default` callable so the lowerer packs loose trailing
/// elements into the slot's array — or emits an EMPTY array when the vararg itself is omitted
/// (kotlinc's `$default` passes the array straight through, so the slot takes NO mask bit and a
/// null placeholder would trip the callee's non-null vararg check). The arg-vs-param identity
/// guard keeps a caller passing the array itself (`f("n", arr)`) from being packed.
pub(super) fn record_default_vararg_slot(
    callable: &mut LibraryCallable,
    vararg_index: Option<usize>,
    params: &[Ty],
    args: &[Ty],
) {
    let Some(index) = vararg_index else {
        return;
    };
    let Some(elem) = params.get(index).and_then(|param| param.array_read_elem()) else {
        return;
    };
    if args.get(index).copied() != params.get(index).copied() {
        callable.vararg_elem = Some(elem);
        callable.vararg_index = Some(index);
    }
}

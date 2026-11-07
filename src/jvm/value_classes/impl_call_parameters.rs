//! The checked parameters of a call rewritten to a value class's static `-impl` member.

use crate::ir::{ExprId, IrFile};
use crate::types::{Ty, TypeName};

/// Record the checked parameters of `call` once it has become a static over `owner`'s carrier.
///
/// The physical static call gains the former dispatch receiver at parameter zero. Keep the checked
/// declaration coordinates aligned with that new argument vector: otherwise parameter zero from
/// the source declaration is incorrectly applied to the receiver (for example `X.foo(other: I)`
/// boxes the `X` carrier as though it were `other`). The receiver remains semantically the value
/// class; this backend pass alone decides that the selected `*-impl` consumes its carrier. A
/// static whose own parameters are not the call's (`replacement`: an inherited default's static
/// takes the member as the class's applied supertype substitutes it) is represented for those.
/// The operands' `crossinline`/`noinline` modifiers of an inline call move with them: the receiver
/// takes neither, and an inliner that reads a lambda operand's modifier by its position still finds
/// it (a member inlined from its compiled body is the `-impl` static's body).
pub(super) fn align(
    ir: &mut IrFile,
    call: ExprId,
    owner: TypeName,
    replacement: Option<Box<[Ty]>>,
) {
    if let Some(modifiers) = ir.call_inline_modifiers.get(&call) {
        let aligned = std::iter::once(crate::types::InlineParameterModifier::None)
            .chain(modifiers.iter().copied())
            .collect();
        ir.call_inline_modifiers.insert(call, aligned);
    }
    let Some(parameters) = replacement.or_else(|| ir.call_declared_params.get(&call).cloned())
    else {
        return;
    };
    ir.call_declared_params.insert(
        call,
        std::iter::once(Ty::obj_name(owner))
            .chain(parameters.iter().copied())
            .collect(),
    );
}

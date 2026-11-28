//! The call a specialized realization of an inherited interface member makes to the member it
//! stands for.
//!
//! A supertype substitution specializes an inherited default (`foo(x: String)` for `I<String>`):
//! an implementing class's forwarder, and a sub-interface's `$DefaultImpls` static and
//! `access$<name>$jd` bridge, declare the substituted shape, while the member they call keeps its
//! own erased one. This adapts each argument and the result between the two.

use super::*;
use crate::bridge_adaptations::ValueClassAdapter;

/// The physical shape of the member a specialized realization calls, and the value classes the
/// substitution carries unboxed in the realization's own parameters and result.
#[derive(Clone, Copy)]
pub(super) struct InheritedMemberTarget<'a> {
    pub(super) params: &'a [Ty],
    pub(super) ret: Ty,
    pub(super) value_class_params: &'a [Option<ValueClassAdapter>],
    pub(super) value_class_ret: Option<ValueClassAdapter>,
}

impl InheritedMemberTarget<'_> {
    /// Load the realization's `params` from `first_slot` as the target takes them: a value class's
    /// carrier the substitution introduced is boxed with its `box-impl`, and a primitive with its
    /// wrapper, cast to the erased type-parameter slot it fills.
    pub(super) fn load_arguments(
        &self,
        cw: &mut ClassWriter,
        code: &mut CodeBuilder,
        first_slot: u16,
        params: &[Ty],
    ) {
        let mut slot = first_slot;
        for (index, (parameter, target)) in params.iter().zip(self.params).enumerate() {
            load(*parameter, slot, code);
            if let Some(carried) = self.value_class_params.get(index).copied().flatten() {
                emit_value_class_box_adapter(
                    cw,
                    code,
                    carried.owner,
                    *parameter,
                    carried.null_preserving,
                );
            } else if parameter.is_jvm_scalar() && target.is_reference() {
                box_prim_free(cw, code, *parameter);
                // kotlinc coerces the box to the erased slot it fills: `Number` for `U : Number`.
                let slot_class = krusty::jvm::names::instanceof_internal_name(*target);
                let boxed = parameter
                    .jvm_boxed_ref()
                    .map(krusty::jvm::names::instanceof_internal_name);
                if slot_class != "java/lang/Object" && boxed.as_deref() != Some(slot_class.as_str())
                {
                    let slot_class = cw.class_ref(&slot_class);
                    code.checkcast(slot_class);
                }
            }
            slot += slot_words(*parameter);
        }
    }

    /// Cast the target's erased result back to the realization's substituted `ret`, and unbox a
    /// value class the realization returns as its carrier.
    pub(super) fn cast_result(&self, cw: &mut ClassWriter, code: &mut CodeBuilder, ret: Ty) {
        if let Some(carried) = self.value_class_ret {
            emit_value_class_unbox_adapter(cw, code, carried.owner, ret, carried.null_preserving);
        } else if self.ret.is_reference() && ret.is_jvm_scalar() {
            unbox_prim_from(cw, code, self.ret, ret);
        } else if self.ret != ret && self.ret.is_reference() && ret.is_reference() {
            let result = cw.class_ref(&krusty::jvm::names::instanceof_internal_name(ret));
            code.checkcast(result);
        }
    }
}

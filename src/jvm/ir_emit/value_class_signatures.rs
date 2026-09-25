//! The generic `Signature` of a function whose value-class positions the JVM pass erased.
//!
//! A value class in a top-level parameter or result position is signed as its physical slot, exactly
//! as the descriptor spells it: kotlinc signs `fun <X> f(s: S, x: X)` over `S(val v: String)` as
//! `<X>(Ljava/lang/String;TX;)`. A value-class member's static replacement also signs the carrier it
//! receives first. A value class nested in a type argument (`List<S>`) keeps its own name. The IR
//! shape itself stays semantic, since the declaration's metadata is written from it.

use std::borrow::Cow;

use crate::ir::{IrFile, IrFunction, IrGenericSig};
use crate::types::Ty;

pub(super) fn physical_generic_signature<'a>(
    ir: &IrFile,
    fid: u32,
    function: &IrFunction,
    generic: &'a IrGenericSig,
) -> Cow<'a, IrGenericSig> {
    let Some((_, declared_params, declared_ret)) = ir.vc_declared_sigs.get(&fid) else {
        return Cow::Borrowed(generic);
    };
    let is_value_class = |ty: &Ty| {
        ty.non_null()
            .obj_internal()
            .is_some_and(|classifier| ir.is_value_class_name(classifier))
    };
    let carrier = usize::from(ir.jvm_value_class_receiver_impls.contains(&fid));
    assert_eq!(
        generic.params.len(),
        declared_params.len(),
        "a generic shape and its value-class declaration list the same parameters"
    );
    let mut physical = generic.clone();
    for (index, parameter) in physical.params.iter_mut().enumerate() {
        if is_value_class(&declared_params[index]) {
            *parameter = function.params[index + carrier];
        }
    }
    if carrier == 1 {
        physical.params.insert(0, function.params[0]);
    }
    if physical.ret.is_some() && is_value_class(declared_ret) {
        physical.ret = Some(function.ret);
    }
    Cow::Owned(physical)
}

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
    let Some((params, ret)) = physical_positions(ir, fid, function, &generic.params, generic.ret)
    else {
        return Cow::Borrowed(generic);
    };
    let mut physical = generic.clone();
    physical.params = params;
    physical.ret = ret;
    Cow::Owned(physical)
}

/// A member's recorded semantic parameters and result (an accessor's, or one using its class's
/// type parameters), with the same value-class positions spelled physically.
pub(super) fn physical_member_signature<'a>(
    ir: &'a IrFile,
    fid: u32,
    function: &IrFunction,
) -> Option<(Cow<'a, [Ty]>, Ty)> {
    let (params, ret) = ir.member_semantic_sigs.get(&fid)?;
    Some(
        match physical_positions(ir, fid, function, params, Some(*ret)) {
            Some((params, ret)) => (Cow::Owned(params), ret.expect("a member keeps its result")),
            None => (Cow::Borrowed(params.as_slice()), *ret),
        },
    )
}

/// `params` and `ret` with each top-level value-class position replaced by the function's physical
/// slot, and a static replacement's carrier first; `None` when the pass erased nothing here.
fn physical_positions(
    ir: &IrFile,
    fid: u32,
    function: &IrFunction,
    params: &[Ty],
    ret: Option<Ty>,
) -> Option<(Vec<Ty>, Option<Ty>)> {
    let (_, declared_params, declared_ret) = ir.vc_declared_sigs.get(&fid)?;
    // A value class written as such; a type parameter bounded by one keeps its own name (`TT;`).
    let is_value_class = |ty: &Ty| matches!(ty.non_null(), Ty::Obj(classifier, _) if ir.is_value_class_name(classifier));
    let carrier = usize::from(ir.jvm_value_class_receiver_impls.contains(&fid));
    assert_eq!(
        params.len(),
        declared_params.len(),
        "a signature shape and its value-class declaration list the same parameters"
    );
    let mut physical = params
        .iter()
        .zip(declared_params)
        .enumerate()
        .map(|(index, (param, declared))| {
            if is_value_class(declared) {
                function.params[index + carrier]
            } else {
                *param
            }
        })
        .collect::<Vec<_>>();
    if carrier == 1 {
        physical.insert(0, function.params[0]);
    }
    let ret = ret.map(|ret| {
        if is_value_class(declared_ret) {
            function.ret
        } else {
            ret
        }
    });
    Some((physical, ret))
}

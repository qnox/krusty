//! The slots through which a lifted lambda's own parameters arrive. A generic `FunctionN.invoke`
//! or SAM slot carries a value class boxed; a typed lambda-class `invoke` takes its carrier.

use crate::fx_hash::{HashMap, HashSet};
use crate::ir::IrFile;
use crate::types::{Ty, TypeName};

/// The suspend lambdas that become `SuspendLambda` classes: each one a plain function value with a
/// class site, not SAM converted and not an argument an inline call's splice consumes. kotlinc
/// writes every suspend lambda so; the others are realized another way here, over the lifted
/// function, and keep the boxed slot that realization hands them.
pub(super) fn suspend_lambda_classes(ir: &IrFile, suspend_fids: &HashSet<u32>) -> HashSet<u32> {
    suspend_fids
        .iter()
        .copied()
        .filter(|fid| {
            ir.lambda_own_params_from.contains_key(fid)
                && !ir.lambda_sam_signature.contains_key(fid)
                && crate::jvm::lambda_classes::site(ir, *fid).is_some()
        })
        .collect()
}

/// Where a lifted lambda's own parameters begin when they arrive through a generic `FunctionN` or
/// SAM slot, which carries a value class boxed. A suspend lambda that becomes a `SuspendLambda`
/// class has no such slot: kotlinc's typed `invoke-<hash>` takes each value class as its carrier,
/// the erased `FunctionN.invoke` bridge unboxing the box it receives, so its implementation takes
/// the carrier like any other function.
pub(super) fn boxed_lambda_own_params_from(
    own_params_from: &HashMap<u32, u32>,
    suspend_lambda_classes: &HashSet<u32>,
    fid: u32,
) -> Option<u32> {
    let own_from = own_params_from.get(&fid).copied()?;
    (!suspend_lambda_classes.contains(&fid)).then_some(own_from)
}

/// Whether a lifted lambda realizes the value class in one of its OWN slots BOXED.
///
/// A plain `FunctionN.invoke` slot is generic (`Object`), and a value class travelling through one is
/// boxed. A SAM conversion targets a DECLARED method instead, so the answer is whatever the interface
/// spells: a slot declared as the value class itself erases to the underlying (kotlinc's
/// `ResultHandler.onResult(Ljava/lang/Object;)` carries the *carrier*, not a `kotlin/Result` box),
/// while a slot declared as a type parameter is generic again and does box. `declared` is the SAM
/// method's declaration at that position; `None` means there is no SAM (or its arity doesn't line up
/// with the lambda's own parameters), which keeps the `FunctionN` reading.
pub(super) fn lambda_slot_is_boxed(declared: Option<&Ty>, value_class: TypeName) -> bool {
    declared.is_none_or(|t| t.non_null().obj_internal() != Some(value_class))
}

/// The SAM method's declared parameter types for a lifted lambda, aligned to the lambda's OWN
/// parameters (`own_from` is where those begin, after the captures). `None` unless the lambda was SAM
/// converted AND the two arities agree — an implicit receiver or context parameter in the lambda's
/// own slots has no counterpart in the interface declaration, and misaligned slots must not be read.
pub(super) fn lambda_sam_params(
    signatures: &HashMap<u32, (Vec<Ty>, Ty)>,
    fid: u32,
    own_from: u32,
    total_params: usize,
) -> Option<&[Ty]> {
    let (params, _) = signatures.get(&fid)?;
    (params.len() == total_params.saturating_sub(own_from as usize)).then_some(params.as_slice())
}

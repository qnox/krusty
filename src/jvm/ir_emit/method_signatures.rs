//! Which generic `Signature` attribute kotlinc writes for a method, given its access word and
//! declaration identity. The access word itself is computed by `method_access`.

use super::signature_formatter::{JvmSignatureFormatter, Wildcards};
use super::type_parameter_signatures::jvm_type_params;
use crate::ir::IrFile;
use crate::jvm::classfile::ACC_SYNTHETIC;
use crate::types::Ty;

/// Whether a method keeps its generic `Signature` even when it is synthetic. kotlinc
/// (`FunctionCodegen.needsGenericSignature`) keeps it on an `inline` function, whose generics the
/// inliner reads, and on a suspend function's `$suspendImpl` static.
pub(super) fn keeps_signature_when_synthetic(ir: &IrFile, fid: u32) -> bool {
    ir.inline_fns.contains(&fid) || ir.jvm_suspend_impl_bodies.contains_key(&fid)
}

/// The generic `Signature` kotlinc writes for a method. It writes none on a synthetic method
/// unless `keeps_when_synthetic` says otherwise, and none that spells the erased descriptor back:
/// such an attribute says nothing the descriptor does not.
pub(super) fn written_signature(
    access: u16,
    keeps_when_synthetic: bool,
    descriptor: &str,
    signature: Option<String>,
) -> Option<String> {
    signature.filter(|signature| {
        (keeps_when_synthetic || access & ACC_SYNTHETIC == 0) && signature != descriptor
    })
}

pub(super) fn suspend_generic_method_sig(
    formatter: &JvmSignatureFormatter<'_>,
    generic: &crate::ir::IrGenericSig,
    ret: &Ty,
) -> Option<String> {
    let mut signature = jvm_type_params(formatter, generic)?;
    signature.push_str(&suspend_method_sig(formatter, &generic.params, ret)?);
    Some(signature)
}

/// The generic `Signature` of a `suspend fun`'s CPS method. The declared return type survives only in
/// the trailing continuation's type argument — `suspend fun f(a: String)` compiles to
/// `f(String, Continuation): Object` but signs as
/// `(Ljava/lang/String;Lkotlin/coroutines/Continuation<-Lkotlin/Unit;>;)Ljava/lang/Object;`, the `-`
/// being `? super`. Takes the DECLARED parameters and return, not the rewritten ones.
pub(super) fn suspend_method_sig(
    formatter: &JvmSignatureFormatter<'_>,
    params: &[Ty],
    ret: &Ty,
) -> Option<String> {
    // The suspend return travels as `Continuation<-RET>`, a PARAMETER of the erased method, and
    // kotlinc wildcards inside it: `suspend fun <U> f(): Cont<U>` signs
    // `(Lkotlin/coroutines/Continuation<-LCont<+TU;>;>;)Ljava/lang/Object;`.
    let continuation = formatter.continuation(*ret, Wildcards::Declared)?;
    let mut s = String::from("(");
    for p in params {
        s.push_str(&formatter.parameter_ty(p)?);
    }
    s.push_str(&continuation);
    s.push_str(")Ljava/lang/Object;");
    Some(s)
}

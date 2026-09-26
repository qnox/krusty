//! Which generic `Signature` attribute kotlinc writes for a method, given its access word and
//! declaration identity. The access word itself is computed by `method_access`.

use crate::ir::IrFile;
use crate::jvm::classfile::ACC_SYNTHETIC;

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

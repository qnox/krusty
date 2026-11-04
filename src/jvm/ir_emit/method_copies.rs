//! The `name$$forInline` copy written beside a suspend function an inlining caller copies, and
//! the continuation class a method's copy constructs.
//!
//! The copy keeps the function's body as codegen wrote it, for the caller's transformer to build
//! its machine over. A method's copy opens by constructing the method's continuation, which is
//! therefore written, and public, even when the method itself needs no machine.

use super::{CodeBuilder, EmitEnv, Emitter, MethodPlacement};
use crate::ir::{ClassId, IrFile};
use crate::jvm::classfile::CoroutineOutcome;
use crate::jvm::suspend_inline_functions::{constructed_by_method_copy, for_inline_copy};

/// The placement of the `$$forInline` copy of `fid`, whose method is written at `placement`, when
/// the function has one: on the same owner, an instance method only where the method is one, and
/// a named object's `@JvmStatic` member's copy static as its method is.
pub(super) fn for_inline_placement(
    ir: &IrFile,
    fid: u32,
    placement: MethodPlacement,
    env: &EmitEnv,
) -> Option<MethodPlacement> {
    let kind = for_inline_copy(ir, fid)?;
    env.emit_time_machines.transformed(fid)?;
    let (static_owner, instance, singleton) = match placement {
        MethodPlacement::Declared {
            static_owner,
            instance,
        } => (static_owner, instance, None),
        MethodPlacement::Singleton {
            static_owner,
            singleton,
        } => (static_owner, false, Some(singleton)),
        MethodPlacement::Holder(_) | MethodPlacement::ForInlineCopy { .. } => return None,
    };
    Some(MethodPlacement::ForInlineCopy {
        static_owner,
        instance,
        singleton,
        kind,
    })
}

/// Whether the continuation class `class` is left unwritten: its function's suspension points all
/// turned out to be tail calls, so it has no machine, and no method's copy constructs it.
pub(super) fn continuation_is_unwritten(
    ir: &IrFile,
    env: &EmitEnv,
    class_id: ClassId,
    class: &str,
) -> bool {
    matches!(
        env.run.transformed_coroutine(class),
        Some(CoroutineOutcome::TailCalls)
    ) && !constructed_by_method_copy(ir, class_id)
}

/// The access of a synthesized continuation class: package-private, as kotlinc's, unless a
/// method's copy constructs it from an inlining caller's class.
pub(super) fn continuation_access(ir: &IrFile, class_id: ClassId) -> u16 {
    u16::from(constructed_by_method_copy(ir, class_id)) | 0x0020 // [PUBLIC |] SUPER
}

/// Whether a continuation's `@DebugMetadata` describes its machine's states: a machine-less one,
/// written only for a method's copy, has none.
pub(super) fn describes_states(transformed: Option<&CoroutineOutcome>) -> bool {
    !matches!(transformed, Some(CoroutineOutcome::TailCalls))
}

impl Emitter<'_> {
    /// Open a method's copy by constructing the method's continuation on the receiver and the
    /// completion value `completion`, under the continuation's final name: a copy's continuation
    /// is renamed with the copy, after coroutine lowering named it.
    pub(super) fn construct_copy_continuation(
        &mut self,
        fid: u32,
        completion: u32,
        code: &mut CodeBuilder,
    ) {
        let completion = self
            .slots
            .get(&completion)
            .map(|&(slot, _)| slot)
            .expect("a suspend method's copy has a completion parameter");
        let continuation = self
            .emit_time_machines
            .transformed(fid)
            .map(|machine| machine.continuation_class.clone())
            .expect("a method's `$$forInline` copy is written beside its transformed machine");
        self.emit_fake_continuation_construction(&continuation, completion, code);
    }
}

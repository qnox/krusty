//! Kotlin metadata fields and versioned synthetic-class annotation policy.

/// A built `@kotlin.Metadata` annotation for a file facade: the `k`/`mv`/`xi` ints and the `d1`
/// (encoded protobuf, one byte per `char`) / `d2` (string table) arrays.
#[derive(Clone)]
pub struct KotlinMetadata {
    pub k: i32,
    pub stamp: crate::jvm::classfile::MetadataStamp,
    pub xi: i32,
    pub d1: Vec<String>,
    pub d2: Vec<String>,
}

pub(super) fn is_continuation_class(class: &crate::ir::IrClass) -> bool {
    class.superclass_matches("kotlin/coroutines/jvm/internal/ContinuationImpl")
        || class.superclass_matches("kotlin/coroutines/jvm/internal/RestrictedContinuationImpl")
}

pub(super) fn is_coroutine_state_machine(class: &crate::ir::IrClass) -> bool {
    is_continuation_class(class)
        || class.superclass_matches("kotlin/coroutines/jvm/internal/SuspendLambda")
        || class.superclass_matches("kotlin/coroutines/jvm/internal/RestrictedSuspendLambda")
}

/// `ProtoBuf.Visibility` numbers kotlinc writes for a synthetic class.
pub(super) const SYNTHETIC_PROTECTED: i32 = 2;
pub(super) const SYNTHETIC_PUBLIC: i32 = 3;
pub(super) const SYNTHETIC_LOCAL: i32 = 5;

/// `JvmAnnotationNames.METADATA_PUBLIC_ABI_FLAG`. A class generated for a non-private `inline`
/// function is part of that function's ABI.
pub(super) const METADATA_PUBLIC_ABI_FLAG: i32 = 1 << 7;

/// `@Metadata.xi` of a class with its own `k=1` record. kotlinc sets
/// [`METADATA_PUBLIC_ABI_FLAG`] on a class declared, at any depth, in the body of an `inline`
/// function other code can inline: one that is not private and not a member of a private class.
/// Only the function's own class counts: a public class nested in a private one still marks its
/// inline members' objects.
/// A script's class also sets `METADATA_SCRIPT_FLAG`.
pub(super) fn class_xi(ir: &crate::ir::IrFile, class: &crate::ir::IrClass) -> i32 {
    const JVM_IR_AND_STABLE_ABI: i32 = 48;
    const METADATA_SCRIPT_FLAG: i32 = 4;
    let xi = if class.is_script {
        JVM_IR_AND_STABLE_ABI | METADATA_SCRIPT_FLAG
    } else {
        JVM_IR_AND_STABLE_ABI
    };
    if in_public_inline_scope(ir, class.enclosure) {
        xi | METADATA_PUBLIC_ABI_FLAG
    } else {
        xi
    }
}

/// `@Metadata.xi` of a lambda's `k=3` class declared at `enclosure`. One written in an `inline`
/// function's body is public, since a caller in another package constructs it, and part of the
/// public ABI when that function is ([`METADATA_PUBLIC_ABI_FLAG`]); any other is local.
pub(super) fn lambda_class_xi(
    ir: &crate::ir::IrFile,
    enclosure: Option<crate::ir::IrEnclosure>,
) -> i32 {
    let in_inline_function = super::enclosure::enclosing_functions(ir, enclosure)
        .any(|(function, _)| ir.inline_fns.contains(&function));
    if !in_inline_function {
        synthetic_class_xi(SYNTHETIC_LOCAL)
    } else if in_public_inline_scope(ir, enclosure) {
        synthetic_class_xi(SYNTHETIC_PUBLIC) | METADATA_PUBLIC_ABI_FLAG
    } else {
        synthetic_class_xi(SYNTHETIC_PUBLIC)
    }
}

fn in_public_inline_scope(
    ir: &crate::ir::IrFile,
    enclosure: Option<crate::ir::IrEnclosure>,
) -> bool {
    super::enclosure::enclosing_functions(ir, enclosure)
        .find(|&(function, _)| ir.inline_fns.contains(&function))
        .is_some_and(|(function, owner)| {
            !ir.method_visibility(function).is_private()
                && owner.is_none_or(|owner| {
                    !ir.class_visibilities
                        .get(&owner)
                        .is_some_and(|visibility| visibility.is_private())
                })
        })
}

/// `@Metadata.xi` of a `k=3` synthetic class. Kotlin 2.4.20 packs the class's normalized
/// visibility into bits 8–10; earlier releases wrote the IR flags alone.
pub(super) fn synthetic_class_xi(visibility: i32) -> i32 {
    const JVM_IR_AND_STABLE_ABI: i32 = 48;
    if crate::kotlin_version::at_least(crate::kotlin_version::KotlinVersion::V2_4_20) {
        JVM_IR_AND_STABLE_ABI | (visibility << 8)
    } else {
        JVM_IR_AND_STABLE_ABI
    }
}

/// Finish a class kotlinc generates for an expression in the scope it was written in: a callable
/// reference or an annotation instantiation. It carries the `k=3` synthetic-class record with LOCAL
/// visibility, as kotlinc 2.4.20 writes it.
///
/// A suspend-conversion or suspend-reference carrier's `invoke` is a suspend function kotlinc's
/// transformer rewrites, so the class is finished through the run that keeps what it found.
pub(super) fn finish_local_synthetic_class(
    mut cw: crate::jvm::classfile::ClassWriter,
    env: &super::EmitEnv,
) -> Vec<u8> {
    cw.set_kotlin_metadata(
        3,
        env.metadata_stamp,
        synthetic_class_xi(SYNTHETIC_LOCAL),
        &[],
        &[],
    );
    env.run.finish_class(cw)
}

/// Finish a callable reference's class: a `k=3` synthetic-class record whose visibility and
/// public-ABI flag follow the scope it is written in, as a lambda's class does
/// ([`lambda_class_xi`]).
pub(super) fn finish_reference_class(
    mut cw: crate::jvm::classfile::ClassWriter,
    env: &super::EmitEnv,
    ir: &crate::ir::IrFile,
    class: &crate::ir::IrClass,
) -> Vec<u8> {
    cw.set_kotlin_metadata(
        3,
        env.metadata_stamp,
        lambda_class_xi(ir, class.enclosure),
        &[],
        &[],
    );
    env.run.finish_class(cw)
}

/// Finish a synthetic class kotlinc generates for a whole file or class, such as the
/// `$EntriesIntrinsicMappings` holder: the `k=3` synthetic-class record with PUBLIC visibility.
pub(super) fn finish_public_synthetic_class(
    mut cw: crate::jvm::classfile::ClassWriter,
    env: &super::EmitEnv,
) -> Vec<u8> {
    cw.set_kotlin_metadata(
        3,
        env.metadata_stamp,
        synthetic_class_xi(SYNTHETIC_PUBLIC),
        &[],
        &[],
    );
    env.run.finish_class(cw)
}

/// Whether a synthetic annotation implementation stamps nullability on its constructor, `equals`
/// and `toString`. Kotlin 2.4.20 stopped; earlier releases did.
pub(super) fn annotation_impl_carries_nullability() -> bool {
    !crate::kotlin_version::at_least(crate::kotlin_version::KotlinVersion::V2_4_20)
}

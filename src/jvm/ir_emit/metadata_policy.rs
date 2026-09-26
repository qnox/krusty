//! Kotlin metadata fields and versioned synthetic-class annotation policy.

/// A built `@kotlin.Metadata` annotation for a file facade: the `k`/`mv`/`xi` ints and the `d1`
/// (encoded protobuf, one byte per `char`) / `d2` (string table) arrays.
#[derive(Clone)]
pub struct KotlinMetadata {
    pub k: i32,
    pub mv: Vec<i32>,
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
pub(super) fn finish_local_synthetic_class(mut cw: crate::jvm::classfile::ClassWriter) -> Vec<u8> {
    cw.set_kotlin_metadata(3, &[2, 4, 0], synthetic_class_xi(SYNTHETIC_LOCAL), &[], &[]);
    cw.finish()
}

/// Whether a synthetic annotation implementation stamps nullability on its constructor, `equals`
/// and `toString`. Kotlin 2.4.20 stopped; earlier releases did.
pub(super) fn annotation_impl_carries_nullability() -> bool {
    !crate::kotlin_version::at_least(crate::kotlin_version::KotlinVersion::V2_4_20)
}

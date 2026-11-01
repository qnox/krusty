//! Which emitted classes are coroutine state machines: a suspend function's continuation class or a
//! suspend lambda's class. Metadata, inner-class records, and parameter guards all follow this one
//! physical classification.

/// A suspend function's continuation class.
pub(crate) fn is_continuation_class(class: &crate::ir::IrClass) -> bool {
    class.superclass_matches("kotlin/coroutines/jvm/internal/ContinuationImpl")
        || class.superclass_matches("kotlin/coroutines/jvm/internal/RestrictedContinuationImpl")
}

/// A function's continuation class or a suspend lambda's class: both are state machines kotlinc
/// generates, not source declarations.
pub(crate) fn is_coroutine_state_machine(class: &crate::ir::IrClass) -> bool {
    is_continuation_class(class)
        || class.superclass_matches("kotlin/coroutines/jvm/internal/SuspendLambda")
        || class.superclass_matches("kotlin/coroutines/jvm/internal/RestrictedSuspendLambda")
}

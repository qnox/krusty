//! JVM emission facts produced while coroutine lowering still owns the physical suspend shape.

/// JVM class metadata computed before continuation spill scopes are discarded.
#[derive(Clone, Debug, Default)]
pub struct ContinuationMetadata {
    pub l: Vec<i32>,
    pub nl: Vec<i32>,
    pub i: Vec<i32>,
    pub s: Vec<String>,
    pub n: Vec<String>,
    pub m: String,
    pub c: String,
    pub v: i32,
    pub enclosing_class: String,
    pub enclosing_method: String,
    pub enclosing_descriptor: String,
}

pub type ContinuationMetadataMap = std::collections::HashMap<String, ContinuationMetadata>;

/// What a JVM `return` makes of the suspend callee's result it forwards. `COROUTINE_SUSPENDED` is
/// always returned as it is; any other result is adapted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForwardedSuspendResult {
    /// A forwarded `Unit` function's result becomes `Unit.INSTANCE` (Kotlin 2.4.20).
    Unit,
    /// A continuation re-entering a function whose value-class result crosses as its reference
    /// carrier boxes it: `checkcast` the carrier, then `box-impl`, null-safely when `nullable`.
    ValueClassBox {
        classifier: crate::types::TypeName,
        carrier: crate::types::Ty,
        nullable: bool,
    },
}

/// JVM return nodes whose forwarded suspend result is adapted as [`ForwardedSuspendResult`] says.
pub(crate) type SuspendResultForwards =
    std::collections::HashMap<crate::ir::ExprId, ForwardedSuspendResult>;

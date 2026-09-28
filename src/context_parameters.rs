//! Cross-phase identity of a declaration's context parameter.
//!
//! This is source semantics retained by parsing, checked FIR, common IR, metadata, and backends;
//! it is not an AST node and must not make later phases depend on the parser model.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ContextParameterKind {
    #[default]
    None,
    Named,
    Anonymous,
    LegacyReceiver,
}

/// Which source-semantic context role an implicit receiver capture preserves across checked FIR,
/// common IR, and target emission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapturedContextKind {
    /// `context(_: Box)` on a callable or class.
    Anonymous,
    /// A context parameter of the function type a lambda is checked against,
    /// `context(Box) () -> R`.
    FunctionType,
    /// Legacy `context(Box)`.
    LegacyReceiver,
}

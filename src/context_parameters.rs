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

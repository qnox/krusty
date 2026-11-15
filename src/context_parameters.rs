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

/// The selected storage of a context binding. The resolver certifies this while the exact
/// selected value and every crossed classifier boundary are still in scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextBindingSource {
    ProvenLocalDepth {
        shadow_depth: usize,
        /// Absent only for genuine nonlexical receiver aliases or reconstructed synthetic reads.
        lexical_binding: Option<u32>,
    },
    RequiredClassCapture {
        lexical_binding: u32,
    },
}

impl ContextBindingSource {
    /// Normalize a lexical coordinate across a nested callable. A required class capture is
    /// already an exact lexical identity and has no depth to reinterpret.
    pub(crate) fn outside_nested_bindings(self, count: usize) -> Option<Self> {
        match self {
            Self::ProvenLocalDepth {
                shadow_depth,
                lexical_binding,
            } => Some(Self::ProvenLocalDepth {
                shadow_depth: shadow_depth.checked_sub(count)?,
                lexical_binding,
            }),
            Self::RequiredClassCapture { .. } => Some(self),
        }
    }
}

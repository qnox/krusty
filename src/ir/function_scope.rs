//! The function body lowering currently owns.

/// Exact function body currently owned by lowering. `source_name` is only the naming stem for
/// generated methods/classes; semantic properties are keyed by `function`, never reconstructed from
/// that spelling. `None` represents a constructor, property initializer, or class initializer.
#[derive(Clone, Debug, Default)]
pub struct IrFunctionScope {
    pub function: Option<u32>,
    pub source_name: String,
    /// Declaration-owned type-parameter identities whose class-literal operations may remain as
    /// reified placeholders in this emitted method. Kept on the lexical function scope so nested
    /// inline expansion saves/restores the fact with its owner instead of a parallel current-state
    /// field recovering parameters from source spelling.
    pub emitted_reified_parameters: std::collections::HashSet<String>,
}

impl IrFunctionScope {
    pub fn declared(function: u32, source_name: String) -> Self {
        Self {
            function: Some(function),
            source_name,
            emitted_reified_parameters: Default::default(),
        }
    }

    pub fn synthetic(source_name: String) -> Self {
        Self {
            function: None,
            source_name,
            emitted_reified_parameters: Default::default(),
        }
    }

    pub fn with_emitted_reified_parameters(
        mut self,
        parameters: std::collections::HashSet<String>,
    ) -> Self {
        self.emitted_reified_parameters = parameters;
        self
    }
}

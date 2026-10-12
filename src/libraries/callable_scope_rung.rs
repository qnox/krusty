/// The source-name scope rung through which an unqualified callable became visible.
///
/// This is call-site selection data, not declaration provenance: compiling the same declaration
/// into the current module or a dependency must not change whether it participates as a current-
/// package declaration or an import. Receiver-call lookup stamps this value onto its cloned
/// candidates before overload selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CallableScopeRung {
    /// The callable was obtained outside an import-scoped lookup (members and direct provider
    /// queries use this value).
    #[default]
    Unscoped,
    ExplicitImport,
    CurrentPackage,
    Import,
}

impl CallableScopeRung {
    /// Stable nearest-first ordering for overload selection. `scope_rank` distinguishes levels
    /// within one semantic rung; it is not comparable across these families (a local and a
    /// current-package declaration can both carry numeric rank zero).
    pub(crate) fn selection_rank(self) -> u8 {
        match self {
            Self::Unscoped => 0,
            Self::ExplicitImport => 1,
            Self::CurrentPackage => 2,
            Self::Import => 3,
        }
    }

    pub fn is_import(self) -> bool {
        matches!(self, Self::ExplicitImport | Self::Import)
    }
}

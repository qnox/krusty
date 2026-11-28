//! The language-feature policy a parsed file carries into later phases.
//!
//! Each field is projected once from the compilation's language features (see
//! `parser::file_features`), so parsing, checking and lowering read the same decision.

/// One file's language-feature decisions.
#[derive(Default)]
pub struct FileLanguagePolicy {
    /// `+CallableReferencesToContextual`: a reference to a declaration with context parameters
    /// selects those arguments from the reference site and exposes only the value parameters.
    pub callable_references_to_contextual: bool,
    /// `+ReferencesToSyntheticJavaProperties`: a callable reference may select a property derived
    /// from a Java getter. Without it kotlinc reports the selected reference as unsupported.
    pub references_to_synthetic_java_properties: bool,
}

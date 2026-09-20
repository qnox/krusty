//! What a platform provider is handed at the Pass-1 boundary, and what it may report back.
//!
//! These three types are the whole of the frontend↔provider contract that is not a query: the
//! foreign-source view a provider is asked to normalize, and the two failures it may answer with.
//! They carry no source text, no parser state and no coordinates a provider could retain as
//! identity — the frontend owns diagnostic rendering, and a provider states an input index and a
//! semantic reason.

/// Failure while a platform provider inventories non-Kotlin source headers that belong to the
/// current source module. The frontend owns diagnostic rendering, so providers report the input
/// index and a semantic reason without retaining source text or parser state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceHeaderError {
    pub source: usize,
    pub message: String,
}

/// A dependency/provider failure discovered before the frontend may query declarations.
/// Providers retain their typed ingestion error internally and expose its stable diagnostic at this
/// boundary; the frontend reports it once and does not enter signature collection with a partial
/// symbol source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformInitializationError {
    pub message: String,
}

/// One foreign-language source whose declaration headers a target provider must normalize before
/// Kotlin signature solving. This is a bounded Pass-1 input view: providers may parse `text` during
/// the call, but the semantic result must not retain it or use its coordinates as symbol identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlatformSourceHeaderInput<'a> {
    pub source: usize,
    pub file_stem: Option<&'a str>,
    pub text: &'a str,
}

//! Frontend entry points.
//!
//! Source analysis: lexing, parsing, signature collection, and checking.

use crate::ast::File;
use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::libraries::{EmptySymbolSource, SemanticPlatform};
pub(crate) use crate::resolve::ClassSig as FrontendClassSig;
pub use crate::resolve::SymbolTable as FrontendSymbols;
pub use crate::resolve::TypeInfo as FrontendTypeInfo;
pub use crate::resolve::{
    check_file, check_file_at, collect_signatures, collect_signatures_with_cp,
};
pub(crate) use crate::resolve::{
    pick_overload, qualified_path, typeref_leaf, ClassNames, CtorDefaultValue,
    DelegateGetValueTarget, DestructureComponentTarget, ExprLowering, InlineCall, InvokeKind,
    IteratorDispatchTarget, LambdaCapture, LambdaInfo, ReceiverLambda, ResolvedCall,
    ResolvedConstructor, ResolvedLocalFunctionCall, ResolvedMember, ResolvedModuleTopLevelCall,
    Signature, StmtLowering,
};

/// A single parsed file together with the frontend facts needed by a backend.
pub struct CheckedFile<'a> {
    pub file: &'a File,
    pub file_index: u32,
    pub info: &'a FrontendTypeInfo,
    pub symbols: &'a FrontendSymbols,
}

/// Lex and parse one source string with an explicit feature set.
pub fn parse_source(src: &str, features: &LangFeatures, diags: &mut DiagSink) -> File {
    let tokens = crate::lexer::lex(src, diags);
    crate::parser::parse_with_features(src, &tokens, diags, features)
}

/// Lex and parse one source string after reading language-feature directives from the source.
pub fn parse_source_with_detected_features(src: &str, diags: &mut DiagSink) -> File {
    let features = LangFeatures::from_source(src);
    parse_source(src, &features, diags)
}

/// Parse a single source and run signature collection plus checking against `platform`.
pub fn analyze_source(
    src: &str,
    platform: Box<dyn SemanticPlatform>,
    diags: &mut DiagSink,
) -> (File, Option<FrontendSymbols>, Option<FrontendTypeInfo>) {
    let mut files = vec![parse_source_with_detected_features(src, diags)];
    if diags.has_errors() {
        return (files.pop().unwrap_or_default(), None, None);
    }

    let mut syms = collect_signatures_with_cp(&files, platform, diags);
    if diags.has_errors() {
        return (files.pop().unwrap_or_default(), Some(syms), None);
    }

    let info = check_file(&files[0], &mut syms, diags);
    (files.pop().unwrap_or_default(), Some(syms), Some(info))
}

/// Parse and check a source with no external libraries.
pub fn analyze_source_standalone(
    src: &str,
    diags: &mut DiagSink,
) -> (File, Option<FrontendSymbols>, Option<FrontendTypeInfo>) {
    analyze_source(src, Box::new(EmptySymbolSource), diags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_analysis_accepts_simple_function() {
        let mut diags = DiagSink::new();
        let (_file, syms, info) =
            analyze_source_standalone("fun box(): String = \"OK\"", &mut diags);
        assert!(!diags.has_errors(), "{:?}", diags.diags);
        assert!(syms.is_some());
        assert!(info.is_some());
    }

    #[test]
    fn standalone_analysis_reports_checker_errors() {
        let mut diags = DiagSink::new();
        let (_file, syms, info) = analyze_source_standalone("fun f(): Int = \"no\"", &mut diags);
        assert!(diags.has_errors());
        assert!(syms.is_some());
        assert!(info.is_some());
    }
}

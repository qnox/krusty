//! Temporary annotation identities for checkers constructed before signature finalization.
//!
//! The common annotation path never chooses a declaration by origin. This bridge exists solely for
//! the public single-file `analyze_source` / `check_file*` APIs, whose legacy checker has no
//! [`crate::fir::ResolvedModuleIndex`] yet. Delete it when those entry points construct their checker
//! from the finalized declaration index like production Pass 2.

use super::{AnnotationRef, CheckerModuleSymbols, TypeName};

pub(super) fn resolved_annotation(
    module: &CheckerModuleSymbols<'_>,
    file_index: u32,
    annotation: &AnnotationRef,
) -> Option<TypeName> {
    module
        .legacy_symbols()?
        .resolved_annotation(file_index, annotation)
}

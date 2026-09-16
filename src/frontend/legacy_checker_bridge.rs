//! Compatibility entry into the parser-keyed checker when no finalized index exists.
//!
//! Successful source-set inspection consumes [`crate::fir::ResolvedModuleIndex`] through
//! `check_source_set_skipping_with_index`. This bridge remains only for failed signature recovery
//! and the public parsed-source compatibility API; neither caller has a finalized index to bind.

use crate::ast::File;
use crate::diag::DiagSink;

use super::{FrontendSymbols, FrontendTypeInfo};

pub(super) fn check_source_set_skipping(
    files: &[File],
    symbols: &mut FrontendSymbols,
    skip: &[bool],
    checked_count: usize,
    diags: &mut DiagSink,
) -> Vec<Option<FrontendTypeInfo>> {
    files
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if index >= checked_count || skip.get(index).copied().unwrap_or(false) {
                None
            } else {
                diags.set_file(index as u32);
                Some(crate::resolve::check_preinferred_file_in_source_set(
                    files,
                    index as u32,
                    symbols,
                    diags,
                ))
            }
        })
        .collect()
}

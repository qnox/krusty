//! Final normalization of one JVM provider package-function candidate.
//!
//! Coarse semantic roles come from the complete declaration plus its exact input provenance. An
//! authenticated common declaration identity is then allowed to refine that normalized record.

use crate::jvm::common_metadata::CommonExpectationIndex;
use crate::libraries::builtin_top_level_realization::attach_dependency_function_realization;
use crate::libraries::FunctionInfo;
use crate::symbol_source::SymbolNamespace;

pub(super) fn normalize_tail(
    common: &CommonExpectationIndex,
    namespace: SymbolNamespace,
    name: &str,
    compiler_builtin_owner: bool,
    candidates: &mut [FunctionInfo],
) {
    let SymbolNamespace::Package(package) = namespace else {
        return;
    };
    let candidate = candidates
        .last_mut()
        .expect("a candidate was just appended");
    attach_dependency_function_realization(package, name, compiler_builtin_owner, candidate);
    common.attach(compiler_builtin_owner, namespace, name, candidate);
}

/// Publish one receiver-less Java declaration in its classifier's static namespace while
/// retaining that source call shape for diagnostics. Later selection consumes the normalized
/// candidate and never needs to re-read JVM access flags.
pub(super) fn publish_classifier_static(
    candidates: &mut Vec<crate::libraries::LibraryMember>,
    mut member: crate::libraries::LibraryMember,
) {
    member.set_is_classifier_static(true);
    candidates.push(member);
}

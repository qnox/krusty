//! Final normalization of one JVM provider package-function candidate.
//!
//! Coarse semantic roles come from the complete declaration plus its exact input provenance. An
//! authenticated common declaration identity is then allowed to refine that normalized record.

use crate::jvm::common_metadata::CommonExpectationIndex;
use crate::libraries::builtin_top_level_realization::attach_dependency_function_realization;
use crate::libraries::FunctionInfo;
use crate::symbol_source::SymbolNamespace;

pub(super) fn normalize_tail(
    classpath: &crate::jvm::classpath::Classpath,
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
    // A stdlib typealias is declaration evidence, not a physical descriptor. Its expanded source
    // classifier remains provider-owned; the common matcher must not learn Java implementation names.
    let array_list =
        (compiler_builtin_owner && package.matches("kotlin/collections") && name == "arrayListOf")
            .then(|| {
                classpath.paired_stdlib_type_alias_expansion(crate::types::type_name(
                    "kotlin/collections/ArrayList",
                ))
            })
            .flatten()
            .and_then(|alias| {
                let [formal] = alias.formals.as_slice() else {
                    return None;
                };
                let element = crate::types::Ty::ty_param(
                    formal,
                    crate::types::Ty::nullable(crate::types::Ty::obj("kotlin/Any")),
                );
                (alias.expansion == crate::types::Ty::obj_args_name(alias.target, &[element]))
                    .then_some(alias.target)
            });
    attach_dependency_function_realization(
        package,
        name,
        compiler_builtin_owner,
        candidate,
        array_list,
    );
    if let Err(error) = common.attach(compiler_builtin_owner, namespace, name, candidate) {
        classpath.defer_common_expectation_error(error);
    }
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

//! `RETURN_TYPE_MISMATCH_ON_OVERRIDE`: an overriding function's result must be a subtype of the
//! result of every member it overrides, as that member is seen from the overriding class.
//!
//! The overridden member is the hierarchy declaration whose complete input shape the
//! implementation matches. Its result is compared with type arguments substituted along the
//! inheritance path and with a Java result's flexibility read at its lower bound, so a
//! `Supplier<String>.get()` override must return a `String`. kotlinc names the overridden member's
//! owner as the classifier that applies the substitution: the nearest class on the path whose
//! direct supertype carries type arguments, or the declaring classifier when no supertype does.

use crate::ast::{ClassDecl, DeclId};
use crate::libraries::{FunctionInfo, SourceMember};
use crate::types::{Ty, TyKind, TypeName, Visibility};

use super::{declaration_rendering, selected_result_publication, Checker};

/// One overriding method of the class being checked.
pub(super) struct OverrideResult {
    /// The method's index among its class's methods.
    pub(super) method: usize,
    pub(super) class: (DeclId, TypeName),
    pub(super) identity: (SourceMember, Option<crate::fir::DeclarationId>),
}

/// Overrides whose result is inferred from a body not checked yet. They are compared once the
/// class's method bodies have published their results.
#[derive(Default)]
pub(super) struct DeferredOverrideResults(Vec<(OverrideResult, FunctionInfo, Vec<FunctionInfo>)>);

impl Checker<'_> {
    /// The member declarations named `name` across `direct_supertypes`, each substituted at the
    /// supertype it is reached through.
    pub(super) fn override_candidates(
        &self,
        direct_supertypes: &[Ty],
        name: &str,
    ) -> Vec<FunctionInfo> {
        let source = self.fed_source();
        direct_supertypes
            .iter()
            .flat_map(|supertype| {
                crate::symbol_resolver::member_declarations_in_hierarchy(&source, *supertype, name)
                    .into_parts()
                    .0
                    .overloads
            })
            .collect()
    }

    /// Report the first overridden member of `implementation`, in supertype order, whose result
    /// does not accept the implementation's result; kotlinc reports one mismatch per override. An override whose result its body has
    /// yet to infer waits in `deferred`.
    pub(super) fn check_override_results(
        &mut self,
        class: &ClassDecl,
        result: OverrideResult,
        (implementation, candidates): (&FunctionInfo, Vec<FunctionInfo>),
        deferred: &mut DeferredOverrideResults,
    ) {
        let actual = implementation
            .ret
            .apply(implementation.callable.ret)
            .canonical_semantic();
        if matches!(actual.kind(), TyKind::Pending) {
            deferred
                .0
                .push((result, implementation.clone(), candidates));
            return;
        }
        self.report_override_results(class, &result, implementation, &candidates, actual);
    }

    /// Compare the overrides `deferred` with the results their checked bodies published.
    pub(super) fn check_deferred_override_results(
        &mut self,
        class: &ClassDecl,
        deferred: DeferredOverrideResults,
    ) {
        for (result, implementation, candidates) in deferred.0 {
            let (source_member, declaration) = result.identity;
            if let Some(actual) =
                self.checked_source_member_result(Some(source_member), declaration)
            {
                // A body's `null` result publishes as `Nothing?`.
                let actual = if actual == Ty::Null {
                    Ty::nullable(Ty::Nothing)
                } else {
                    actual
                };
                self.report_override_results(class, &result, &implementation, &candidates, actual);
            }
        }
    }

    fn report_override_results(
        &mut self,
        class: &ClassDecl,
        result: &OverrideResult,
        implementation: &FunctionInfo,
        candidates: &[FunctionInfo],
        actual: Ty,
    ) {
        let function = &class.methods[result.method];
        let source = self.fed_source();
        if actual == Ty::Error {
            return;
        }
        let mut mismatch = None;
        for candidate in candidates.iter().filter(|candidate| {
            candidate.visibility != Visibility::Private
                && !candidate.flags.is_final
                && crate::symbol_resolver::function_input_shapes_match(
                    &source,
                    candidate,
                    implementation,
                )
        }) {
            let expected = selected_result_publication::inherited_result(
                &candidate.callable,
                candidate.ret.apply(candidate.callable.ret),
            );
            if expected == Ty::Error
                || crate::symbol_resolver::checked_function_result_refines(
                    &source,
                    (implementation, actual),
                    (candidate, expected),
                )
            {
                continue;
            }
            let declaring = candidate
                .callable
                .declaration_owner
                .map_or(candidate.callable.owner, |owner| owner.name());
            let owner = self.substituted_member_owner(result.class, declaring);
            let message = format!(
                "return type of '{}' is not a subtype of the return type of the overridden \
                 member '{}' defined in '{owner}'.",
                declaration_rendering::declared_function(
                    &source,
                    &with_result(implementation, actual)
                ),
                declaration_rendering::declared_function(
                    &source,
                    &with_result(candidate, expected)
                ),
            );
            mismatch = Some(message);
            break;
        }
        if let Some(message) = mismatch {
            let span = function
                .ret
                .as_ref()
                .map_or(function.name_span, |result| result.span);
            self.diags.error(span, message);
        }
    }

    /// The classifier kotlinc names as the owner of `declaring`'s member seen from `class`: the
    /// nearest class on an inheritance path whose direct supertype on that path carries type
    /// arguments, else `declaring` itself.
    fn substituted_member_owner(&self, class: (DeclId, TypeName), declaring: TypeName) -> String {
        let (declaration, internal) = class;
        let mut queue = std::collections::VecDeque::from([(Ty::obj_name(internal), None)]);
        let mut seen = crate::fx_hash::HashSet::default();
        while let Some((current, substituting)) = queue.pop_front() {
            let Some(current_name) = current.kotlin_class_internal() else {
                continue;
            };
            if current_name == declaring {
                return match substituting {
                    Some(owner) if owner == internal => self.declared_classifier_label(
                        declaration,
                        internal,
                        self.file.is_anonymous_object_class(declaration),
                    ),
                    Some(owner) => super::access_control::owner_display(owner),
                    None => super::access_control::owner_display(declaring),
                };
            }
            if !seen.insert(current_name) {
                continue;
            }
            let supertypes = crate::assignable::with_implicit_any(
                current_name,
                <Self as crate::assignable::TypeOracle>::direct_supertypes(self, current),
            );
            for supertype in supertypes {
                let substitutes = !supertype.non_null().type_args().is_empty();
                let next = substituting.or(substitutes.then_some(current_name));
                queue.push_back((supertype.non_null(), next));
            }
        }
        super::access_control::owner_display(declaring)
    }
}

/// `function` as rendered with the result `ret`: an inferred result once its body is checked, or
/// an overridden Java result at its lower bound.
fn with_result(function: &FunctionInfo, ret: Ty) -> FunctionInfo {
    let mut function = function.clone();
    function.callable.ret = ret;
    if let Some(signature) = function.generic_sig.as_mut() {
        signature.ret = ret;
    }
    function
}

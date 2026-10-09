//! Collection-literal syntax: the language-defined factory lookup a `[…]` expression resolves
//! through, and its meaning while `CollectionLiterals` is off.
//!
//! A collection literal first offers the expected classifier's companion `operator fun of`.
//! Kotlin additionally defines qualified standard-library factories for its built-in collection
//! interfaces. This module owns only that syntax-to-symbol convention; candidate applicability,
//! overload selection, generic inference, and call commitment remain in the ordinary resolver.

use super::*;
use crate::types::{type_name, TypeName};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StandardCollectionFactory {
    package: TypeName,
    callable: &'static str,
}

/// Return the qualified standard-library factory associated with a semantic classifier identity.
///
/// Arrays are deliberately absent: the checker handles their compiler-synthetic construction
/// before consulting this table. Matching is by interned qualified identity, never source spelling.
fn standard_factory(classifier: TypeName) -> Option<StandardCollectionFactory> {
    let (package, callable) = if classifier.matches("kotlin/collections/List") {
        ("kotlin/collections", "listOf")
    } else if classifier.matches("kotlin/collections/MutableList") {
        ("kotlin/collections", "mutableListOf")
    } else if classifier.matches("kotlin/collections/Set") {
        ("kotlin/collections", "setOf")
    } else if classifier.matches("kotlin/collections/MutableSet") {
        ("kotlin/collections", "mutableSetOf")
    } else if classifier.matches("kotlin/sequences/Sequence") {
        ("kotlin/sequences", "sequenceOf")
    } else {
        return None;
    };
    Some(StandardCollectionFactory {
        package: type_name(package),
        callable,
    })
}

/// With no expected classifier Kotlin's collection-literal fallback is an immutable `List`.
fn default_factory() -> StandardCollectionFactory {
    StandardCollectionFactory {
        package: type_name("kotlin/collections"),
        callable: "listOf",
    }
}

impl Checker<'_> {
    pub(super) fn check_collection_literal(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        args: &[ExprId],
        span: Span,
        expected: Option<Ty>,
    ) -> Ty {
        // Expected-type rechecking replaces the literal's expectation-free `listOf` probe with its
        // selected construction. Type substitutions and source-slot mapping belong to that target,
        // just like `resolved_calls`; retaining either from the discarded probe can attach a generic
        // argument to a non-generic companion `of` declaration in checked FIR.
        self.resolved_call_type_args.remove(&call);
        self.resolved_call_type_argument_bounds.remove(&call);
        self.resolved_call_arg_slots.remove(&call);
        let expected = expected
            .filter(|expected| *expected != Ty::Error)
            .map(Ty::non_null);
        if let Some(message) = self
            .file
            .language_gates
            .collection_literals
            .unsupported_message()
            .cloned()
            .filter(|_| !self.in_annotation_context())
        {
            return self.check_unsupported_array_literal(scope, args, span, expected, &message);
        }

        // Array literals are compiler-provided constructions, selected from the expected array
        // classifier rather than from the provisional parser spelling. Primitive and unsigned
        // arrays use their specialized vararg creator; `Array<T>` uses the reference creator.
        if let Some((expected, element)) = expected.zip(expected.and_then(Ty::array_elem)) {
            let synthetic = if matches!(expected, Ty::Obj(owner, _) if owner.matches("kotlin/Array"))
            {
                crate::synthetics::lookup("arrayOf")
            } else {
                crate::synthetics::by_kind(crate::synthetics::SyntheticKind::PrimitiveVararg(
                    element,
                ))
            };
            let Some(synthetic) = synthetic else {
                self.diags.error(
                    span,
                    format!(
                        "collection literal cannot construct '{}'",
                        expected.source_name()
                    ),
                );
                return Ty::Error;
            };
            let Some(result) =
                self.check_array_builtin(scope, call, synthetic, args, span, Some(element))
            else {
                return Ty::Error;
            };
            self.expr_lowers
                .insert(call, ExprLowering::CompilerSynthetic(synthetic.kind));
            return result;
        }

        let classifier = expected.and_then(Ty::kotlin_class_internal);
        let standard_element_expectation = classifier
            .and_then(standard_factory)
            .and_then(|_| expected.and_then(|expected| expected.type_args().first().copied()))
            .map(|element| element.projection_inner().unwrap_or(element));
        let argument_types = args
            .iter()
            .map(|&argument| match standard_element_expectation {
                Some(element) => self.expr_expected(scope, argument, element),
                None => self.expr(scope, argument),
            })
            .collect::<Vec<_>>();

        // Keep the value facet selected for the expected classifier. The declaration owner is not
        // necessarily this singleton: a companion may inherit `of` from an ordinary base class.
        // The receiver therefore has to travel independently from the selected callable identity.
        let companion_dispatch =
            classifier.and_then(|classifier| self.classifier_singleton_value(classifier));

        // The expected classifier's companion operator is the first language strategy. Its raw
        // declarations enter the same applicability and overload-selection engine as an ordinary
        // call; only declarations explicitly marked `operator` are eligible for this syntax.
        let companion_candidates = classifier
            .map(|classifier| {
                let mut candidates = self
                    .resolver()
                    .classifier_call_candidates(classifier, "of")
                    .map(|(_, candidates)| candidates)
                    .unwrap_or_default();
                candidates.extend(
                    self.resolver()
                        .classifier_associated_callables(classifier, "of"),
                );
                candidates
                    .into_iter()
                    .filter(|candidate| candidate.flags.operator)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        crate::trace_compiler!(
            "resolve",
            "collection literal expected={expected:?} classifier={classifier:?} companion_candidates={:?}",
            companion_candidates
                .iter()
                .map(|candidate| (
                    candidate.callable.owner,
                    candidate.callable.name.as_str(),
                    candidate.semantic_params(),
                    candidate.callable.ret,
                ))
                .collect::<Vec<_>>(),
        );
        let companion_selection = (!companion_candidates.is_empty()).then(|| {
            self.select_callable_candidate(
                scope,
                CallArgs {
                    call,
                    args,
                    arg_tys: &argument_types,
                },
                &[],
                classifier.map(Ty::obj_name),
                CallResultConstraint::direct(expected),
                companion_candidates,
            )
        });
        match companion_selection.flatten() {
            Some(CallableCandidateSelection::Selected(selected)) => {
                let selected = *selected;
                if !self.member_accessible(selected.visibility, selected.callable.owner) {
                    self.reject_if_inaccessible(
                        selected.visibility,
                        "of",
                        selected.callable.owner,
                        span,
                    );
                    return Ty::Error;
                }
                // An associated `operator fun of` has no value operand: it is recorded as the
                // receiver-less call its provider shape already is.
                if selected.kind == crate::libraries::FnKind::TopLevel {
                    let argument_names = self.file.call_arg_names.get(&call.0).cloned();
                    return self.finish_top_level_call(
                        scope,
                        call,
                        args,
                        &argument_types,
                        argument_names.as_deref(),
                        selected,
                        &[],
                        None,
                    );
                }
                let Some(shape) = self.contextual_call_shape(
                    scope,
                    &selected.applied_params(),
                    &selected.call_sig,
                    selected.context_count,
                    None,
                ) else {
                    return Ty::Error;
                };
                if !self.expect_selected_call_args(
                    scope,
                    CallArgs {
                        call,
                        args,
                        arg_tys: &argument_types,
                    },
                    &shape.params,
                    &shape.params,
                    &shape.call_sig,
                    None,
                ) {
                    return Ty::Error;
                }
                if let Some(signature) = selected.generic_sig.as_ref() {
                    let resolved = signature
                        .formals
                        .iter()
                        .map(|formal| selected.bindings.get(formal).copied())
                        .collect::<Vec<_>>();
                    if !resolved.is_empty() && resolved.iter().all(Option::is_some) {
                        self.resolved_call_type_args.insert(call, resolved);
                    }
                }
                self.record_selected_sam_arguments(scope, args, &selected.applied_params());
                let result = selected.callable.ret;
                let mut member = selected.member_with_return(result);
                // An `operator fun of` is an ordinary member of the classifier's value facet, and
                // this syntax writes no receiver expression to carry that instance. Keep the
                // already-resolved singleton independently of the declaration owner: an inherited
                // operator's owner is its base class, not the companion object that dispatches it.
                if member.singleton_dispatch.is_none()
                    && member.implicit_classifier_callable.is_none()
                {
                    member.singleton_dispatch = companion_dispatch.map(|singleton| {
                        Box::new(crate::libraries::SingletonDispatch {
                            classifier: singleton.classifier,
                        })
                    });
                }
                self.resolved_calls
                    .insert(call, ResolvedCall::Companion(member));
                return result;
            }
            Some(CallableCandidateSelection::MissingContext(_)) => {
                self.diags.error(
                    span,
                    "no implicit value is available for collection literal factory context parameters"
                        .to_string(),
                );
                return Ty::Error;
            }
            Some(CallableCandidateSelection::Ambiguous(_)) => {
                self.diags.error(
                    span,
                    "collection literal factory overload ambiguity".to_string(),
                );
                return Ty::Error;
            }
            None => {}
        }

        // Built-in collection interfaces use Kotlin's qualified factory convention after the
        // companion strategy. With no expected type the language fallback is immutable `List`.
        let factory = classifier
            .and_then(standard_factory)
            .or_else(|| expected.is_none().then(default_factory));
        if let Some(factory) = factory {
            let candidates = self
                .resolver_in_scope(std::slice::from_ref(&factory.package))
                .top_level_candidates(factory.callable);
            let selection = self.select_callable_candidate(
                scope,
                CallArgs {
                    call,
                    args,
                    arg_tys: &argument_types,
                },
                &[],
                None,
                CallResultConstraint::direct(expected),
                candidates,
            );
            match selection {
                Some(CallableCandidateSelection::Selected(selected)) => {
                    return self.finish_top_level_call(
                        scope,
                        call,
                        args,
                        &argument_types,
                        None,
                        *selected,
                        &[],
                        None,
                    );
                }
                Some(CallableCandidateSelection::MissingContext(_)) => {
                    self.diags.error(
                        span,
                        "no implicit value is available for collection literal factory context parameters"
                            .to_string(),
                    );
                    return Ty::Error;
                }
                Some(CallableCandidateSelection::Ambiguous(_)) => {
                    self.diags.error(
                        span,
                        "collection literal factory overload ambiguity".to_string(),
                    );
                    return Ty::Error;
                }
                None => {
                    self.diags.error(
                        span,
                        format!(
                            "no applicable standard collection factory '{}'",
                            factory.callable
                        ),
                    );
                    return Ty::Error;
                }
            }
        }

        self.diags.error(
            span,
            match expected {
                Some(expected) => format!(
                    "no applicable 'operator fun of' for collection literal type '{}'",
                    expected.source_name()
                ),
                None => "not enough information to infer collection literal type".to_string(),
            },
        );
        Ty::Error
    }

    /// Whether a selected parameter can supply a language-defined construction strategy for a
    /// collection literal. This is intentionally a target-availability query, not a second call
    /// resolver: overload applicability for the factory itself is checked exactly once when the
    /// enclosing callable has won and `check_collection_literal` commits the expression.
    pub(super) fn collection_literal_target_available(&self, expected: Ty) -> bool {
        let expected = expected.non_null();
        if expected.array_elem().is_some() {
            return true;
        }
        let Some(classifier) = expected.kotlin_class_internal() else {
            return false;
        };
        if standard_factory(classifier).is_some() {
            return true;
        }
        self.resolver()
            .classifier_call_candidates(classifier, "of")
            .into_iter()
            .flat_map(|(_, candidates)| candidates)
            .chain(
                self.resolver()
                    .classifier_associated_callables(classifier, "of"),
            )
            .any(|candidate| candidate.flags.operator)
    }

    /// An annotation context, where `[…]` is the array literal of every language version: anywhere
    /// in an annotation application's argument, and in an annotation class's constructor, whose
    /// parameter defaults are the only code such a class can carry and are checked inside it.
    fn in_annotation_context(&self) -> bool {
        self.annotation_argument_depth > 0
            || self
                .lexical_source_class_names()
                .first()
                .and_then(|&class| self.fed_source().classifier(class))
                .is_some_and(|class| class.is_annotation())
    }

    /// `CollectionLiterals` off: kotlinc reads `[…]` outside an annotation as the array literal it
    /// had before the feature and rejects it. The literal is still typed, so a context expecting
    /// something other than that array also reports a mismatch: `Array<E>` of the elements' common
    /// supertype `E` (the expected array's element type, else `Any?`, when there are none), the
    /// specialized primitive array when that is what the context expects.
    fn check_unsupported_array_literal(
        &mut self,
        scope: &CheckerScope<'_>,
        args: &[ExprId],
        span: Span,
        expected: Option<Ty>,
        unsupported: &str,
    ) -> Ty {
        let expected_element = expected.and_then(Ty::array_elem);
        let elements = args
            .iter()
            .map(|&argument| {
                let ty = match expected_element {
                    Some(element) => self.expr_expected(scope, argument, element),
                    None => self.expr(scope, argument),
                };
                (ty, argument)
            })
            .collect::<Vec<_>>();
        let element = super::conditional_branch::join_results(self, scope, None, elements)
            .or(expected_element)
            .unwrap_or_else(|| Ty::nullable(Ty::obj_name(crate::types::wk::any())));
        let primitive_expected = expected
            .and_then(Ty::kotlin_class_internal)
            .is_some_and(|classifier| crate::types::prim_array_element(classifier).is_some());
        for message in [
            "array literals outside of annotations are unsupported.",
            unsupported,
        ] {
            self.diags.error_kind(
                span,
                crate::diag::DiagnosticKind::ExpressionChecker,
                message,
            );
        }
        if primitive_expected {
            Ty::array(element)
        } else {
            Ty::obj_args_name(crate::types::wk::array(), &[element])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_factories_are_selected_by_qualified_classifier_identity() {
        assert_eq!(
            standard_factory(type_name("kotlin/collections/List")),
            Some(StandardCollectionFactory {
                package: type_name("kotlin/collections"),
                callable: "listOf",
            })
        );
        assert_eq!(standard_factory(type_name("sample/List")), None);
    }
}

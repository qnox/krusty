//! Language-defined factory lookup for collection-literal syntax.
//!
//! A collection literal first offers the expected classifier's companion `operator fun of`.
//! Kotlin additionally defines qualified standard-library factories for its built-in collection
//! interfaces. This module owns only that syntax-to-symbol convention; candidate applicability,
//! overload selection, generic inference, and call commitment remain in the ordinary resolver.

use crate::ast::ExprId;
use crate::diag::Span;
use crate::libraries::FunctionInfo;
use crate::symbol_resolver::CallArgKind;
use crate::types::{type_name, Ty, TypeName};

use super::Checker;

/// See [`Checker::collection_literal_bound`].
pub(super) enum LiteralBound {
    /// The literal was not passed for a type variable, or nothing else bounds it.
    Unbounded,
    Single(Ty),
    Ambiguous,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StandardCollectionFactory {
    pub(super) package: TypeName,
    pub(super) callable: &'static str,
}

/// Return the qualified standard-library factory associated with a semantic classifier identity.
///
/// Arrays are deliberately absent: the checker handles their compiler-synthetic construction
/// before consulting this table. Matching is by interned qualified identity, never source spelling.
pub(super) fn standard_factory(classifier: TypeName) -> Option<StandardCollectionFactory> {
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
pub(super) fn default_factory() -> StandardCollectionFactory {
    StandardCollectionFactory {
        package: type_name("kotlin/collections"),
        callable: "listOf",
    }
}

impl Checker<'_> {
    /// Whether a selected parameter can supply a language-defined construction strategy for a
    /// collection literal. This is intentionally a target-availability query, not a second call
    /// resolver: overload applicability for the factory itself is checked exactly once when the
    /// enclosing callable has won and `check_collection_literal` commits the expression.
    pub(super) fn collection_literal_target_available(&self, expected: Ty) -> bool {
        let expected = expected.non_null();
        expected.array_elem().is_some()
            || expected.kotlin_class_internal().is_some_and(|classifier| {
                self.classifier_constructs_collection_literals(classifier)
            })
    }

    fn classifier_constructs_collection_literals(&self, classifier: TypeName) -> bool {
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

    /// The kind of a collection-literal argument before it is postponed against a call's
    /// candidates: its provisional reading, with no factory classifier admitted yet.
    /// [`Self::postponed_collection_literal_kinds`] fills them in from the candidates' parameters.
    pub(super) fn unpostponed_collection_literal_kind(
        &self,
        argument: ExprId,
        provisional: Ty,
    ) -> Option<CallArgKind> {
        self.file
            .collection_literal_calls
            .contains(&argument.0)
            .then(|| CallArgKind::CollectionLiteral {
                provisional,
                factory_classifiers: std::sync::Arc::from([]),
            })
    }

    /// The bounds of a type variable that a literal passed for it could be built as, in bound
    /// order: its lower bounds, then its expected upper bound, one per classifier that constructs
    /// literals.
    pub(super) fn literal_bound_classifiers(&self, lowers: &[Ty], upper: Option<Ty>) -> Vec<Ty> {
        let mut bounds: Vec<Ty> = Vec::new();
        for bound in lowers.iter().copied().chain(upper) {
            let Some(classifier) = bound.non_null().kotlin_class_internal() else {
                continue;
            };
            if bounds
                .iter()
                .all(|known| known.non_null().kotlin_class_internal() != Some(classifier))
                && self.classifier_constructs_collection_literals(classifier)
            {
                bounds.push(bound);
            }
        }
        bounds
    }

    /// What the bounds of the type variable a literal was passed for say about its type. An
    /// ambiguity is decided when the literal's call selection wins; it is reported once, here.
    pub(super) fn collection_literal_bound(&mut self, call: ExprId, span: Span) -> LiteralBound {
        let Some(bounds) = self
            .collection_literal_ambiguities
            .borrow()
            .get(&call)
            .cloned()
        else {
            return LiteralBound::Unbounded;
        };
        let [bound] = bounds[..] else {
            if self.reported_ambiguous_literals.insert(call) {
                let mut message = "type of collection literal is ambiguous. Multiple candidates \
                                   declare operator 'of':"
                    .to_string();
                for bound in bounds {
                    message.push('\n');
                    message.push_str(
                        &self.literal_candidate_declaration(
                            bound
                                .non_null()
                                .kotlin_class_internal()
                                .expect("a literal bound names a classifier"),
                        ),
                    );
                }
                self.diags.error_named(
                    span,
                    crate::diag::names::AMBIGUOUS_COLLECTION_LITERAL,
                    message,
                );
            }
            return LiteralBound::Ambiguous;
        };
        LiteralBound::Single(bound)
    }

    /// The classifier the way kotlinc's declaration renderer prints a candidate: its keyword,
    /// name, type parameters and direct supertypes, with no visibility or modality.
    fn literal_candidate_declaration(&self, classifier: TypeName) -> String {
        let Some(library) = self.resolver().classifier(classifier) else {
            return format!(
                "internal error: candidate classifier '{}' is unresolved",
                classifier.render()
            );
        };
        match render_candidate_classifier(&library, classifier) {
            Ok(rendered) => rendered,
            Err(reason) => format!(
                "internal error: candidate classifier '{}' {reason}",
                classifier.render()
            ),
        }
    }

    /// Argument kinds for selecting among `candidates` with each collection literal postponed.
    ///
    /// An expectation-free literal reads as `List<T>`, which is not evidence against a candidate
    /// whose parameter builds the literal itself. The kind instead carries the classifiers among
    /// the candidates' parameters that construct literals, so selection admits those parameters
    /// and the selected parameter then becomes the literal's expected type.
    pub(super) fn postponed_collection_literal_kinds(
        &self,
        args: &[ExprId],
        kinds: Vec<CallArgKind>,
        candidates: &[FunctionInfo],
    ) -> Vec<CallArgKind> {
        if !args
            .iter()
            .any(|arg| self.file.collection_literal_calls.contains(&arg.0))
        {
            return kinds;
        }
        let mut factory_classifiers = Vec::new();
        for parameter in candidates
            .iter()
            .flat_map(|candidate| candidate.semantic_signature().params.clone())
        {
            if let Some(classifier) = parameter.non_null().kotlin_class_internal() {
                if !factory_classifiers.contains(&classifier)
                    && self.classifier_constructs_collection_literals(classifier)
                {
                    factory_classifiers.push(classifier);
                }
            }
        }
        let factory_classifiers: std::sync::Arc<[TypeName]> = factory_classifiers.into();
        args.iter()
            .zip(kinds)
            .map(|(arg, kind)| {
                if self.file.collection_literal_calls.contains(&arg.0) {
                    CallArgKind::CollectionLiteral {
                        provisional: kind.ty(),
                        factory_classifiers: factory_classifiers.clone(),
                    }
                } else {
                    kind
                }
            })
            .collect()
    }
}

/// A classifier the way kotlinc lists it as a candidate: `class B<X> : A<X>`,
/// `data class E : D`, `interface Set<out E> : Collection<E>`. Only the shapes measured against
/// kotlinc are rendered; any other is an explicit error, never a guessed sentence.
fn render_candidate_classifier(
    library: &crate::libraries::LibraryType,
    classifier: TypeName,
) -> Result<String, &'static str> {
    use crate::libraries::TypeKind;
    use crate::types::TypeVariance;
    let keyword = match library.kind {
        TypeKind::Class if library.is_data => "data class",
        TypeKind::Class => "class",
        TypeKind::Interface if !library.is_fun_interface() => "interface",
        _ => return Err("has a kind whose candidate rendering was not measured"),
    };
    let default_bound = [Ty::nullable(Ty::obj("kotlin/Any"))];
    if library
        .type_param_bounds()
        .iter()
        .any(|bounds| !bounds.is_empty() && bounds[..] != default_bound)
    {
        return Err("has bounded type parameters, whose candidate rendering was not measured");
    }
    let parameters = library
        .type_params()
        .iter()
        .zip(library.type_param_variances())
        .map(|(name, variance)| {
            let prefix = match variance {
                TypeVariance::Invariant => "",
                TypeVariance::In => "in ",
                TypeVariance::Out => "out ",
            };
            format!("{prefix}{}", crate::types::type_parameter_source_name(name))
        })
        .collect::<Vec<_>>();
    let parameters = if parameters.is_empty() {
        String::new()
    } else {
        format!("<{}>", parameters.join(", "))
    };
    let mut supertypes = library
        .supertype_templates
        .iter()
        .filter(|supertype| {
            !supertype
                .kotlin_class_internal()
                .is_some_and(|class| class.matches("kotlin/Any"))
        })
        .map(|supertype| supertype.source_name())
        .collect::<Vec<_>>();
    if supertypes.is_empty() {
        supertypes.push("Any".to_string());
    }
    let name = classifier.segment();
    Ok(format!(
        "{keyword} {name}{parameters} : {}",
        supertypes.join(", ")
    ))
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

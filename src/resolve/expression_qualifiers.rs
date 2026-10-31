//! Expression-position qualifier resolution: one left-to-right walk of a dotted name chain, from
//! scope-tower root classification through the symbol-source package and classifier edges.
//!
//! The walk binds each written segment once. A segment continues its predecessor's qualifier
//! ([`Checker::qualifier_step`]): a package continues to a classifier or a subpackage, a classifier
//! to a nested classifier or to a typealias declared in its TYPE namespace, which is carried with
//! the classifier it expands to. The receiver chain of an access is resolved through
//! [`Checker::resolve_recorded_qualifier`], which records what every segment bound
//! ([`BoundQualifierSegment`]); the checks that later visit the chain, and the member selection
//! through it, read that record instead of walking the chain again.

use super::classifier_binding::SelectedTypeAlias;
use super::qualifiers::subpackage;
use super::*;
use crate::symbol_source::SymbolNamespace;

/// `FirStandaloneQualifierChecker`: a package qualifier that is not the explicit receiver of a
/// qualified access, a class literal's argument, or another qualifier's explicit parent is used as
/// an expression. A callable reference is no qualified access of kotlinc's checker context, so its
/// left-hand side is such a use too.
pub(super) const EXPRESSION_EXPECTED_PACKAGE_FOUND: &str =
    "expression expected, but package name found.";

/// What one written qualifier segment binds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum QualifierBinding {
    Package(TypeName),
    Classifier(TypeName),
}

impl QualifierBinding {
    pub(super) fn of(resolved: ResolvedQualifier) -> Option<Self> {
        match resolved {
            ResolvedQualifier::Package(package) => Some(Self::Package(package)),
            ResolvedQualifier::Classifier(internal) => Some(Self::Classifier(internal)),
            ResolvedQualifier::Value => None,
        }
    }
}

/// One edge of the walk: the qualifier a segment continues its predecessor to, and the typealias
/// declaration the segment selected when it names one (`resolved` is then the expanded classifier).
pub(super) struct QualifierStep {
    pub(super) resolved: ResolvedQualifier,
    pub(super) alias: Option<SelectedTypeAlias>,
}

/// A written segment that the walk of an access's receiver chain bound to a package or a
/// classifier, recorded once for the checks and selections that consume the chain.
#[derive(Clone, Copy, Debug)]
pub(super) struct BoundQualifierSegment {
    pub(super) binding: QualifierBinding,
    /// FIR's `typeArguments` of the segment continued as a qualifier is non-empty: its own written
    /// list, or the list a classifier predecessor carries (`QualifiedNameResolution.kt`
    /// `continueQualifier`). A package predecessor's list is not inherited.
    pub(super) type_arguments: bool,
    /// The segment names a typealias of its classifier.
    pub(super) alias: bool,
}

/// The classifier an alias expansion denotes in qualifier position. `Unit` has a distinct semantic
/// type but is still the `kotlin.Unit` object classifier there.
pub(super) fn alias_expansion_classifier(expansion: Ty) -> Option<TypeName> {
    match expansion.non_null() {
        Ty::Unit => Some(type_name("kotlin/Unit")),
        expansion => expansion.obj_internal(),
    }
}

impl Checker<'_> {
    /// Whether the root of a qualified spelling is a value in scope. A value root commits the
    /// entire spelling to ordinary expression checking. An implicit receiver supplies a value
    /// through a member property, or through an extension property that reads on it (`java` on a
    /// `KClass` receiver), at that receiver's tower level.
    pub(super) fn qualifier_root_is_value(&self, scope: &CheckerScope<'_>, name: &str) -> bool {
        let resolver = self.resolver();
        scope
            .root_value(
                name,
                |receiver| {
                    !crate::symbol_resolver::declared_member_callables(
                        &self.fed_source(),
                        receiver,
                        name,
                    )
                    .properties()
                    .is_empty()
                        || receiver.obj_internal().is_some_and(|owner| {
                            self.fed_source()
                                .classifier(owner)
                                .is_some_and(|classifier| classifier.constants.contains_key(name))
                        })
                        || !matches!(self.select_property_read(scope, receiver, name), Ok(None))
                },
                || {
                    resolver.top_level_properties(name).iter().any(|property| {
                        property.kind == crate::libraries::PropKind::TopLevel
                            && property.receiver.is_none()
                    })
                },
            )
            .is_some()
    }

    /// Walk one qualified spelling exactly once, left-to-right. This walk never performs
    /// call/constructor overload selection or retries another namespace.
    pub(super) fn qualifier(
        &self,
        scope: &CheckerScope<'_>,
        input: QualifierInput<'_>,
    ) -> Result<ResolvedQualifier, QualifierError> {
        self.walk_expression_qualifier(scope, input, |_, _, _| {})
    }

    /// Resolve the receiver chain `expression` of an access and record what each of its written
    /// segments bound. A segment the walk did not bind (after a value root or segment, or past an
    /// error) has no record, including one an earlier check of the same expression left.
    pub(super) fn resolve_recorded_qualifier(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
    ) -> Result<ResolvedQualifier, QualifierError> {
        let mut bound = Vec::new();
        let resolved = self.walk_expression_qualifier(
            scope,
            QualifierInput::Expression(expression),
            |segment, binding, alias| bound.push((segment, binding, alias)),
        );
        self.record_qualifier_chain(expression, bound);
        resolved
    }

    /// Replace the records of every written segment of the chain `expression` by `bound`, the
    /// segments its one walk bound, root first.
    pub(super) fn record_qualifier_chain(
        &mut self,
        expression: ExprId,
        bound: Vec<(ExprId, QualifierBinding, bool)>,
    ) {
        let mut segment = Some(expression);
        while let Some(current) = segment {
            self.bound_qualifier_segments.remove(&current);
            segment = match self.file.expr(current) {
                Expr::Member { receiver, .. } => Some(*receiver),
                _ => None,
            };
        }
        let mut previous = None;
        for (segment, binding, alias) in bound {
            previous = Some(self.record_qualifier_segment(segment, previous, binding, alias));
        }
    }

    /// Record the member segment `expression` that continues the recorded receiver chain
    /// `receiver` by `step`, or forget it when the step binds no qualifier.
    pub(super) fn record_qualifier_continuation(
        &mut self,
        expression: ExprId,
        receiver: ExprId,
        step: Option<&QualifierStep>,
    ) {
        self.bound_qualifier_segments.remove(&expression);
        let Some(binding) = step.and_then(|step| QualifierBinding::of(step.resolved)) else {
            return;
        };
        let Some(previous) = self.bound_qualifier_segments.get(&receiver).copied() else {
            return;
        };
        let alias = step.is_some_and(|step| step.alias.is_some());
        self.record_qualifier_segment(expression, Some(previous), binding, alias);
    }

    fn record_qualifier_segment(
        &mut self,
        expression: ExprId,
        previous: Option<BoundQualifierSegment>,
        binding: QualifierBinding,
        alias: bool,
    ) -> BoundQualifierSegment {
        let inherited = previous.is_some_and(|previous| {
            previous.type_arguments && matches!(previous.binding, QualifierBinding::Classifier(_))
        });
        let segment = BoundQualifierSegment {
            binding,
            type_arguments: self.file.call_type_args.contains_key(&expression.0) || inherited,
            alias,
        };
        self.bound_qualifier_segments.insert(expression, segment);
        segment
    }

    /// The recorded qualifier of a written segment of a resolved receiver chain.
    pub(super) fn bound_qualifier_segment(
        &self,
        expression: ExprId,
    ) -> Option<BoundQualifierSegment> {
        self.bound_qualifier_segments.get(&expression).copied()
    }

    /// Whether the receiver chain `receiver` is a classifier qualifier whose FIR type arguments are
    /// non-empty. Such a qualifier does not supply its object or companion value as a receiver
    /// (`FirTowerResolveTask.runResolverForQualifierReceiver` takes the qualifier's value only when
    /// `typeArguments` is empty), so only its static scope remains.
    pub(super) fn qualifier_value_is_hidden_by_type_arguments(&self, receiver: ExprId) -> bool {
        self.bound_qualifier_segment(receiver)
            .is_some_and(|segment| {
                segment.type_arguments && matches!(segment.binding, QualifierBinding::Classifier(_))
            })
    }

    /// The single walk: classify the root, then continue one written segment at a time, reporting
    /// every segment bound to a package or a classifier to `on_segment` in order.
    fn walk_expression_qualifier(
        &self,
        scope: &CheckerScope<'_>,
        input: QualifierInput<'_>,
        mut on_segment: impl FnMut(ExprId, QualifierBinding, bool),
    ) -> Result<ResolvedQualifier, QualifierError> {
        let mut segments = Vec::new();
        match input {
            QualifierInput::Expression(expression) => {
                qualifier_expression_segments(self.file, expression, &mut segments)?;
            }
            QualifierInput::Root(name) => {
                segments.push((None, name.to_string()));
            }
        }
        let Some((root_expression, root_name)) = segments.first() else {
            return Err(QualifierError::UnresolvedSegment {
                expression: None,
                name: String::new(),
            });
        };

        let root_is_value = self.qualifier_root_is_value(scope, root_name);
        crate::trace_compiler!(
            "resolve",
            "qualifier root={root_name} value={root_is_value}",
        );
        if root_is_value {
            return Ok(ResolvedQualifier::Value);
        }

        let root = self.qualifier_root_step(scope, *root_expression, root_name)?;
        let (mut prefix, root_alias) = (root.resolved, root.alias.is_some());
        if let (Some(expression), Some(binding)) = (*root_expression, QualifierBinding::of(prefix))
        {
            on_segment(expression, binding, root_alias);
        }
        for (expression, name) in &segments[1..] {
            let step = self.qualifier_step(prefix, *expression, name)?;
            prefix = step.resolved;
            let Some(binding) = QualifierBinding::of(prefix) else {
                return Ok(ResolvedQualifier::Value);
            };
            if let Some(expression) = *expression {
                on_segment(expression, binding, step.alias.is_some());
            }
        }
        Ok(prefix)
    }

    /// The root segment `name` of a qualifier chain, at scope-tower priority: a classifier, with the
    /// typealias declaration that named it; else a typealias in scope; else a root package, unless
    /// the segment cannot name one.
    pub(super) fn qualifier_root_step(
        &self,
        scope: &CheckerScope<'_>,
        expression: Option<ExprId>,
        name: &str,
    ) -> Result<QualifierStep, QualifierError> {
        let (selection, _, alias, _) = self.select_classifier_binding(scope, name);
        let unresolved = || QualifierError::UnresolvedSegment {
            expression,
            name: name.to_string(),
        };
        match selection {
            InheritedNestedClassifier::Found(classifier) => Ok(QualifierStep {
                resolved: ResolvedQualifier::Classifier(classifier),
                alias,
            }),
            InheritedNestedClassifier::Ambiguous => Err(QualifierError::AmbiguousRoot {
                expression,
                name: name.to_string(),
            }),
            InheritedNestedClassifier::NotFound => {
                if let Some(alias) = self.scoped_type_alias(scope, name) {
                    let classifier =
                        alias_expansion_classifier(alias.expansion).ok_or_else(unresolved)?;
                    return Ok(QualifierStep {
                        resolved: ResolvedQualifier::Classifier(classifier),
                        alias: Some(alias),
                    });
                }
                if self.segment_cannot_name_a_package(expression) {
                    return Err(unresolved());
                }
                let package =
                    subpackage(&self.fed_source(), TypeName::ROOT, name).ok_or_else(unresolved)?;
                Ok(QualifierStep {
                    resolved: ResolvedQualifier::Package(package),
                    alias: None,
                })
            }
        }
    }

    /// The typealias declaration the simple name `name` selects in `scope`: a lexical one, else a
    /// module one.
    fn scoped_type_alias(&self, scope: &CheckerScope<'_>, name: &str) -> Option<SelectedTypeAlias> {
        if let Some(alias) = scope.type_alias(name) {
            return Some(SelectedTypeAlias {
                identity: None,
                formals: alias.formals,
                expansion: alias.expansion,
                expansion_spelling: alias.expansion_spelling,
            });
        }
        let identity = self.scoped_source_alias_identity(scope, name)?;
        let binding = self.source_alias_binding(identity)?;
        Some(SelectedTypeAlias {
            identity: Some(identity),
            formals: binding.formals,
            expansion: binding.expansion,
            expansion_spelling: binding.expansion_spelling,
        })
    }

    /// Continue `prefix` by the written segment `name`.
    ///
    /// - A package continues to the classifier its provider record names (with the typealias
    ///   declaration that record carries), else to a subpackage, unless the segment cannot name a
    ///   package ([`package_qualifier_forms::segment_cannot_name_a_package`]).
    /// - A classifier with an enum entry `name` selects that entry's VALUE: a classpath entry with a
    ///   body also has a physical nested subclass (`E$ENTRY`), which expression position never
    ///   names. Otherwise it continues to its nested classifier, or to the typealias declared in
    ///   its body, which carries its own declaration identity because a nested alias of a nested
    ///   or local owner has no provider record under the owner's runtime name.
    pub(super) fn qualifier_step(
        &self,
        prefix: ResolvedQualifier,
        expression: Option<ExprId>,
        name: &str,
    ) -> Result<QualifierStep, QualifierError> {
        let unresolved = || QualifierError::UnresolvedSegment {
            expression,
            name: name.to_string(),
        };
        let classifier = |classifier: TypeName, alias: Option<SelectedTypeAlias>| QualifierStep {
            resolved: ResolvedQualifier::Classifier(classifier),
            alias,
        };
        let source = self.fed_source();
        match prefix {
            ResolvedQualifier::Value => Ok(QualifierStep {
                resolved: ResolvedQualifier::Value,
                alias: None,
            }),
            ResolvedQualifier::Package(package) => {
                let record = source.symbols(SymbolNamespace::Package(package), name);
                if let Some(internal) = record.classifier_name {
                    let alias = self
                        .selected_provider_alias(internal, record.classifier_declaration.as_ref())
                        .map_err(|()| unresolved())?;
                    Ok(classifier(internal, alias))
                } else if let Some(child) = (!self.segment_cannot_name_a_package(expression))
                    .then(|| subpackage(&source, package, name))
                    .flatten()
                {
                    Ok(QualifierStep {
                        resolved: ResolvedQualifier::Package(child),
                        alias: None,
                    })
                } else {
                    Err(unresolved())
                }
            }
            ResolvedQualifier::Classifier(owner) => {
                if self.classifier_has_enum_entry(owner, name) {
                    return Ok(QualifierStep {
                        resolved: ResolvedQualifier::Value,
                        alias: None,
                    });
                }
                let nested = self.nested_classifier(owner, name).ok_or_else(unresolved)?;
                Ok(classifier(nested.classifier, nested.alias))
            }
        }
    }

    /// Whether the simple name `expression`, which no value or classifier rung bound, names a root
    /// package: the qualifier the walk binds it to.
    /// A package qualifier used as an expression at `span`. kotlinc reports it and types the
    /// qualifier `Unit`, so the use goes on to be checked against that type: `val x: Int = pkg` is
    /// also a mismatch, and `pkg?.f()` selects `f` on `Unit`.
    pub(super) fn package_used_as_expression(&mut self, span: Span) -> Ty {
        self.diags
            .error(span, EXPRESSION_EXPECTED_PACKAGE_FOUND.to_string());
        Ty::Unit
    }

    /// Whether `package` has no callable named `name` in the scope kotlinc's
    /// `PackageQualifierReceiver` resolves through: no top-level function or property, and no
    /// classifier with a constructor.
    pub(super) fn package_lacks_callable(&self, package: TypeName, name: &str) -> bool {
        let resolver = self.resolver_in_scope(std::slice::from_ref(&package));
        if !resolver.top_level_candidates(name).is_empty()
            || !resolver.top_level_properties(name).is_empty()
        {
            return false;
        }
        let record = self
            .fed_source()
            .symbols(SymbolNamespace::Package(package), name);
        !record.classifier_name.is_some_and(|classifier| {
            self.resolver()
                .classifier(classifier)
                .is_some_and(|classifier| !classifier.constructors.is_empty())
        })
    }

    /// `pkg.name(args)` once neither a top-level function nor a classifier constructor of the
    /// package was selected: the package's scope alone is searched, so a top-level property is
    /// read and its value invoked, and any other `name` is unresolved at the callee. `None` when
    /// the package has a callable this does not own.
    pub(super) fn package_property_call(
        &mut self,
        scope: &CheckerScope<'_>,
        (call, callee, args): (ExprId, ExprId, &[ExprId]),
        package: TypeName,
        name: &str,
        expected: Option<Ty>,
    ) -> Option<Ty> {
        let resolver = self.resolver_in_scope(std::slice::from_ref(&package));
        let properties = resolver.top_level_properties(name);
        if properties.is_empty() {
            if !self.package_lacks_callable(package, name) {
                return None;
            }
        } else if let Some(value) = self.read_package_property(scope, callee, package, name) {
            if value == Ty::Error {
                return Some(Ty::Error);
            }
            if self.value_type_claims_call(scope, value) {
                let arg_tys = self.arg_tys(scope, args);
                return self.record_invoke_or_report(
                    scope,
                    CallArgs {
                        call,
                        args,
                        arg_tys: &arg_tys,
                    },
                    callee,
                    value,
                    self.span(call),
                    CallResultConstraint::direct(expected),
                );
            }
        }
        let message = crate::diagnostic_wording::unresolved_reference_on(name, None);
        self.diags
            .error(self.member_name_span(callee, name), message);
        Some(Ty::Error)
    }

    pub(super) fn names_root_package(&self, expression: ExprId, name: &str) -> bool {
        !self.segment_cannot_name_a_package(Some(expression))
            && subpackage(&self.fed_source(), TypeName::ROOT, name).is_some()
    }

    fn segment_cannot_name_a_package(&self, expression: Option<ExprId>) -> bool {
        expression.is_some_and(|expression| {
            package_qualifier_forms::segment_cannot_name_a_package(self.file, expression)
        })
    }
}

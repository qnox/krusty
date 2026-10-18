//! The resolved qualifiers that kotlinc's FIR keeps for a qualifier chain in expression position
//! (`QualifiedNameResolution.kt` and `FirCallResolver.resolveVariableAccessAndSelectCandidateImpl`),
//! which its qualifier checkers then visit.
//!
//! Every written segment that binds a package or a classifier is one qualifier, built from the
//! qualifier of the segment before it:
//! - A segment used as a receiver (`b` and `c` in `a.b.c.f()`) continues its predecessor
//!   (`continueQualifier`). It records an outer-class type-argument diagnostic when the predecessor
//!   has type arguments. A classifier nested in a classifier also takes the predecessor's type
//!   arguments and keeps it as its explicit parent; a segment continuing a package keeps only its
//!   own type arguments and drops the package, except that a classifier keeps it once
//!   `ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier` is enabled.
//! - A last segment that names a classifier with a value and is not itself a receiver
//!   (`val o = a.b.Obj`) is selected as a variable instead. It keeps the predecessor as its
//!   explicit parent, inherits the predecessor's diagnostics, has only its own type arguments, and
//!   is a nested classifier accessed via an instance when the predecessor has type arguments.
//!
//! Only the last qualifier and its chain of explicit parents stay in the checked tree. The tree is
//! built once, at the expression that consumes the chain: the access whose explicit receiver it
//! is, or the chain itself when it is a value.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum QualifierBinding {
    Package(TypeName),
    Classifier(TypeName),
}

impl QualifierBinding {
    fn resolved(self) -> ResolvedQualifier {
        match self {
            Self::Package(package) => ResolvedQualifier::Package(package),
            Self::Classifier(internal) => ResolvedQualifier::Classifier(internal),
        }
    }
}

/// One FIR resolved qualifier.
#[derive(Clone, Copy, Debug)]
pub(super) struct QualifierNode {
    /// The written segment, whose expression ends at this qualifier.
    pub(super) expression: ExprId,
    pub(super) binding: QualifierBinding,
    /// FIR's `typeArguments` is non-empty: the segment's own list, or an outer classifier's.
    pub(super) has_type_arguments: bool,
    /// How many outer-class type-argument diagnostics (`ConeTypeArgumentsForOuterClass`) the
    /// qualifier carries.
    pub(super) outer_class_type_arguments: usize,
    /// The previous segment's qualifier is this one's explicit parent and stays in the tree.
    pub(super) has_explicit_parent: bool,
}

/// What consumes the last qualifier of a chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum QualifierConsumer {
    /// A call, property access, assignment or callable reference whose explicit receiver it is.
    QualifiedAccess,
    /// A class literal (`a.b.Cls::class`).
    ClassLiteral,
    /// Nothing selects through it: the chain is itself the expression.
    Standalone,
}

pub(super) struct QualifierTree {
    /// Every written segment's qualifier, root first.
    pub(super) nodes: Vec<QualifierNode>,
    pub(super) consumer: QualifierConsumer,
    /// The last segment is a classifier selected as a value, not continued as a qualifier.
    pub(super) value: Option<SelectedQualifierValue>,
}

/// A classifier that the last segment selects as a value.
#[derive(Clone, Copy, Debug)]
pub(super) struct SelectedQualifierValue {
    /// The typealias the segment names, when it names one.
    pub(super) alias: Option<TypeName>,
}

impl QualifierTree {
    /// Indices of the qualifiers that stay in the checked tree, last first.
    pub(super) fn retained(&self) -> Vec<usize> {
        let mut retained = Vec::new();
        let mut index = self.nodes.len().checked_sub(1);
        while let Some(current) = index {
            retained.push(current);
            index = (self.nodes[current].has_explicit_parent && current > 0).then(|| current - 1);
        }
        retained
    }

    pub(super) fn last(&self) -> &QualifierNode {
        self.nodes.last().expect("a qualifier tree has a qualifier")
    }

    /// Whether qualifier `index` is the explicit receiver of the element that contains it: the
    /// qualifier whose explicit parent it is, or a qualified access through the chain.
    pub(super) fn is_explicit_receiver_in_container(&self, index: usize) -> bool {
        match self.nodes.get(index + 1) {
            Some(next) => next.has_explicit_parent,
            None => self.consumer == QualifierConsumer::QualifiedAccess,
        }
    }
}

/// How the parent of an expression uses it. kotlinc resolves a qualifier differently when it is the
/// explicit receiver of a selector, and a call's callee is part of the call rather than an access
/// of its own.
#[derive(Default)]
pub(super) struct SelectorOperands {
    receivers: HashSet<ExprId>,
    callees: HashSet<ExprId>,
}

impl SelectorOperands {
    fn of(file: &File) -> Self {
        let mut operands = Self::default();
        for expression in &file.expr_arena {
            match expression {
                Expr::Member { receiver, .. }
                | Expr::SafeCall { receiver, .. }
                | Expr::ExtensionAccess { receiver, .. }
                | Expr::SafeIndex { receiver, .. }
                | Expr::SafeIndexIncDec { receiver, .. }
                | Expr::CallableRef {
                    receiver: Some(receiver),
                    ..
                }
                | Expr::Index {
                    array: receiver, ..
                } => {
                    operands.receivers.insert(*receiver);
                }
                Expr::Call { callee, .. } => {
                    operands.callees.insert(*callee);
                }
                _ => {}
            }
        }
        for statement in &file.stmt_arena {
            match statement {
                Stmt::AssignMember { receiver, .. }
                | Stmt::AssignSafeIndex { receiver, .. }
                | Stmt::AssignIndex {
                    array: receiver, ..
                } => {
                    operands.receivers.insert(*receiver);
                }
                _ => {}
            }
        }
        operands
    }
}

impl Checker<'_> {
    /// Check the qualifier chain that `expression` consumes, or that `expression` is.
    pub(super) fn check_expression_qualifier_tree(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
    ) {
        let (chain, consumer) = match self.file.expr(expression) {
            Expr::Member { .. } => (expression, None),
            Expr::SafeCall { receiver, .. } => {
                (*receiver, Some(QualifierConsumer::QualifiedAccess))
            }
            Expr::CallableRef {
                receiver: Some(receiver),
                ..
            } => {
                let consumer = if self.file.class_literal_references.contains(&expression.0) {
                    QualifierConsumer::ClassLiteral
                } else {
                    QualifierConsumer::QualifiedAccess
                };
                (*receiver, Some(consumer))
            }
            Expr::Call { callee, .. } => match self.file.expr(*callee) {
                Expr::Member { receiver, .. } => {
                    (*receiver, Some(QualifierConsumer::QualifiedAccess))
                }
                _ => return,
            },
            _ => return,
        };
        if self.discover_anonymous_captures || !self.chain_has_qualifier_syntax(chain) {
            return;
        }
        let (chain, consumer) = match consumer {
            Some(consumer) => (chain, consumer),
            // A member access is either a qualifier itself or an access through its receiver.
            None => {
                let Expr::Member { receiver, .. } = self.file.expr(expression) else {
                    return;
                };
                let receiver = *receiver;
                let operands = self.selector_operands();
                if operands.callees.contains(&expression) {
                    // The call that owns this callee consumes the receiver.
                    return;
                }
                let is_receiver = operands.receivers.contains(&expression);
                if matches!(
                    self.qualifier(scope, QualifierInput::Expression(expression)),
                    Ok(ResolvedQualifier::Package(_) | ResolvedQualifier::Classifier(_))
                ) {
                    if is_receiver {
                        // The access through this qualifier builds its tree.
                        return;
                    }
                    (expression, QualifierConsumer::Standalone)
                } else {
                    (receiver, QualifierConsumer::QualifiedAccess)
                }
            }
        };
        self.check_qualifier_chain(scope, chain, consumer);
    }

    /// The receiver of `receiver.name = value`, when it is a qualifier.
    pub(super) fn check_assignment_receiver_qualifier_tree(
        &mut self,
        scope: &CheckerScope<'_>,
        receiver: ExprId,
    ) {
        if self.discover_anonymous_captures || !self.chain_has_qualifier_syntax(receiver) {
            return;
        }
        self.check_qualifier_chain(scope, receiver, QualifierConsumer::QualifiedAccess);
    }

    fn check_qualifier_chain(
        &mut self,
        scope: &CheckerScope<'_>,
        chain: ExprId,
        consumer: QualifierConsumer,
    ) {
        let Some(tree) = self.qualifier_tree(scope, chain, consumer) else {
            return;
        };
        self.check_outer_class_type_arguments(&tree);
        if consumer == QualifierConsumer::QualifiedAccess {
            self.check_package_receiver_type_arguments(&tree);
        }
        self.check_package_qualifier_forms(&tree);
    }

    /// Only type arguments and parentheses written on a qualifier segment are checked here, so a
    /// chain without either needs no tree.
    fn chain_has_qualifier_syntax(&self, chain: ExprId) -> bool {
        let mut segments = Vec::new();
        if qualifier_expression_segments(self.file, chain, &mut segments).is_err() {
            return false;
        }
        segments.iter().any(|(expression, _)| {
            expression.is_some_and(|expression| {
                self.file.call_type_args.contains_key(&expression.0)
                    || self
                        .file
                        .parenthesized_expression_spans
                        .contains_key(&expression.0)
            })
        })
    }

    fn selector_operands(&mut self) -> &SelectorOperands {
        let file = self.file;
        self.selector_operands
            .get_or_insert_with(|| SelectorOperands::of(file))
    }

    /// The FIR qualifiers of `chain`, or `None` when one of its prefixes is not a package or a
    /// classifier (a value, or an error the walk reports elsewhere).
    fn qualifier_tree(
        &self,
        scope: &CheckerScope<'_>,
        chain: ExprId,
        consumer: QualifierConsumer,
    ) -> Option<QualifierTree> {
        let mut segments = Vec::new();
        qualifier_expression_segments(self.file, chain, &mut segments).ok()?;
        let mut nodes: Vec<QualifierNode> = Vec::with_capacity(segments.len());
        let mut value = None;
        for (index, (expression, name)) in segments.iter().enumerate() {
            let expression = (*expression)?;
            let binding = match self.qualifier(scope, QualifierInput::Expression(expression)) {
                Ok(ResolvedQualifier::Package(package)) => QualifierBinding::Package(package),
                Ok(ResolvedQualifier::Classifier(internal)) => {
                    QualifierBinding::Classifier(internal)
                }
                Ok(ResolvedQualifier::Value) | Err(_) => return None,
            };
            let own_type_arguments = self.file.call_type_args.contains_key(&expression.0);
            let Some(previous) = nodes.last().copied() else {
                nodes.push(QualifierNode {
                    expression,
                    binding,
                    has_type_arguments: own_type_arguments,
                    outer_class_type_arguments: 0,
                    has_explicit_parent: false,
                });
                continue;
            };
            let selected_value = (index + 1 == segments.len()
                && consumer == QualifierConsumer::Standalone)
                .then(|| self.selected_qualifier_value(previous.binding, binding, expression, name))
                .flatten();
            let node = if let Some(selected) = selected_value {
                value = Some(selected);
                QualifierNode {
                    expression,
                    binding,
                    has_type_arguments: own_type_arguments,
                    outer_class_type_arguments: previous.outer_class_type_arguments,
                    has_explicit_parent: true,
                }
            } else {
                let nested = matches!(previous.binding, QualifierBinding::Classifier(_));
                QualifierNode {
                    expression,
                    binding,
                    has_type_arguments: own_type_arguments
                        || (nested && previous.has_type_arguments),
                    outer_class_type_arguments: usize::from(previous.has_type_arguments),
                    has_explicit_parent: nested
                        || (self.file.forbid_package_qualifier_forms
                            && matches!(binding, QualifierBinding::Classifier(_))),
                }
            };
            nodes.push(node);
        }
        (!nodes.is_empty()).then_some(QualifierTree {
            nodes,
            consumer,
            value,
        })
    }

    /// A last segment that names a classifier with a value is selected as that value; a package or
    /// a classifier without one is still continued as a qualifier.
    fn selected_qualifier_value(
        &self,
        previous: QualifierBinding,
        binding: QualifierBinding,
        expression: ExprId,
        name: &str,
    ) -> Option<SelectedQualifierValue> {
        let QualifierBinding::Classifier(internal) = binding else {
            return None;
        };
        if internal != crate::types::wk::unit()
            && self.classifier_singleton_value(internal).is_none()
        {
            return None;
        }
        let source = self.fed_source();
        let (_, declaration) = walk_qualifier_with_declaration_identity(
            &source,
            previous.resolved(),
            &[(Some(expression), name.to_string())],
        )
        .ok()?;
        let alias = match declaration {
            Some(crate::libraries::ClassifierDeclaration::TypeAlias(alias)) => Some(alias.identity),
            Some(crate::libraries::ClassifierDeclaration::Ordinary(_)) | None => None,
        };
        Some(SelectedQualifierValue { alias })
    }
}

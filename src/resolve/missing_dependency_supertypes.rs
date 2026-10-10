//! kotlinc's missing-dependency supertype checks: `FirMissingDependencySupertypeInDeclarationsChecker`
//! and `FirMissingDependencySupertypeInQualifiedAccessExpressionsChecker`.
//!
//! A classpath classifier may name a supertype the classpath does not provide. kotlinc then reports
//! `MISSING_DEPENDENCY_SUPERCLASS` once for each such supertype, at a class or object that inherits
//! it, at a type parameter whose bound inherits it, and where a call or property access selects
//! a member through such a classifier: its dispatch receiver, the member's owner, or an extension's
//! declared receiver. A constructor call, or a member reached through a dispatch receiver already
//! reported, is the eager form: `MISSING_DEPENDENCY_SUPERCLASS_WARNING` unless
//! `AllowEagerSupertypeAccessibilityChecks` makes it the error.
//!
//! A supertype the current module names but cannot resolve is an unresolved reference, never a
//! missing dependency; only an edge leading out of a classpath classifier is one.

use super::*;

/// The supertypes of a classifier, transitively, that a classpath classifier names and no source
/// provides, in kotlinc's depth-first `collectSuperTypes` order. `parents` are the classifier's
/// own direct supertypes; they are missing dependencies only when `binary`, the classifier itself
/// being a classpath one.
pub(super) fn missing_inherited_supertypes(
    source: &dyn SymbolSource,
    parents: impl IntoIterator<Item = TypeName>,
    binary: bool,
) -> Vec<TypeName> {
    let any = crate::types::wk::any();
    let mut seen = HashSet::new();
    let mut missing = Vec::new();
    let mut pending: Vec<(TypeName, bool)> = parents
        .into_iter()
        .map(|parent| (parent, binary))
        .collect::<Vec<_>>();
    pending.reverse();
    while let Some((supertype, from_classpath)) = pending.pop() {
        if supertype == any || !seen.insert(supertype) {
            continue;
        }
        let Some(classifier) = source.classifier(supertype) else {
            if from_classpath {
                missing.push(supertype);
            }
            continue;
        };
        let binary = is_binary(&classifier);
        let first_child = pending.len();
        pending.extend(
            classifier
                .supertypes
                .iter_ids()
                .map(|parent| (parent, binary)),
        );
        pending[first_child..].reverse();
    }
    missing
}

/// Whether a classifier record comes from the classpath rather than from source.
fn is_binary(classifier: &crate::libraries::LibraryType) -> bool {
    classifier.source_file.is_none() && classifier.stable_declaration.is_none()
}

/// How a missing supertype is reported at one site.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Severity {
    Error,
    /// kotlinc's eager check: an error only under `AllowEagerSupertypeAccessibilityChecks`.
    Eager,
}

impl Checker<'_> {
    /// The direct supertypes of `classifier`, and whether it is a classpath classifier: the
    /// published record's, or the header this body resolved for a body-local classifier.
    fn direct_supertype_names(&self, classifier: TypeName) -> (Vec<TypeName>, bool) {
        if let Some(local) = self.resolved_body_local_supertypes.get(&classifier) {
            return (
                local
                    .iter()
                    .filter_map(|ty| ty.kotlin_class_internal())
                    .collect(),
                false,
            );
        }
        self.fed_source()
            .classifier(classifier)
            .map(|record| (record.supertypes.iter_ids().collect(), is_binary(&record)))
            .unwrap_or_default()
    }

    /// Report each missing supertype `classifier` inherits at `span`, naming the classifier as
    /// `label`. Returns whether any was reported.
    fn report_missing_supertypes(
        &mut self,
        span: Span,
        classifier: TypeName,
        label: &str,
        severity: Severity,
    ) -> bool {
        let (parents, binary) = self.direct_supertype_names(classifier);
        let missing = missing_inherited_supertypes(&self.fed_source(), parents, binary);
        let warning =
            severity == Severity::Eager && !self.file.allow_eager_supertype_accessibility_checks;
        for supertype in &missing {
            let supertype = Self::access_owner_display(*supertype);
            if warning {
                if !self.suppresses_diagnostic("MISSING_DEPENDENCY_SUPERCLASS_WARNING") {
                    self.diags.warning(
                        span,
                        format!(
                            "cannot access '{supertype}' which is a supertype of '{label}'. This \
                             may be forbidden soon. Check the module classpath for missing or \
                             conflicting dependencies."
                        ),
                    );
                }
            } else {
                self.diags.error(
                    span,
                    format!(
                        "cannot access '{supertype}' which is a supertype of '{label}'. Check your \
                         module classpath for missing or conflicting dependencies."
                    ),
                );
            }
        }
        !missing.is_empty()
    }

    /// kotlinc's declaration check of a class or object: its inherited missing supertypes, at its
    /// signature (`class Name`, a nameless object declaration whole, an object expression whole).
    pub(super) fn check_class_missing_supertypes(
        &mut self,
        class: &ClassDecl,
        declaration: DeclId,
        owner: Option<TypeName>,
        anonymous: bool,
    ) {
        if self.discover_anonymous_captures {
            return;
        }
        let Some(owner) = owner else {
            return;
        };
        let span = if anonymous {
            class.span
        } else {
            match self.file.declaration_prefixes.at(class.name_span.lo) {
                // A nameless object (`companion object`) is named by its `object` keyword.
                Some(prefix) if prefix.classifier_keyword == Some(class.name_span) => {
                    Span::new(prefix.declaration_start, class.span.hi)
                }
                Some(prefix) => Span::new(
                    prefix
                        .classifier_keyword
                        .map_or(class.span.lo, |keyword| keyword.lo),
                    class.name_span.hi,
                ),
                None => Span::new(class.span.lo, class.name_span.hi),
            }
        };
        let label = self.declared_classifier_label(declaration, owner, anonymous);
        self.report_missing_supertypes(span, owner, &label, Severity::Error);
    }

    /// kotlinc's `ClassId.asSingleFqName` of a declared classifier: a local classifier is
    /// `<local>.Name`, and an object expression is `<anonymous>` in its package.
    fn declared_classifier_label(
        &self,
        declaration: DeclId,
        owner: TypeName,
        anonymous: bool,
    ) -> String {
        let package = self.source_package_name().render().replace('/', ".");
        if anonymous {
            return if package.is_empty() {
                "<anonymous>".to_string()
            } else {
                format!("{package}.<anonymous>")
            };
        }
        if self.file.is_local_declaration(declaration) {
            let mut names = Vec::new();
            let mut current = Some(declaration);
            while let Some(id) = current {
                let Decl::Class(class) = self.file.decl(id) else {
                    break;
                };
                names.push(class_declaration_label(&class.name).to_string());
                if !self.file.is_local_declaration(id) {
                    break;
                }
                current = self.file.hoisted_classifier_owner(id);
            }
            names.reverse();
            return format!("<local>.{}", names.join("."));
        }
        Self::access_owner_display(owner)
    }

    /// kotlinc's declaration check of a type parameter: each upper bound's inherited missing
    /// supertypes, at the parameter as its `<…>` list wrote it.
    pub(super) fn check_bound_missing_supertypes(&mut self, bound: &TypeRef, checked: Ty) {
        if self.discover_anonymous_captures {
            return;
        }
        let Some(parameter) = self
            .file
            .type_parameter_bound_owners
            .get(&bound.span.lo)
            .copied()
        else {
            return;
        };
        let mut upper = checked;
        while let Ty::TyParam(_, bound) = upper.non_null() {
            upper = *bound;
        }
        if let Some(classifier) = upper.non_null().kotlin_class_internal() {
            let label = Self::access_owner_display(classifier);
            self.report_missing_supertypes(parameter, classifier, &label, Severity::Error);
        }
    }

    /// kotlinc's qualified-access check: the selected member's dispatch receiver, owner and
    /// declared extension receiver, each classifier once, at the selected name.
    pub(super) fn check_expression_missing_supertypes(&mut self, expression: ExprId) {
        if self.discover_anonymous_captures {
            return;
        }
        let Some(access) = self.selected_access(expression) else {
            return;
        };
        let (span, unresolved) = (access.span, access.unresolved);
        let reported_from = self.diags.diags.len();
        self.check_access_missing_supertypes(access);
        // kotlinc reports an unresolved call's missing receiver supertypes ahead of the
        // unresolved reference at the same name.
        if unresolved {
            let file = self.diags.current_file();
            if let Some(first) = self.diags.diags[..reported_from]
                .iter()
                .position(|diagnostic| diagnostic.file == file && diagnostic.span == span)
            {
                let reported = self.diags.diags.len() - reported_from;
                self.diags.diags[first..].rotate_right(reported);
            }
        }
    }

    /// The qualified accesses a statement makes without an expression of their own: a member
    /// property write, each destructuring `componentN`, a `for` loop's range operator or iterator
    /// protocol, and an increment or decrement.
    pub(super) fn check_statement_missing_supertypes(&mut self, statement: StmtId) {
        if self.discover_anonymous_captures {
            return;
        }
        let class_of = |ty: Ty| ty.non_null().kotlin_class_internal();
        let mut accesses = Vec::new();
        match self.file.stmt(statement) {
            Stmt::AssignMember { receiver, name, .. } => {
                let span = self.assignment_member_name_span(statement, name);
                match self.stmt_lowers.get(&statement) {
                    Some(StmtLowering::MemberPropertyWrite { owner, .. }) => {
                        accesses.push(SelectedAccess {
                            dispatch_receiver: class_of(self.expr_types[receiver.0 as usize]),
                            owner: Some(*owner),
                            ..SelectedAccess::at(span)
                        })
                    }
                    Some(StmtLowering::ExtensionPropertyWrite { access }) => {
                        accesses.push(SelectedAccess {
                            extension_receiver: access.property.receiver.and_then(class_of),
                            ..SelectedAccess::at(span)
                        })
                    }
                    _ => {}
                }
            }
            Stmt::Destructure { entries, .. } => {
                for (index, entry) in entries.iter().enumerate() {
                    if let Some(access) = self
                        .resolved_destructure_components
                        .get(&(statement, index))
                        .and_then(|call| self.selected_call_access(call, entry.name_span))
                    {
                        accesses.push(access);
                    }
                }
            }
            Stmt::For { range, .. } => {
                if let Some(operator) = SyntheticOperatorCall::from_name(range.kind.operator_name())
                {
                    let span = self.binary_range_span(
                        Span::new(self.span(range.start).lo, self.span(range.end).hi),
                        range.start,
                        range.operator_span,
                    );
                    if let Some(access) = self
                        .resolved_stmt_operator_calls
                        .get(&(statement, operator))
                        .and_then(|call| self.selected_call_access(call, span))
                        .or_else(|| {
                            self.resolved_stmt_builtin_operator_accesses
                                .get(&(statement, operator))
                                .map(|access| Self::selected_builtin_operator_access(*access, span))
                        })
                    {
                        accesses.push(access);
                    }
                }
            }
            Stmt::IncDec { dec, prefix, .. } => {
                if self
                    .resolved_inc_dec
                    .contains_key(&IncDecSite::Statement(statement))
                {
                    let span = self.file.stmt_spans[statement.0 as usize];
                    let span = if *prefix {
                        Span::new(span.lo, span.lo.saturating_add(2))
                    } else {
                        Span::new(span.hi.saturating_sub(2), span.hi)
                    };
                    let operator = if *dec {
                        SyntheticOperatorCall::Dec
                    } else {
                        SyntheticOperatorCall::Inc
                    };
                    if let Some(access) = self
                        .resolved_stmt_operator_calls
                        .get(&(statement, operator))
                        .and_then(|call| self.selected_call_access(call, span))
                        .or_else(|| {
                            self.resolved_stmt_builtin_operator_accesses
                                .get(&(statement, operator))
                                .map(|access| Self::selected_builtin_operator_access(*access, span))
                        })
                    {
                        accesses.push(access);
                    }
                }
            }
            Stmt::ForEach { iterable, .. } => {
                if let Some(protocol) = self.iterator_protocols.get(iterable) {
                    let span = self.span(*iterable);
                    for call in [&protocol.iterator, &protocol.has_next, &protocol.next] {
                        accesses.extend(self.selected_call_access(call, span));
                    }
                }
            }
            _ => {}
        }
        for access in accesses {
            self.check_access_missing_supertypes(access);
        }
    }

    /// Report the missing supertypes of one access's dispatch receiver, owner and extension
    /// receiver, each classifier once.
    fn check_access_missing_supertypes(&mut self, access: SelectedAccess) {
        let mut checked = Vec::new();
        let mut reported_receiver = false;
        if let Some(receiver) = access.dispatch_receiver {
            checked.push(receiver);
            let label = Self::access_owner_display(receiver);
            reported_receiver =
                self.report_missing_supertypes(access.span, receiver, &label, Severity::Error);
        }
        let severity = if access.constructor || reported_receiver {
            Severity::Eager
        } else {
            Severity::Error
        };
        for classifier in [access.owner, access.extension_receiver]
            .into_iter()
            .flatten()
        {
            if checked.contains(&classifier) {
                continue;
            }
            checked.push(classifier);
            let label = Self::access_owner_display(classifier);
            self.report_missing_supertypes(access.span, classifier, &label, severity);
        }
    }

    /// The classifiers a call, property read or callable reference selected its target through.
    fn selected_access(&self, expression: ExprId) -> Option<SelectedAccess> {
        let class_of = |ty: Ty| ty.non_null().kotlin_class_internal();
        match self.file.expr(expression) {
            Expr::Call { callee, .. } => {
                let mut span = self.missing_supertype_callee_span(*callee);
                if self.file.infix_calls.contains(&expression.0) {
                    if let Expr::Member { receiver, .. } = self.file.expr(*callee) {
                        span = self.binary_operator_span(expression, *receiver, span);
                    }
                }
                if let Some(constructor) = self.resolved_constructors.get(&expression) {
                    return Some(SelectedAccess {
                        span,
                        owner: Some(constructor.owner()),
                        constructor: true,
                        ..SelectedAccess::at(span)
                    });
                }
                if let Some(call) = self.resolved_calls.get(&expression) {
                    return self.selected_call_access(call, span);
                }
                // An unresolved call is checked through its explicit receiver's type.
                if self.expr_types[expression.0 as usize] == Ty::Error {
                    let Expr::Member { receiver, .. } = self.file.expr(*callee) else {
                        return None;
                    };
                    return Some(SelectedAccess {
                        dispatch_receiver: class_of(self.expr_types[receiver.0 as usize]),
                        unresolved: true,
                        ..SelectedAccess::at(span)
                    });
                }
                // `value(args)` on a value whose `operator fun invoke` was selected.
                match self.expr_lowers.get(&expression)? {
                    ExprLowering::Invoke {
                        kind: InvokeKind::Operator { target, .. },
                        ..
                    } => self.selected_call_access(target, span),
                    _ => None,
                }
            }
            Expr::Binary {
                op,
                lhs,
                operator_span,
                ..
            } => {
                let operator = match op {
                    BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        SyntheticOperatorCall::CompareTo
                    }
                    _ => SyntheticOperatorCall::from_name(op.arith_operator_name()?)?,
                };
                let span = self.binary_operator_span(expression, *lhs, *operator_span);
                self.operator_access(expression, operator, span)
            }
            Expr::Unary { op, operand } => {
                // A recorded token is the `!in` of `value !in container`, a binary expression.
                let span = match self.file.operator_token_spans.get(&expression.0) {
                    Some(span) => match self.file.expr(*operand) {
                        Expr::Call { args, .. } => {
                            self.binary_operator_span(expression, args[0], *span)
                        }
                        _ => *span,
                    },
                    None => {
                        let span = self.span(expression);
                        Span::new(span.lo, span.lo.saturating_add(1))
                    }
                };
                let operator = SyntheticOperatorCall::from_name(op.operator_name())?;
                self.operator_access(expression, operator, span)
            }
            Expr::IncDec { dec, prefix, .. } => {
                let span = self.span(expression);
                let span = if *prefix {
                    Span::new(span.lo, span.lo.saturating_add(2))
                } else {
                    Span::new(span.hi.saturating_sub(2), span.hi)
                };
                let operator = if *dec {
                    SyntheticOperatorCall::Dec
                } else {
                    SyntheticOperatorCall::Inc
                };
                self.operator_access(expression, operator, span)
            }
            // `lo..hi` and `value in lo..hi`: the range operator the bound selects.
            Expr::RangeTo { lo, kind, .. } => {
                let operator = *self.file.operator_token_spans.get(&expression.0)?;
                let span = self.binary_operator_span(expression, *lo, operator);
                let operator = SyntheticOperatorCall::from_name(kind.operator_name())?;
                self.operator_access(expression, operator, span)
            }
            Expr::InRange {
                start: lo,
                end,
                kind,
                ..
            } => {
                let operator = *self.file.operator_token_spans.get(&expression.0)?;
                let range = Span::new(self.span(*lo).lo, self.span(*end).hi);
                let span = self.binary_range_span(range, *lo, operator);
                let operator = SyntheticOperatorCall::from_name(kind.operator_name())?;
                self.operator_access(expression, operator, span)
            }
            // `value[indices]` selected as a `get` operator.
            Expr::Index { .. } => self
                .selected_call_access(self.resolved_calls.get(&expression)?, self.span(expression)),
            Expr::SafeCall { name, args, .. } => {
                let span = self.member_name_span(expression, name);
                if args.is_some() {
                    self.selected_call_access(self.resolved_calls.get(&expression)?, span)
                } else {
                    self.selected_read_access(self.expr_lowers.get(&expression)?, span)
                }
            }
            Expr::Member { name, receiver, .. } => {
                let span = self.member_name_span(expression, name);
                let mut access =
                    self.selected_read_access(self.expr_lowers.get(&expression)?, span)?;
                if access.dispatch_receiver.is_none() && access.owner.is_some() {
                    access.dispatch_receiver = class_of(self.expr_types[receiver.0 as usize]);
                }
                Some(access)
            }
            Expr::CallableRef { name, .. } => {
                let span = self.span(expression);
                let span = Span::new(span.hi.saturating_sub(name.len() as u32), span.hi);
                self.selected_reference_access(self.expr_lowers.get(&expression)?, span)
            }
            _ => None,
        }
    }

    /// An operator convention at `span`, consuming only the exact callable or builtin access that
    /// checking selected. Missing records are not reconstructed from syntax or expression types.
    fn operator_access(
        &self,
        expression: ExprId,
        operator: SyntheticOperatorCall,
        span: Span,
    ) -> Option<SelectedAccess> {
        if let Some(call) = self.resolved_operator_calls.get(&(expression, operator)) {
            return self.selected_call_access(call, span);
        }
        self.resolved_builtin_operator_accesses
            .get(&(expression, operator))
            .map(|access| Self::selected_builtin_operator_access(*access, span))
    }

    fn selected_builtin_operator_access(
        access: SelectedBuiltinOperatorAccess,
        span: Span,
    ) -> SelectedAccess {
        SelectedAccess {
            dispatch_receiver: access.dispatch_receiver,
            owner: access.owner,
            extension_receiver: access.extension_receiver,
            ..SelectedAccess::at(span)
        }
    }

    /// Where kotlinc marks the operator token `operator` of the binary expression `expression`.
    fn binary_operator_span(&self, expression: ExprId, first: ExprId, operator: Span) -> Span {
        self.binary_range_span(self.span(expression), first, operator)
    }

    /// Where kotlinc marks the operator token `operator` of the binary expression spanning
    /// `node`, whose first operand is `first`: see [`crate::ast::BinaryReferenceTokens`].
    fn binary_range_span(&self, node: Span, first: ExprId, operator: Span) -> Span {
        self.file
            .binary_reference_tokens
            .binary_reference_span(node, self.span(first), operator)
    }

    fn missing_supertype_callee_span(&self, callee: ExprId) -> Span {
        match self.file.expr(callee) {
            Expr::Member { name, .. } => self.member_name_span(callee, name),
            _ => self.span(callee),
        }
    }

    fn selected_call_access(&self, call: &ResolvedCall, span: Span) -> Option<SelectedAccess> {
        let class_of = |ty: Ty| ty.non_null().kotlin_class_internal();
        Some(match call {
            ResolvedCall::Member(selected) => SelectedAccess {
                dispatch_receiver: class_of(selected.receiver),
                owner: selected.member.owner,
                ..SelectedAccess::at(span)
            },
            ResolvedCall::Companion(member) => SelectedAccess {
                owner: member.owner,
                ..SelectedAccess::at(span)
            },
            ResolvedCall::Extension(call) => SelectedAccess {
                extension_receiver: call.callable.source_receiver.and_then(class_of),
                ..SelectedAccess::at(span)
            },
            ResolvedCall::MemberExtension {
                owner,
                extension_receiver,
                ..
            } => SelectedAccess {
                owner: Some(*owner),
                extension_receiver: class_of(*extension_receiver),
                ..SelectedAccess::at(span)
            },
            ResolvedCall::TopLevel(_) | ResolvedCall::LocalFunction(_) => return None,
        })
    }

    fn selected_read_access(&self, lowering: &ExprLowering, span: Span) -> Option<SelectedAccess> {
        let class_of = |ty: Ty| ty.non_null().kotlin_class_internal();
        Some(match lowering {
            ExprLowering::MemberPropertyRead { owner, .. } => SelectedAccess {
                owner: Some(*owner),
                ..SelectedAccess::at(span)
            },
            ExprLowering::ExtensionPropertyGet { access } => SelectedAccess {
                extension_receiver: access.property.receiver.and_then(class_of),
                ..SelectedAccess::at(span)
            },
            _ => return None,
        })
    }

    fn selected_reference_access(
        &self,
        lowering: &ExprLowering,
        span: Span,
    ) -> Option<SelectedAccess> {
        let class_of = |ty: Ty| ty.non_null().kotlin_class_internal();
        match lowering {
            ExprLowering::ConstructorRef { internal, .. } => Some(SelectedAccess {
                owner: Some(*internal),
                constructor: true,
                ..SelectedAccess::at(span)
            }),
            ExprLowering::CallableReference { target, .. }
            | ExprLowering::AdaptedCallableReference { target, .. } => match target {
                CallableReferenceTarget::Member { member, receiver } => Some(SelectedAccess {
                    owner: member.owner.or_else(|| class_of(*receiver)),
                    ..SelectedAccess::at(span)
                }),
                CallableReferenceTarget::Extension { callable, .. } => Some(SelectedAccess {
                    extension_receiver: callable.source_receiver.and_then(class_of),
                    ..SelectedAccess::at(span)
                }),
                CallableReferenceTarget::Property(property) => {
                    Some(match property.extension_facade {
                        None => SelectedAccess {
                            owner: Some(property.getter.owner),
                            ..SelectedAccess::at(span)
                        },
                        Some(_) => SelectedAccess {
                            extension_receiver: property.getter.source_receiver.and_then(class_of),
                            ..SelectedAccess::at(span)
                        },
                    })
                }
                _ => None,
            },
            _ => None,
        }
    }
}

/// The classifiers kotlinc checks for one qualified access.
struct SelectedAccess {
    span: Span,
    dispatch_receiver: Option<TypeName>,
    owner: Option<TypeName>,
    extension_receiver: Option<TypeName>,
    constructor: bool,
    /// The access selected nothing; only its explicit receiver is checked.
    unresolved: bool,
}

impl SelectedAccess {
    fn at(span: Span) -> Self {
        Self {
            span,
            dispatch_receiver: None,
            owner: None,
            extension_receiver: None,
            constructor: false,
            unresolved: false,
        }
    }
}

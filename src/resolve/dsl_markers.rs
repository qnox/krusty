//! `@DslMarker` scope control for implicit receivers.
//!
//! A receiver's markers are the annotations on its classifier, or on any supertype, whose own
//! classifier is annotated with `kotlin.DslMarker`. An implicit receiver is out of scope when a
//! receiver nearer in the tower shares one of its markers; a call or property read that binds it
//! implicitly is rejected, while an explicit `this@label` or receiver expression is not.

use super::symbol_display::source_declaration_display;
use super::*;
use crate::fx_hash::HashSet;

impl Checker<'_> {
    /// The DSL markers carried by `ty`'s classifier and its supertypes, without duplicates.
    pub(super) fn dsl_markers(&self, ty: Ty) -> Vec<TypeName> {
        let Some(root) = ty.non_null().kotlin_class_internal() else {
            return Vec::new();
        };
        let source = self.fed_source();
        let dsl_marker = crate::types::wk::dsl_marker();
        let mut markers = Vec::new();
        let mut visited = HashSet::default();
        let mut pending = vec![root];
        while let Some(classifier) = pending.pop() {
            if !visited.insert(classifier) {
                continue;
            }
            let Some(shape) = source.classifier(classifier) else {
                continue;
            };
            for annotation in &shape.annotations {
                let marks = source
                    .classifier(annotation.annotation)
                    .is_some_and(|class| {
                        class
                            .annotations
                            .iter()
                            .any(|meta| meta.annotation == dsl_marker)
                    });
                if marks && !markers.contains(&annotation.annotation) {
                    markers.push(annotation.annotation);
                }
            }
            pending.extend(shape.supertypes.iter());
        }
        markers
    }

    /// Mark each receiver of a nearest-first tower that a nearer receiver shares a marker with.
    pub(super) fn mark_dsl_shadowed_receivers(&self, receivers: &mut [ImplicitReceiver]) {
        let mut nearer: Vec<TypeName> = Vec::new();
        for receiver in receivers.iter_mut() {
            let markers = self.dsl_markers(receiver.declared_ty);
            receiver.dsl_shadowed = markers.iter().any(|marker| nearer.contains(marker));
            for marker in markers {
                if !nearer.contains(&marker) {
                    nearer.push(marker);
                }
            }
        }
    }

    /// Record an implicit binding of a shadowed receiver. The use is reported with the selected
    /// declaration once the expression's resolution is complete ([`Self::check_expression_dsl_scope`]).
    pub(super) fn record_dsl_scope_use(&mut self, expression: ExprId, receiver: ImplicitReceiver) {
        if receiver.dsl_shadowed {
            self.dsl_scope_violations.insert(expression);
        }
    }

    /// A context argument filled from an implicit receiver is an implicit binding of that receiver,
    /// found by the exact tower coordinate the selection recorded. A lexical singleton is not a
    /// scoped rung and has no coordinate.
    pub(super) fn record_dsl_context_scope_use(
        &mut self,
        call: ExprId,
        receivers: &[ImplicitReceiver],
        selected: &ImplicitReceiverSelection,
    ) {
        if selected.singleton.is_some() {
            return;
        }
        if let Some(receiver) = receivers
            .iter()
            .find(|receiver| receiver.receiver_depth == selected.receiver_depth)
        {
            self.record_dsl_scope_use(call, *receiver);
        }
    }

    /// Report the shadowed implicit receiver `expression` binds, rendering the declaration it
    /// selected as kotlinc's `DSL_SCOPE_VIOLATION` does.
    pub(super) fn check_expression_dsl_scope(&mut self, expression: ExprId) {
        if self.discover_anonymous_captures || !self.dsl_scope_violations.remove(&expression) {
            return;
        }
        let (span, symbol) = match self.file.expr(expression) {
            Expr::Call { callee, .. } => (self.span(*callee), self.dsl_call_symbol(expression)),
            _ => (self.span(expression), self.dsl_read_symbol(expression)),
        };
        let symbol = symbol.unwrap_or_else(|| self.implicit_callee_name(expression));
        self.diags.error(
            span,
            format!(
                "'{symbol}' cannot be called in this context with an implicit receiver. Use an explicit receiver if necessary."
            ),
        );
    }

    fn dsl_call_symbol(&self, call: ExprId) -> Option<String> {
        let name = self.implicit_callee_name(call);
        let resolved = self.resolved_calls.get(&call)?;
        if let Some(declaration) = deprecation_usage::call_declaration(resolved) {
            return source_declaration_display(self.resolved_index?, declaration);
        }
        Some(match resolved {
            ResolvedCall::Member(selected) => {
                Self::library_member_candidate_display(&name, &selected.member)
            }
            ResolvedCall::Companion(member) => {
                Self::library_member_candidate_display(&name, member)
            }
            // A dependency callable selected without its generic signature has no SYMBOL form.
            ResolvedCall::TopLevel(_)
            | ResolvedCall::Extension(_)
            | ResolvedCall::MemberExtension { .. }
            | ResolvedCall::LocalFunction(_) => return None,
        })
    }

    fn dsl_read_symbol(&self, read: ExprId) -> Option<String> {
        let declaration = match self.expr_lowers.get(&read)? {
            ExprLowering::MemberPropertyRead {
                stable_declaration, ..
            }
            | ExprLowering::MemberExtensionPropertyRead {
                stable_declaration, ..
            }
            | ExprLowering::AssociatedPropertyRead {
                stable_declaration, ..
            } => (*stable_declaration)?,
            ExprLowering::TopLevelPropertyGet(access)
            | ExprLowering::ExtensionPropertyGet { access } => access.property.stable_declaration?,
            _ => return None,
        };
        source_declaration_display(self.resolved_index?, declaration)
    }

    /// The spelling of the name an implicit-receiver call or read binds.
    fn implicit_callee_name(&self, expression: ExprId) -> String {
        match self.file.expr(expression) {
            Expr::Call { callee, .. } => self.implicit_callee_name(*callee),
            Expr::Name(name) | Expr::Member { name, .. } => name.clone(),
            _ => String::new(),
        }
    }
}

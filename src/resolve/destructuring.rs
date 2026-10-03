//! Resolution of destructuring declarations.
//!
//! A positional entry calls `componentN`. A name-based entry reads the property the parser
//! recorded. An explicit `_ = property` still performs that read and discards the value. An
//! implicit name-based `_` is rejected and reads nothing.

use super::*;
use crate::ast::DestructureProperty;

impl Checker<'_> {
    pub(super) fn stmt_destructure(
        &mut self,
        scope: &CheckerScope<'_>,
        s: StmtId,
        entries: Vec<crate::ast::DestructureEntry>,
        init: ExprId,
    ) {
        let it = self.expr(scope, init);
        let span = self.file.stmt_spans[s.0 as usize];
        // Destructuring requires the initializer to be a known reference type whose class
        // declares `component1..N` (e.g. a krusty `data class`). Anything else is rejected,
        // never miscompiled.
        let source_props = self.file.destructuring.source_properties.get(&s.0).cloned();
        let entry_types = self.file.destructuring.entry_types.get(&s.0).cloned();
        for (idx, entry) in entries.iter().enumerate() {
            let property = source_props
                .as_ref()
                .and_then(|props| props.get(idx))
                .and_then(Option::as_ref);
            // A positional `_` skips that component: no binding and no `componentN` call. An
            // implicit name-based `_` is forbidden and also reads nothing. Only an explicit
            // `_ = prop` reads `prop`, so the getter runs, and then discards the value.
            if entry.ignored && !matches!(property, Some(DestructureProperty::Renamed(_))) {
                if matches!(property, Some(DestructureProperty::Implicit(_))) {
                    self.diags.error(
                        entry.name_span,
                        "underscore in name-based destructuring without renaming is forbidden.",
                    );
                }
                continue;
            }
            let name = &entry.name;
            let is_var = entry.mutable;
            if !entry.ignored && self.declared_in_current_scope(scope, name) {
                self.diags.error(
                    span,
                    format!("krusty: conflicting local declaration '{name}'"),
                );
            }
            // NAME-BASED entry (`val (newName = sourceProp) = src`): bind to the receiver's
            // `sourceProp` property (a member read), not `componentN`.
            if let Some(property) = property {
                let property_name = property.name();
                let property_receiver = self
                    .flow_intersection_member_receiver(scope, init, property_name)
                    .or_else(|| self.type_parameter_member_receiver(scope, it, property_name))
                    .unwrap_or(it);
                let target = self
                    .select_property_member(property_receiver, property_name)
                    .map(ResolvedCall::Member)
                    .map(Box::new);
                match target {
                    Some(target) => {
                        let component = target.ret();
                        let t = entry_types
                            .as_ref()
                            .and_then(|types| types.get(idx))
                            .and_then(Option::as_ref)
                            .map(|annotation| {
                                let declared = self.type_ref_ty(scope, annotation);
                                self.expect_assignable(
                                    declared,
                                    component,
                                    annotation.span,
                                    "destructuring initializer",
                                );
                                declared
                            })
                            .unwrap_or(component);
                        self.resolved_destructure_components
                            .insert((s, idx), target);
                        if !entry.ignored {
                            self.declare(scope, name, t, is_var);
                        }
                    }
                    None => {
                        self.diags.error(
                            span,
                            format!(
                                "krusty: unresolved property '{property_name}' in destructuring"
                            ),
                        );
                        if !entry.ignored {
                            self.declare(scope, name, Ty::Error, is_var);
                        }
                    }
                }
                continue;
            }
            let comp = format!("component{}", idx + 1);
            let component_receiver = self
                .flow_intersection_member_receiver(scope, init, &comp)
                .or_else(|| self.type_parameter_member_receiver(scope, it, &comp))
                .unwrap_or(it);
            let target = match self.destructure_component_target(
                scope,
                s,
                component_receiver,
                &comp,
                &[],
                span,
            ) {
                Ok(target) => target,
                Err(()) => {
                    self.declare(scope, name, Ty::Error, is_var);
                    continue;
                }
            };
            match target {
                Some(target) => {
                    let component = target.ret();
                    let t = entry_types
                        .as_ref()
                        .and_then(|types| types.get(idx))
                        .and_then(Option::as_ref)
                        .map(|annotation| {
                            let declared = self.type_ref_ty(scope, annotation);
                            self.expect_assignable(
                                declared,
                                component,
                                annotation.span,
                                "destructuring initializer",
                            );
                            declared
                        })
                        .unwrap_or(component);
                    self.resolved_destructure_components
                        .insert((s, idx), target);
                    self.declare(scope, name, t, is_var);
                }
                None => {
                    self.diags.error(
                        span,
                        format!("krusty: cannot destructure this type (no operator '{comp}')"),
                    );
                    self.declare(scope, name, Ty::Error, is_var);
                }
            }
        }
    }

    fn destructure_component_target(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: StmtId,
        recv: Ty,
        name: &str,
        params: &[Ty],
        span: Span,
    ) -> Result<Option<DestructureComponentTarget>, ()> {
        if !params.is_empty() {
            return Ok(None);
        }
        Ok(self
            .zero_arg_operator_call(
                scope,
                Some(IncDecSite::Statement(statement)),
                recv,
                name,
                span,
                Some(span),
            )?
            .map(Box::new))
    }
}

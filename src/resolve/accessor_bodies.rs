//! Property accessor bodies (`get() = …`, `get() { … }`, `set(v) { … }`).
//!
//! An accessor body is checked inside the policies its accessor opens: the property's
//! applications written for that use-site (`@get:OptIn(M::class)`) and the accessor's own
//! (`@OptIn(M::class) get()`), nested in the property's declaration policies.

use crate::ast::{FunBody, PropAccessor, PropDecl};
use crate::types::{PropertyAnnotationSite, Ty};

use super::{Checker, CheckerScope, ScopeKind};

/// What a getter body is checked against.
pub(super) struct GetterResult {
    /// The property type the getter returns.
    pub(super) ty: Ty,
    /// The type `field` reads in the getter, when it declares one.
    pub(super) field: Option<Ty>,
    /// Whether the property's type is written, so the body is checked against it rather than
    /// inferring it.
    pub(super) declared: bool,
    /// The property's stable declaration, which publishes an inferred type.
    pub(super) declaration: Option<crate::fir::DeclarationId>,
}

impl Checker<'_> {
    pub(super) fn check_getter_body(
        &mut self,
        scope: &CheckerScope<'_>,
        property: &PropDecl,
        body: &FunBody,
        result: GetterResult,
    ) {
        let policy_depth =
            self.push_accessor_policies(scope, property, PropertyAnnotationSite::Getter);
        self.with_ret_field(result.ty, result.field, scope, |checker| match body {
            FunBody::Expr(body) => {
                checker.check_expression_getter(
                    scope,
                    *body,
                    result.ty,
                    result.declared,
                    result.declaration,
                );
                if !result.declared {
                    if let Some(declaration) = result.declaration {
                        checker.record_inferred_property_publication(declaration, *body, result.ty);
                    }
                }
            }
            FunBody::Block(body) => checker.check_executable_body(scope, *body),
            FunBody::None => {}
        });
        self.active_lexical_policies.truncate(policy_depth);
    }

    /// A setter body, with its value parameter bound to the property type `value`.
    pub(super) fn check_setter_body(
        &mut self,
        scope: &CheckerScope<'_>,
        property: &PropDecl,
        setter: &PropAccessor,
        body: &FunBody,
        value: Ty,
        field: Option<Ty>,
    ) {
        let policy_depth =
            self.push_accessor_policies(scope, property, PropertyAnnotationSite::Setter);
        self.with_ret_field(Ty::Unit, field, scope, |checker| {
            let setter_scope = scope.child(ScopeKind::Function { receiver: None });
            let parameter = crate::ast::setter_param_or_value(setter.param.as_ref());
            checker.declare(&setter_scope, &parameter, value, true);
            checker.record_lexical_declaration(
                &setter_scope,
                &parameter,
                super::lexical_bindings::LexicalValueRole::Setter(setter.span),
            );
            match body {
                FunBody::Expr(body) | FunBody::Block(body) => {
                    checker.check_executable_body(&setter_scope, *body);
                }
                FunBody::None => {}
            }
        });
        self.active_lexical_policies.truncate(policy_depth);
    }
}

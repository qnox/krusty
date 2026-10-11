//! Checked FIR for a function Fleet's `expects` plugin links to its `@Actual` implementation.
//!
//! The checker selected the actual declaration (`resolve::plugin_expects`); this module replaces
//! the `linkToActual()` body with the call kotlinc's plugin builds: the actual applied to the
//! function's own type parameters, receiving the extension receiver and every parameter in
//! declaration order. kotlinc builds that call without source offsets, so every part of it is
//! positionless and the body marks no line.

use super::*;

impl BodyFirChecker<'_> {
    /// The call of `actual` that replaces the body rooted at `root`.
    pub(super) fn linked_actual_call(
        &mut self,
        root: ExprId,
        actual: DeclarationId,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let span = self.file.expr_span(root);
        let cause = self.expression_origin(root)?;
        let origin = self
            .origins
            .synthetic(cause, SyntheticOriginKind::LinkedActual);
        let expect = DeclarationId::from_raw(self.body.owner().raw());
        let index = self.index;
        let (Some(expect_callable), Some(expect_signature), Some(actual_callable)) = (
            index.callable_for_declaration(expect),
            index.signature(expect),
            index.callable_for_declaration(actual),
        ) else {
            return Err(self.failure(span, BodyCheckFailureKind::MissingStableCallTarget));
        };
        let extension_receiver = expect_callable.shape.extension_receiver.map(|ty| FirReceiver {
            value: self.body.add_expr(FirExpr {
                origin,
                ty,
                kind: FirExprKind::ImplicitReceiver {
                    current: true,
                    depth: 0,
                },
            }),
            conversion: None,
        });
        let parameters = self
            .body
            .parameters()
            .iter()
            .map(|parameter| (parameter.value, parameter.ty))
            .collect::<Vec<_>>();
        if parameters.len() != expect_signature.parameters.len() {
            return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
        }
        let mut arguments = Vec::with_capacity(parameters.len());
        for (parameter, (value, ty)) in (0u32..).zip(parameters) {
            arguments.push(FirCallArgument::Expression {
                parameter,
                value: self.body.add_expr(FirExpr {
                    origin,
                    ty,
                    kind: FirExprKind::ValueRead(value),
                }),
                conversion: None,
            });
        }
        let mut substitutions = Vec::new();
        for ordinal in 0u32.. {
            let Some(parameter) = index.type_parameter(actual, ordinal) else {
                break;
            };
            let (Some(header), Some(value)) = (
                index.type_parameter_header(parameter),
                own_type_parameter(index, expect, ordinal),
            ) else {
                return Err(self.failure(span, BodyCheckFailureKind::MissingStableCallTarget));
            };
            let value = ResolvedTy::new(value).map_err(|error| {
                self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
            })?;
            let reified = header.flags.is_reified();
            substitutions.push(FirTypeSubstitution {
                parameter: parameter.into(),
                reified,
                reified_runtime: self.reified_substitution_runtime(reified, value),
                value,
                additional_bounds: Box::new([]),
            });
        }
        Ok(self.body.add_expr(FirExpr {
            origin,
            ty: expect_signature.result,
            kind: FirExprKind::Call(FirCall {
                target: FirCallTarget::Module(actual_callable.id),
                dispatch_receiver: None,
                extension_receiver,
                parameter_types: expect_signature.parameters.clone(),
                arguments: arguments.into_boxed_slice(),
                substitutions: substitutions.into_boxed_slice(),
            }),
        }))
    }
}

/// The type of `declaration`'s own type parameter `ordinal`, as its body sees it.
fn own_type_parameter(index: &ResolvedModuleIndex, declaration: DeclarationId, ordinal: u32) -> Option<Ty> {
    let parameter = index.type_parameter(declaration, ordinal)?;
    let bound = index
        .type_parameter_header(parameter)?
        .bounds
        .first()
        .map_or_else(
            || Ty::nullable(Ty::obj_name(crate::types::wk::any())),
            |bound| bound.ty.get(),
        );
    Some(Ty::ty_param(index.type_parameter_semantic_name(parameter)?, bound))
}

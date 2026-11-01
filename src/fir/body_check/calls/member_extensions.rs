//! Publication of an already-selected member-extension call: its checked target and the
//! argument slots its extension receiver shifts.

use super::*;

impl BodyFirChecker<'_> {
    /// Publish one already-selected member-extension call without branching on provider origin in
    /// lowering. Module declarations keep their callable identity. Dependency declarations carry
    /// the same semantic parameters plus the extension receiver's exact physical slot.
    pub(in crate::fir::body_check) fn member_extension_call_target(
        &self,
        expression: ExprId,
        selected: &ResolvedCall,
    ) -> Result<MemberExtensionFirTarget, BodyCheckFailure> {
        let ResolvedCall::MemberExtension {
            stable_declaration,
            external_identity,
            external_default_provider,
            owner,
            name,
            dispatch_receiver,
            extension_receiver,
            params,
            physical_params,
            context_args,
            ret,
            inline,
            inline_body_plan,
            reified_type_parameter_ordinals,
            suspend,
            declared_ret,
            overridden_results,
            vararg_index,
            ..
        } = selected
        else {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        };
        crate::trace_compiler!(
            "fir",
            "publish member-extension expression={expression:?} span={:?} stable={stable_declaration:?} external={external_identity:?} owner={owner:?} name={name} dispatch={dispatch_receiver:?} extension={extension_receiver:?} params={params:?}",
            self.file.expr_span(expression),
        );
        let argument_vararg = self.declared_argument_vararg(
            self.file.expr_span(expression),
            *vararg_index,
            context_args.len(),
        )?;
        let mut parameters =
            self.selected_call_parameters(expression, *stable_declaration, params)?;
        // Resolution exposes a vararg's element type in `params` so applicability and inference see
        // each source argument. Checked FIR instead publishes the selected declaration slot, which is
        // the array consumed by packing/default materialization. Preserve every other semantic
        // parameter and restore only that selected array slot.
        if let Some(vararg) = *vararg_index {
            let array = physical_params.get(vararg).copied().ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?;
            let parameter = parameters.get_mut(vararg).ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?;
            // A generic element is the selected one (`vararg values: T` with `T = String` packs a
            // `String[]`); the physical slot carries only its erasure.
            *parameter = match array.type_args().first().map(|argument| argument.kind()) {
                Some(crate::types::TyKind::OutProjection(_)) if array.is_reference_array() => {
                    Ty::obj_args_name(crate::types::wk::array(), &[Ty::out_projection(*parameter)])
                }
                // A reference array stays one when the element is a primitive: `T = Int` packs
                // an `Integer[]`, never an `int[]`.
                Some(_) if array.is_reference_array() => {
                    Ty::obj_args_name(crate::types::wk::array(), &[*parameter])
                }
                _ => array,
            };
        }
        if let Some(declaration) = stable_declaration {
            let callable = self
                .index
                .callable_for_declaration(*declaration)
                .ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::MissingStableCallTarget,
                    )
                })?;
            return Ok(MemberExtensionFirTarget {
                target: FirCallTarget::Module(callable.id),
                substitutions: self.call_substitutions(expression, *declaration)?,
                parameters,
                extension_parameter: None,
                extension_receiver: *extension_receiver,
                argument_vararg,
            });
        }

        let declaration = external_identity.ok_or_else(|| {
            crate::trace_compiler!(
                "fir",
                "member-extension call has no stable target expression={expression:?} span={:?} owner={owner:?} name={name} receiver={extension_receiver:?} params={params:?}",
                self.file.expr_span(expression),
            );
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::MissingStableCallTarget,
            )
        })?;
        let extension_parameter = context_args.len();
        if extension_parameter > parameters.len() {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        }
        let mut target_parameters = parameters.clone();
        target_parameters.insert(extension_parameter, *extension_receiver);
        let (mut target, substitutions) = self.external_call_target_with_identity(
            expression,
            ExternalCallTarget {
                declaration,
                singleton_dispatch: dispatch_receiver
                    .singleton
                    .as_ref()
                    .map(|singleton| singleton.classifier),
                receiver: Some(dispatch_receiver.ty),
                declared_receiver: None,
                parameters: target_parameters,
                result: *ret,
                declared_result: *declared_ret,
                overridden_results,
                semantic_role: None,
                intrinsic_const: false,
                overridden_declarations: &[],
                suspend: *suspend,
                inline: *inline,
                inline_plan: inline_body_plan.as_deref(),
                inline_receiver_parameter: None,
                inline_declared_receiver: None,
                reified_type_parameter_ordinals,
            },
        )?;
        let extension_parameter = u32::try_from(extension_parameter).map_err(|_| {
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            )
        })?;
        let FirCallTarget::External {
            default_provider,
            extension_receiver_parameter,
            ..
        } = &mut target
        else {
            unreachable!("an external member extension must produce an external FIR target")
        };
        *default_provider = *external_default_provider;
        *extension_receiver_parameter = Some(extension_parameter);
        Ok(MemberExtensionFirTarget {
            target,
            substitutions,
            parameters,
            extension_parameter: Some(extension_parameter),
            extension_receiver: *extension_receiver,
            argument_vararg,
        })
    }

    pub(in crate::fir::body_check) fn member_extension_arguments(
        &self,
        expression: ExprId,
        mut arguments: Box<[FirCallArgument]>,
        extension_parameter: Option<u32>,
    ) -> Result<Box<[FirCallArgument]>, BodyCheckFailure> {
        let Some(extension_parameter) = extension_parameter else {
            return Ok(arguments);
        };
        for argument in &mut arguments {
            let parameter = match argument {
                FirCallArgument::Expression { parameter, .. }
                | FirCallArgument::Default { parameter, .. }
                | FirCallArgument::Vararg { parameter, .. } => parameter,
            };
            if *parameter >= extension_parameter {
                *parameter = parameter.checked_add(1).ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnsupportedCallShape,
                    )
                })?;
            }
        }
        Ok(arguments)
    }
}

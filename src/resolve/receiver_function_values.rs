//! Invocation of a value whose function type has an extension receiver, `block()` or
//! `receiver.block()`: finding the value, splitting its `context(C…) R.(V…) -> T` shape, supplying
//! the implicit receiver and context arguments, and recording the selected invoke.

use super::*;

/// A receiver-function signature's parameters split at kotlinc's function-type boundaries.
pub(super) struct ReceiverFunctionParts {
    pub(super) context: &'static [Ty],
    pub(super) receiver: Ty,
    pub(super) values: &'static [Ty],
}

impl Checker<'_> {
    pub(super) fn receiver_function_value(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> Option<(&'static crate::types::FnSig, ReceiverFnValueOrigin)> {
        if let Some((semantic, origin)) = self.local_callable_type(scope, name) {
            return match semantic {
                Ty::Fun(signature) if signature.has_receiver => {
                    let origin = match origin {
                        ReceiverFnValueOrigin::DispatchProperty {
                            owner,
                            receiver_identity,
                            declared_ty,
                            enum_entry_property,
                            owner_storage,
                        } => {
                            let receivers = self.implicit_receivers(scope);
                            let recorded_is_owner = receivers.iter().any(|receiver| {
                                receiver.identity == receiver_identity
                                    && self.receiver_is_assignable(receiver.ty, Ty::obj_name(owner))
                            });
                            let receiver_identity = if recorded_is_owner {
                                receiver_identity
                            } else {
                                receivers
                                    .into_iter()
                                    .find(|receiver| {
                                        receiver.extension_receiver.is_none()
                                            && self.receiver_is_assignable(
                                                receiver.ty,
                                                Ty::obj_name(owner),
                                            )
                                    })
                                    .map(|receiver| receiver.identity)
                                    .unwrap_or(receiver_identity)
                            };
                            ReceiverFnValueOrigin::DispatchProperty {
                                owner,
                                receiver_identity,
                                declared_ty,
                                enum_entry_property,
                                owner_storage,
                            }
                        }
                        origin => origin,
                    };
                    Some((signature, origin))
                }
                _ => None,
            };
        }
        for receiver in self.implicit_receivers(scope) {
            let Some(owner) = receiver.ty.obj_internal() else {
                continue;
            };
            if let Some(property) = self
                .scoped_properties(scope, owner)
                .into_iter()
                .rev()
                .find(|property| property.name == name)
            {
                return match property.ty {
                    Ty::Fun(signature) if signature.has_receiver => Some((
                        signature,
                        ReceiverFnValueOrigin::DispatchProperty {
                            owner: property.owner,
                            receiver_identity: receiver.identity,
                            declared_ty: property.ty,
                            enum_entry_property: property.enum_entry_property,
                            owner_storage: false,
                        },
                    )),
                    _ => None,
                };
            }
        }
        match self.select_top_level_property(scope, name) {
            TopLevelPropertySelection::Selected(property) if matches!(property.property.ty, Ty::Fun(signature) if signature.has_receiver) =>
            {
                let Ty::Fun(signature) = property.property.ty else {
                    unreachable!("guard selected a function-typed property")
                };
                Some((signature, ReceiverFnValueOrigin::TopLevelProperty))
            }
            _ => None,
        }
    }

    /// Split a receiver-function signature at its context, receiver and value boundaries. The
    /// function type `context(C…) R.(V…) -> T` lays its parameters out as `[C…, R, V…]`.
    pub(super) fn receiver_function_parts(
        signature: &'static crate::types::FnSig,
    ) -> Option<ReceiverFunctionParts> {
        if !signature.has_receiver {
            return None;
        }
        let (context, rest) = signature.params.split_at_checked(signature.context_count)?;
        let (&receiver, values) = rest.split_first()?;
        Some(ReceiverFunctionParts {
            context,
            receiver,
            values,
        })
    }

    /// Whether the scope supplies every context argument of a receiver-function value invoked with
    /// its context parameters omitted.
    pub(super) fn receiver_function_context_available(
        &self,
        scope: &CheckerScope<'_>,
        parts: &ReceiverFunctionParts,
    ) -> bool {
        parts.context.is_empty()
            || self
                .select_context_arguments(scope, parts.context)
                .is_some()
    }

    pub(super) fn receiver_function_member_call_params(
        &self,
        scope: &CheckerScope<'_>,
        receiver_ty: Ty,
        name: &str,
        argument_count: usize,
    ) -> Option<Vec<Ty>> {
        let (signature, _) = self.receiver_function_value(scope, name)?;
        let parts = Self::receiver_function_parts(signature)?;
        if !self.receiver_is_assignable(receiver_ty, parts.receiver)
            || !self.receiver_function_context_available(scope, &parts)
        {
            return None;
        }
        let params = parts.values.to_vec();
        (params.len() == argument_count).then_some(params)
    }

    pub(super) fn receiver_function_implicit_receiver(
        &self,
        scope: &CheckerScope<'_>,
        expected: Ty,
        value_parameter_count: usize,
        actual_argument_count: usize,
    ) -> Option<ImplicitReceiver> {
        if value_parameter_count != actual_argument_count {
            return None;
        }
        self.implicit_receivers(scope)
            .into_iter()
            .find(|actual| self.receiver_is_assignable(actual.ty, expected))
    }

    /// Commit invocation of a value whose function type has an extension receiver. The caller chooses
    /// only whether source syntax supplied that receiver explicitly; argument validation and lowering
    /// use one path for locals, properties, callable-reference locals, and safe calls.
    pub(super) fn record_receiver_function_invoke(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        name: &str,
        signature: &'static crate::types::FnSig,
        origin: ReceiverFnValueOrigin,
        explicit_receiver: Option<Ty>,
    ) -> Option<Ty> {
        let CallArgs {
            call,
            args,
            arg_tys,
        } = call_args;
        let parts = Self::receiver_function_parts(signature)?;
        let (expected_receiver, params) = (parts.receiver, parts.values);
        if params.len() != arg_tys.len() {
            return None;
        }
        // Context parameters are always supplied implicitly here: the form that passes them
        // explicitly (`f(context, receiver, value)`) is the ordinary function-value invoke.
        let context_args = if parts.context.is_empty() {
            None
        } else {
            Some(self.select_context_arguments(scope, parts.context)?)
        };
        let implicit_receiver = match explicit_receiver {
            Some(actual) => {
                if !self.receiver_is_assignable(actual, expected_receiver) {
                    return None;
                }
                None
            }
            None => Some(self.receiver_function_implicit_receiver(
                scope,
                expected_receiver,
                params.len(),
                arg_tys.len(),
            )?),
        };
        let property_callee = match origin {
            ReceiverFnValueOrigin::TopLevelProperty => {
                let Expr::Call { callee, .. } = self.file.expr(call) else {
                    return None;
                };
                let TopLevelPropertySelection::Selected(access) =
                    self.select_top_level_property(scope, name)
                else {
                    return None;
                };
                self.set(*callee, access.property.ty);
                self.expr_lowers
                    .insert(*callee, ExprLowering::TopLevelPropertyGet(access));
                Some(*callee)
            }
            ReceiverFnValueOrigin::DispatchProperty {
                receiver_identity, ..
            } => {
                let Expr::Call { callee, .. } = self.file.expr(call) else {
                    return None;
                };
                let receiver = self
                    .implicit_receivers(scope)
                    .into_iter()
                    .find(|receiver| receiver.identity == receiver_identity)?;
                let selection = self
                    .select_property_read(scope, receiver.ty, name)
                    .ok()
                    .flatten()?;
                let property_ty = self.record_property_read(scope, Some(*callee), selection);
                self.set(*callee, property_ty);
                self.mark_implicit_receiver_selection(*callee, receiver);
                Some(*callee)
            }
            ReceiverFnValueOrigin::Local
            | ReceiverFnValueOrigin::ClassStorage(_)
            | ReceiverFnValueOrigin::EnumEntryPropertyStorage { .. } => None,
        };
        self.expect_call_args(scope, params, false, args, arg_tys);
        if let Some(context_args) = context_args {
            self.context_args.insert(call, context_args);
        }
        if let Some(receiver) = implicit_receiver {
            self.mark_implicit_receiver_selection(call, receiver);
        }
        self.expr_lowers.insert(
            call,
            ExprLowering::ReceiverFnInvoke {
                name: name.to_string(),
                params: signature.params.clone(),
                context_count: parts.context.len(),
                ret: signature.ret,
                origin,
                property_callee,
                implicit_receiver: implicit_receiver.map(|receiver| receiver.ty),
                suspend: signature.suspend,
            },
        );
        Some(signature.ret)
    }
}

//! The scope-tower levels an explicit-receiver call `receiver.name(args)` visits after the members
//! of `receiver` and before top-level extensions.
//!
//! kotlinc enumerates every local scope first, innermost first, and then the implicit receivers.
//! A level contributes its extension functions and, through the invoke convention, a value of
//! extension function type named `name` (`f.invoke(receiver, args)`). The value's candidate lives
//! at the level that declared the VALUE; within one level a function precedes an invoke.
//!
//! An object or companion named by its classifier (`TheScope.name()`) is different: every
//! candidate taking the singleton as a receiver sits in kotlinc's last tower group
//! (`QualifierValue`), so any applicable function value — local, property, or top level — outranks
//! even a member of that singleton.

use super::*;

/// Whether the function-value invokes of the scope-tower levels still compete in this call, or a
/// qualified singleton has already given every function value its earlier turn.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FunctionValueRungs {
    InTower,
    AlreadyTried,
}

/// The scope-tower level that declared a receiver-function value.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FunctionValueLevel {
    Lexical,
    ImplicitReceiver,
    TopLevel,
}

impl FunctionValueLevel {
    pub(super) fn of(origin: ReceiverFnValueOrigin) -> Self {
        match origin {
            ReceiverFnValueOrigin::Local
            | ReceiverFnValueOrigin::ClassStorage(_)
            | ReceiverFnValueOrigin::EnumEntryPropertyStorage { .. } => Self::Lexical,
            ReceiverFnValueOrigin::DispatchProperty { .. } => Self::ImplicitReceiver,
            ReceiverFnValueOrigin::TopLevelProperty => Self::TopLevel,
        }
    }
}

impl Checker<'_> {
    /// Commit `receiver.name(args)` on the local and implicit-receiver levels: local extension
    /// functions and lexical function values by lexical depth, then member extensions of the
    /// implicit receivers, then their extension-function-typed properties.
    pub(super) fn record_scope_level_receiver_call(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        receiver_expression: ExprId,
        receiver: Ty,
        name: &str,
        function_values: FunctionValueRungs,
    ) -> Option<Ty> {
        if let Some(ret) = self.record_local_level_call(
            scope,
            call_args,
            receiver_expression,
            receiver,
            name,
            function_values,
        ) {
            return Some(ret);
        }
        if let Some(ret) = self.check_member_extension_function_call(
            scope,
            call_args.call,
            receiver,
            name,
            call_args.args,
            call_args.arg_tys,
        ) {
            return Some(ret);
        }
        if function_values == FunctionValueRungs::AlreadyTried {
            return None;
        }
        self.record_explicit_receiver_function_invoke(
            scope,
            call_args,
            receiver,
            name,
            Some(FunctionValueLevel::ImplicitReceiver),
        )
    }

    /// `receiver.name(args)` as `name.invoke(receiver, args)` for the receiver-function value that
    /// `name` denotes, when that value is declared at `level` (`None`: at any level).
    pub(super) fn record_explicit_receiver_function_invoke(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        receiver: Ty,
        name: &str,
        level: Option<FunctionValueLevel>,
    ) -> Option<Ty> {
        let (signature, origin) = self.receiver_function_value(scope, name)?;
        if level.is_some_and(|level| FunctionValueLevel::of(origin) != level) {
            return None;
        }
        self.record_receiver_function_invoke(
            scope,
            call_args,
            name,
            signature,
            origin,
            Some(receiver),
        )
    }

    /// `Singleton.name(args)` where `name` is a function value accepting the singleton as its
    /// extension receiver. Such a value precedes every candidate on the singleton's value, so its
    /// own value-parameter types shape the arguments.
    pub(super) fn record_qualified_singleton_function_invoke(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        args: &[ExprId],
        receiver: Ty,
        name: &str,
    ) -> Option<Ty> {
        let partial = args
            .iter()
            .map(|&argument| match self.file.expr(argument) {
                Expr::Lambda { .. } | Expr::CallableRef { .. } => None,
                _ => Some(self.expr(scope, argument)),
            })
            .collect::<Vec<_>>();
        let params =
            self.receiver_function_member_call_params(scope, call, receiver, name, args, &partial)?;
        let arg_tys =
            args.iter()
                .zip(params)
                .enumerate()
                .map(
                    |(source, (&argument, parameter))| match self.file.expr(argument) {
                        Expr::Lambda { .. } | Expr::CallableRef { .. } => {
                            self.check_argument_expected(scope, argument, parameter, false, None)
                        }
                        _ => partial[source]
                            .expect("ordinary argument was checked during applicability"),
                    },
                )
                .collect::<Vec<_>>();
        self.record_explicit_receiver_function_invoke(
            scope,
            CallArgs {
                call,
                args,
                arg_tys: &arg_tys,
            },
            receiver,
            name,
            None,
        )
    }

    /// Select and commit the local scope-tower levels, innermost first. Each level offers its
    /// local extension functions, then — at the level declaring the nearest lexical value `name` —
    /// that value's invoke when it has an extension function type. Qualified and safe calls share
    /// this exact path so neither can skip a nearer local declaration in favor of an outer one.
    fn record_local_level_call(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        receiver_expression: ExprId,
        receiver: Ty,
        name: &str,
        function_values: FunctionValueRungs,
    ) -> Option<Ty> {
        let CallArgs {
            call,
            args,
            arg_tys,
        } = call_args;
        let argument_names = self.file.call_arg_names.get(&call.0).cloned();
        let trailing_lambda = self.file.call_has_trailing_lambda.contains(&call.0);
        let mut nearest_value_pending = function_values == FunctionValueRungs::InTower;
        for rung in scope.ancestors() {
            let selection = rung.own_binding(name, Ns::Function).map_or(
                LocalExtensionSelection::None,
                |binding| {
                    self.select_local_extension_candidate_in_rung(
                        scope,
                        receiver,
                        binding.funs(),
                        args,
                        arg_tys,
                        argument_names.as_deref(),
                        trailing_lambda,
                    )
                },
            );
            match selection {
                LocalExtensionSelection::None => {}
                LocalExtensionSelection::Ambiguous => {
                    if !self.call_already_has_argument_diagnostic(call, args) {
                        self.diags.error(
                            self.call_callee_name_span(call),
                            INAPPLICABLE_OVERLOAD_PREFIX.to_string(),
                        );
                    }
                    return Some(Ty::Error);
                }
                LocalExtensionSelection::Selected(selected) => {
                    return Some(self.commit_local_extension_call(
                        scope,
                        call_args,
                        receiver_expression,
                        *selected,
                    ));
                }
            }
            if nearest_value_pending
                && rung
                    .own_binding(name, Ns::Value)
                    .and_then(|binding| binding.value())
                    .is_some()
            {
                nearest_value_pending = false;
                if let Some(ret) = self.record_explicit_receiver_function_invoke(
                    scope,
                    call_args,
                    receiver,
                    name,
                    Some(FunctionValueLevel::Lexical),
                ) {
                    return Some(ret);
                }
            }
        }
        None
    }

    fn commit_local_extension_call(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        receiver_expression: ExprId,
        selected: SelectedLocalExtension,
    ) -> Ty {
        let CallArgs {
            call,
            args,
            arg_tys,
        } = call_args;
        let SelectedLocalExtension {
            statement,
            signature,
            context_args,
        } = selected;
        let context_count = signature.context_count.min(signature.params.len());
        self.expect_call_args(
            scope,
            &signature.params[context_count..],
            signature.vararg(),
            args,
            arg_tys,
        );
        let ret = signature.ret;
        self.mark_local_function_call(
            call,
            statement,
            signature,
            args.len(),
            context_args,
            Some(receiver_expression),
        );
        ret
    }
}

//! Post-resolution plugin expression planning: project every selected call into the plugin contract
//! and record the implementation plans the compilation's native plugins attach to them.
//!
//! Only the native plugins the compilation enabled take part. With none enabled no call is projected
//! at all: kotlinc gives a plugin intrinsic such as `serializer<T>()` its ordinary library body when
//! the plugin was not requested, and so does krusty.

use crate::plugins::registry::NativePlugins;

use super::plugin_expression_annotations::{self, ClassifierAnnotationInputs};
use super::{Checker, ExprLowering, ResolvedCall};

pub(super) fn plan_plugin_expressions(
    checker: &mut Checker<'_>,
    annotation_inputs: ClassifierAnnotationInputs<'_>,
) {
    let file = checker.file;
    let plugins: &NativePlugins = checker.native_plugins;
    if plugins.is_empty() {
        return;
    }
    // No frontend hook reads the module name (only backend output is module-mangled), and the
    // backend builds its own host with the real one.
    let host = plugins.host("main");
    let calls = checker
        .resolved_calls
        .iter()
        .filter_map(|(&expression, call)| {
            let (owner, name, params, ret, generic_sig, inline, implementation) = match call {
                ResolvedCall::TopLevel(target) => (
                    target.callable.owner,
                    target.callable.name.clone(),
                    target.callable.params.clone(),
                    target.callable.ret,
                    target.callable.generic_sig.as_deref().cloned(),
                    target.callable.inline,
                    target.callable.plugin_expression,
                ),
                ResolvedCall::Member(target) => (
                    target
                        .member
                        .owner
                        .or_else(|| target.receiver.kotlin_class_internal())?,
                    target.member.name.clone(),
                    target.member.params.clone(),
                    target.ret,
                    target.member.generic_sig.clone(),
                    target.member.inline,
                    target.member.plugin_expression,
                ),
                ResolvedCall::Extension(target) => (
                    target.callable.owner,
                    target.callable.name.clone(),
                    target.params.clone(),
                    target.callable.ret,
                    target.callable.generic_sig.as_deref().cloned(),
                    target.callable.inline,
                    target.callable.plugin_expression,
                ),
                ResolvedCall::Companion(target) => (
                    target.owner?,
                    target.name.clone(),
                    target.params.clone(),
                    target.ret,
                    target.generic_sig.clone(),
                    target.inline,
                    target.plugin_expression,
                ),
                _ => return None,
            };
            let explicit_receiver =
                crate::ast::explicit_call_receiver(file, expression).map(|receiver| {
                    (
                        receiver,
                        checker.expr_types[receiver.0 as usize].platform_lower_bound(),
                    )
                });
            let implicit_receiver = checker
                .implicit_receiver_selections
                .get(&expression)
                .map(|selected| selected.ty);
            Some(crate::plugins::FrontendSelectedCall {
                expression,
                explicit_receiver,
                implicit_receiver,
                owner,
                name,
                params,
                ret,
                generic_sig,
                inline,
                implementation,
                type_arguments: checker
                    .resolved_call_type_args
                    .get(&expression)
                    .cloned()
                    .unwrap_or_default(),
                argument_slots: checker
                    .resolved_call_arg_slots
                    .get(&expression)
                    .map(|commitment| commitment.slots.clone())
                    .unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    let classifier_annotations =
        plugin_expression_annotations::classifier_annotations_for_calls(annotation_inputs, &calls);
    let plans = {
        let context = crate::plugins::FrontendExpressionContext {
            calls,
            classifier_annotations,
            call_resolver: Some(checker),
        };
        host.plan_frontend_expressions(&context)
    };
    for (expression, plan) in plans {
        let previous = checker
            .expr_lowers
            .insert(expression, ExprLowering::PluginExpression(Box::new(plan)));
        debug_assert!(previous.is_none(), "plugin expression plan collision");
    }
}

impl crate::plugins::FrontendCallResolver for Checker<'_> {
    fn resolve_singleton_member_call(
        &self,
        classifier: crate::types::TypeName,
        name: &str,
        arguments: &[crate::types::Ty],
        expected_result: crate::types::Ty,
    ) -> Option<crate::plugins::FrontendResolvedSingletonCall> {
        use crate::libraries::{Callables, FnKind, FunctionSet, PropertySet};
        use crate::symbol_resolver::{CallArgKind, CandidateSelection};

        let resolver = self.resolver();
        let receiver = resolver.classifier_value_receiver(classifier)?;
        let inventory = resolver.receiver_callables(receiver, name);
        let members = inventory
            .functions()
            .iter()
            .filter(|candidate| candidate.kind == FnKind::Member && candidate.context_count == 0)
            .cloned()
            .collect();
        let candidates =
            Callables::from_parts(FunctionSet { overloads: members }, PropertySet::default());
        let arguments = arguments
            .iter()
            .copied()
            .map(CallArgKind::Typed)
            .collect::<Vec<_>>();
        let CandidateSelection::Selected((selected, parameters, result)) = resolver
            .select_receiver_function_with_params_tracking(
                receiver,
                name,
                &arguments,
                &[],
                &candidates,
                Some(expected_result),
            )
        else {
            return None;
        };
        if !self.receiver_member_accessible(selected.visibility, selected.callable.owner, receiver)
        {
            return None;
        }
        let mut call = resolver.commit_selected_member_function_result(receiver, selected, result);
        call.member.params = parameters;

        let source_arguments = (0..arguments.len())
            .map(|argument| u32::try_from(argument).ok())
            .collect::<Option<Vec<_>>>()?;
        let slots = crate::libraries::map_call_args(
            &source_arguments,
            None,
            &call.member.call_sig.param_names,
            call.member.params.len(),
            call.member.call_sig.required,
            &call.member.call_sig.param_defaults,
            call.member.call_sig.vararg_index,
            false,
        )
        .ok()?;
        let mut argument_parameters = vec![None; arguments.len()];
        for (parameter, source) in slots.into_iter().enumerate() {
            let Some(source) = source else { continue };
            let source = usize::try_from(source).ok()?;
            let parameter = u32::try_from(parameter).ok()?;
            *argument_parameters.get_mut(source)? = Some(parameter);
        }
        let argument_parameters = argument_parameters
            .into_iter()
            .collect::<Option<Vec<_>>>()?;

        let mut type_arguments = Vec::new();
        let mut type_argument_bounds = Vec::new();
        if let Some(signature) = call.member.generic_sig.as_ref() {
            let mut bindings = crate::symbol_resolver::GSigBinds::new();
            for (&declared, &actual) in signature.params.iter().zip(&call.member.params) {
                crate::symbol_resolver::unify_ty(declared, actual, &mut bindings);
            }
            crate::symbol_resolver::unify_ty(signature.ret, result, &mut bindings);
            type_arguments = signature
                .formals
                .iter()
                .map(|formal| bindings.get(formal).copied())
                .collect();
            type_argument_bounds.resize(type_arguments.len(), Vec::new());
        }
        let receiver = receiver.kotlin_class_internal()?;
        Some(crate::plugins::FrontendResolvedSingletonCall {
            receiver,
            selected: crate::plugins::FrontendSelectedMemberCall {
                receiver: call.receiver,
                member: call.member,
                ret: call.ret,
                suspend: call.suspend,
            },
            type_arguments,
            type_argument_bounds,
            argument_parameters,
        })
    }
}

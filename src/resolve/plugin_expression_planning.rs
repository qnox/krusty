//! Post-resolution plugin expression planning: project every selected call into the plugin contract
//! and record the implementation plans the compilation's native plugins attach to them.
//!
//! Only the native plugins the compilation enabled take part. With none enabled no call is projected
//! at all: kotlinc gives a plugin intrinsic such as `serializer<T>()` its ordinary library body when
//! the plugin was not requested, and so does krusty.

use crate::ast::File;
use crate::plugins::registry::NativePlugins;

use super::plugin_expression_annotations::{self, ClassifierAnnotationInputs};
use super::{ExprLowering, ResolvedCall, TypeInfo};

pub(super) fn plan_plugin_expressions(
    file: &File,
    info: &mut TypeInfo,
    plugins: &NativePlugins,
    annotation_inputs: ClassifierAnnotationInputs<'_>,
) {
    if plugins.is_empty() {
        return;
    }
    // No frontend hook reads the module name (only backend output is module-mangled), and the
    // backend builds its own host with the real one.
    let host = plugins.host("main");
    let calls = info
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
            let explicit_receiver = crate::ast::explicit_call_receiver(file, expression)
                .map(|receiver| (receiver, info.ty(receiver)));
            let implicit_receiver = info
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
                type_arguments: info
                    .resolved_call_type_args
                    .get(&expression)
                    .cloned()
                    .unwrap_or_default(),
                argument_slots: info
                    .resolved_call_arg_slots
                    .get(&expression)
                    .cloned()
                    .unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    let classifier_annotations =
        plugin_expression_annotations::classifier_annotations_for_calls(annotation_inputs, &calls);
    let context = crate::plugins::FrontendExpressionContext {
        calls,
        classifier_annotations,
    };
    for (expression, plan) in host.plan_frontend_expressions(&context) {
        let previous = info
            .expr_lowers
            .insert(expression, ExprLowering::PluginExpression(Box::new(plan)));
        debug_assert!(previous.is_none(), "plugin expression plan collision");
    }
}

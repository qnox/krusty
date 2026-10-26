//! Which route a call to a same-module inline function takes: inlined by the backend from the
//! callee's compiled body, or expanded here from the callee's IR template.
//!
//! The route is decided once, from checked facts, before the call is lowered. A backend that asks
//! for compiled bodies ([`SameModuleInlining::CompiledBodies`]) receives the calls whose shape it
//! inlines that way; every other call is expanded in IR, for a named reason.

use super::super::BodyLowering;
use crate::fir::{
    CallableId, DeclarationFlags, DeclarationId, FirBody, FirCapture, FirCaptureSource,
    LocalValueId,
};
use crate::fx_hash::HashSet;
use crate::ir::{ExprId, FunId, IrExpr, SameModuleInlining};
use crate::types::{InlineParameterModifier, Ty, TyKind};

#[cfg(test)]
mod tests;

/// How one call to a same-module inline function is realized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) enum InlineCallRoute {
    /// The call stays a call; the backend inlines the callee's compiled body there.
    CompiledBody,
    /// The callee's IR template is expanded at the call.
    Expansion(ExpansionReason),
}

/// Why a call is expanded from the callee's IR template.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) enum ExpansionReason {
    /// The backend expands every same-module inline call in IR.
    BackendExpandsInIr,
    /// The call is in a default-argument expression.
    DefaultArgument,
    /// The call is in an inline function and a literal lambda argument reads one of that
    /// function's inline parameters. A caller that expands the function's template splices its
    /// own literal for that parameter, which reaches the literal only through an expanded body.
    InlineParameterInLiteral,
    /// The call is in a suspend function or a suspend lambda, or in a lambda written in one,
    /// whose state machine is built around expanded bodies.
    SuspendCaller,
    /// The callee is a suspend function.
    SuspendCallee,
    /// A literal lambda argument leaves its body by a non-local `return`, `break` or `continue`,
    /// which the backend's inliner places only on its byte splice.
    NonLocalJumpInLambda,
    /// The call omits an argument that has a default.
    DefaultArguments,
    /// A parameter (or the receiver) of the callee has a suspend function type, whose lambdas
    /// suspend inside the caller's state machine.
    SuspendFunctionParameter,
    /// A parameter of the callee is `crossinline`: its lambda is placed in an object or lambda the
    /// callee's body creates.
    CrossinlineParameter,
    /// The callee has no body of its own in this file's IR.
    NoTemplate,
    /// The callee's body creates an object of a local or anonymous class.
    LocalClassInBody,
    /// The callee's body holds a subject `when` that switches on an enum's keys. The keys are
    /// read from a table of the callee's own class (the JVM's `$WhenMappings`), numbered as that
    /// class's emission claims them, which a body compiled for the call site cannot reproduce.
    EnumSwitchInBody,
}

impl BodyLowering<'_> {
    /// The route of a call to the same-module inline function `target`, whose IR function is
    /// `function`. `inline_lambdas` holds the call's literal lambda arguments and `has_defaults`
    /// says whether the call omits a defaulted argument. A literal lambda is no reason to expand:
    /// the backend inlines it into the compiled body's invokes.
    pub(in super::super) fn same_module_inline_route(
        &mut self,
        target: CallableId,
        function: Option<FunId>,
        inline_lambdas: &[Option<ExprId>],
        has_defaults: bool,
    ) -> InlineCallRoute {
        match self.compiled_body_exclusion(target, function, inline_lambdas, has_defaults) {
            Some(reason) => {
                crate::trace_compiler!("lower", "inline route: expand {target:?} ({reason:?})");
                InlineCallRoute::Expansion(reason)
            }
            None => InlineCallRoute::CompiledBody,
        }
    }

    fn compiled_body_exclusion(
        &mut self,
        target: CallableId,
        function: Option<FunId>,
        inline_lambdas: &[Option<ExprId>],
        has_defaults: bool,
    ) -> Option<ExpansionReason> {
        if self.ir.same_module_inlining() != SameModuleInlining::CompiledBodies {
            return Some(ExpansionReason::BackendExpandsInIr);
        }
        if let Some(reason) = self.caller_exclusion() {
            return Some(reason);
        }
        if has_defaults {
            return Some(ExpansionReason::DefaultArguments);
        }
        if inline_lambdas
            .iter()
            .flatten()
            .any(|&lambda| self.lambda_leaves_non_locally(lambda))
        {
            return Some(ExpansionReason::NonLocalJumpInLambda);
        }
        if inline_lambdas.iter().flatten().any(|&lambda| {
            matches!(
                self.ir.expr(lambda),
                IrExpr::Lambda { impl_fn, .. }
                    if self.callables_reading_inline_parameters.contains(impl_fn)
            )
        }) {
            return Some(ExpansionReason::InlineParameterInLiteral);
        }
        let Some(callable) = self.index.callable(target) else {
            return Some(ExpansionReason::NoTemplate);
        };
        let declaration = callable.declaration;
        let Some(header) = self.index.declaration_header(declaration) else {
            return Some(ExpansionReason::NoTemplate);
        };
        if header.flags.has(DeclarationFlags::SUSPEND) {
            return Some(ExpansionReason::SuspendCallee);
        }
        let suspend_function =
            |ty: Ty| matches!(ty.non_null().kind(), TyKind::Fun(signature) if signature.suspend);
        let Some(signature) = self.index.signature(declaration) else {
            return Some(ExpansionReason::NoTemplate);
        };
        if signature
            .parameters
            .iter()
            .any(|parameter| suspend_function(parameter.get()))
            || callable
                .shape
                .extension_receiver
                .is_some_and(|receiver| suspend_function(receiver.get()))
        {
            return Some(ExpansionReason::SuspendFunctionParameter);
        }
        let Some(function) = function else {
            return Some(ExpansionReason::NoTemplate);
        };
        if self
            .ir
            .declared_inline_modifiers(function)
            .contains(&InlineParameterModifier::Crossinline)
        {
            return Some(ExpansionReason::CrossinlineParameter);
        }
        let Some(body) = self
            .ir
            .functions
            .get(function as usize)
            .and_then(|function| function.body)
        else {
            return Some(ExpansionReason::NoTemplate);
        };
        self.template_exclusion(body)
    }

    /// What about the body being lowered keeps its same-module inline calls expanded in IR.
    fn caller_exclusion(&self) -> Option<ExpansionReason> {
        if self.in_default_argument {
            return Some(ExpansionReason::DefaultArgument);
        }
        let caller = DeclarationId::from_raw(self.body.owner().raw());
        let suspend = self.suspend_context
            || self
                .index
                .declaration_header(caller)
                .is_some_and(|header| header.flags.has(DeclarationFlags::SUSPEND));
        suspend.then_some(ExpansionReason::SuspendCaller)
    }

    /// What in the callee's IR template keeps a call to it expanded in IR.
    fn template_exclusion(&self, body: ExprId) -> Option<ExpansionReason> {
        let mut pending = vec![body];
        let mut seen = std::collections::HashSet::new();
        while let Some(expression) = pending.pop() {
            if !seen.insert(expression) {
                continue;
            }
            if self.ir.enum_switch_when(expression).is_some() {
                return Some(ExpansionReason::EnumSwitchInBody);
            }
            match self.ir.expr(expression) {
                IrExpr::New { internal, .. }
                    if self
                        .ir
                        .class_id_by_name(*internal)
                        .and_then(|class| self.ir.classes.get(class as usize))
                        // An inner class of a local class needs an instance of one, which only
                        // the body itself can create.
                        .is_some_and(|class| class.is_local_class || class.is_anonymous_object) =>
                {
                    return Some(ExpansionReason::LocalClassInBody);
                }
                _ => {}
            }
            crate::ir::for_each_child(&self.ir.exprs, expression, &mut |child| pending.push(child));
        }
        None
    }

    /// Record the call-site value of each type parameter of `function`, which `call` inlines from
    /// its compiled body, by the parameter's source name the body's markers and signatures carry:
    /// every one specializes the generic signatures of the objects the call regenerates, and the
    /// reified ones the body's operations too.
    pub(in super::super) fn record_compiled_type_arguments(
        &mut self,
        call: ExprId,
        function: FunId,
        substitutions: &[crate::fir::FirTypeSubstitution],
    ) {
        let Some(signature) = self.ir.signatures.get(&function) else {
            return;
        };
        let arguments = substitutions
            .iter()
            .filter_map(|substitution| {
                let crate::fir::FirTypeParameterRef::Module(parameter) = substitution.parameter
                else {
                    return None;
                };
                let semantic = self.index.type_parameter_semantic_name(parameter)?;
                let formal = signature
                    .type_params
                    .iter()
                    .find(|formal| formal.semantic_name == semantic)?;
                Some((formal, substitution))
            })
            .collect::<Vec<_>>();
        let reified = arguments
            .iter()
            .filter(|(formal, substitution)| formal.reified && substitution.reified)
            .map(|(formal, substitution)| (formal.name.clone(), substitution.reified_runtime.get()))
            .collect::<Vec<_>>();
        let type_arguments = arguments
            .iter()
            .map(|(formal, substitution)| (formal.name.clone(), substitution.value.get()))
            .collect::<Vec<_>>();
        if !reified.is_empty() {
            self.ir.reified_call_subst.insert(call, reified);
        }
        if !type_arguments.is_empty() {
            self.ir
                .inline_call_type_arguments
                .insert(call, type_arguments);
        }
    }

    /// Whether the literal `lambda` leaves its inline body non-locally, the fact common lowering
    /// records once for that body (`IrFile::record_lambda_exit`).
    fn lambda_leaves_non_locally(&mut self, lambda: ExprId) -> bool {
        let IrExpr::Lambda {
            inline_body: Some(body),
            ..
        } = *self.ir.expr(lambda)
        else {
            return false;
        };
        self.ir.record_lambda_exit(body)
    }
}

/// The values of a body that hold an inline parameter of the inline function it is written in:
/// a splice-mode parameter of that function's own body, and the captures through which a lambda
/// written in it reads one, keyed as the lambda's checked captures name them.
#[derive(Clone, Debug, Default)]
pub(in super::super) struct InlineParameterValues {
    own: HashSet<LocalValueId>,
    captured: HashSet<(u32, LocalValueId)>,
}

impl InlineParameterValues {
    /// The splice-mode value parameters of an inline function's `body`, as checking bound them.
    pub(in super::super) fn of_inline_function(body: &FirBody) -> Self {
        Self {
            own: body.spliced_inline_parameters().iter().copied().collect(),
            captured: HashSet::default(),
        }
    }

    /// Whether `capture`, of a callable written in this body, reads one of these values.
    pub(in super::super) fn holds(&self, capture: &FirCapture) -> bool {
        let FirCaptureSource::Value(value) = capture.source else {
            return false;
        };
        match capture.enclosing_depth.checked_sub(1) {
            None => self.own.contains(&value),
            Some(depth) => self.captured.contains(&(depth, value)),
        }
    }

    /// The values of `body`, a callable written in this body, that hold an inline parameter.
    pub(in super::super) fn nested(&self, body: &FirBody) -> Self {
        let captured = body
            .captures()
            .iter()
            .filter(|capture| self.holds(capture))
            .filter_map(|capture| {
                let FirCaptureSource::Value(value) = capture.source else {
                    return None;
                };
                Some((capture.enclosing_depth, value))
            })
            .collect();
        Self {
            own: HashSet::default(),
            captured,
        }
    }

    pub(in super::super) fn is_empty(&self) -> bool {
        self.own.is_empty() && self.captured.is_empty()
    }
}

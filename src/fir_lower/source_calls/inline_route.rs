//! Which route a call to a same-module inline function takes: inlined by the backend from the
//! callee's compiled body, or expanded here from the callee's IR template.
//!
//! The route is decided once, from checked facts, before the call is lowered. A backend that asks
//! for compiled bodies ([`SameModuleInlining::CompiledBodies`]) receives the calls whose shape it
//! inlines that way; every other call is expanded in IR, for a named reason.

use super::super::BodyLowering;
use crate::fir::{CallableId, DeclarationFlags, DeclarationId};
use crate::ir::{ExprId, FunId, IrEnclosure, IrExpr, SameModuleInlining};
use crate::types::{InlineParameterModifier, Ty, Visibility};

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
    /// The call is in an inline function's body (or in a callable nested in one), which is itself
    /// a template some caller may expand.
    InlineCaller,
    /// The call is in a lambda or a local function.
    LocalCallable,
    /// The call is in a default-argument expression.
    DefaultArgument,
    /// The call is not in a named function's body (an initializer, a constructor, an accessor).
    NotInFunctionBody,
    /// The call is in a suspend function, whose state machine is built around expanded bodies.
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
    /// The callee declares a reified type parameter.
    ReifiedTypeParameter,
    /// The callee is a member of a class, object or interface, or of a class's companion block.
    ClassMember,
    /// The callee is not public, so its body may reach declarations only its own class can.
    NonPublicCallee,
    /// The callee has no body of its own in this file's IR.
    NoTemplate,
    /// The callee's body creates a lambda or a callable reference.
    LambdaInBody,
    /// The callee's body creates an object of a local or anonymous class.
    LocalClassInBody,
    /// The callee's body expands another same-module inline call.
    NestedInlineExpansion,
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
        &self,
        target: CallableId,
        function: Option<FunId>,
        inline_lambdas: &[Option<ExprId>],
        has_defaults: bool,
    ) -> InlineCallRoute {
        match self.compiled_body_exclusion(target, function, inline_lambdas, has_defaults) {
            Some(reason) => InlineCallRoute::Expansion(reason),
            None => InlineCallRoute::CompiledBody,
        }
    }

    fn compiled_body_exclusion(
        &self,
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
            |ty: Ty| matches!(ty.non_null(), Ty::Fun(signature) if signature.suspend);
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
        // A companion-block member is a static of its class, compiled there.
        if self.index.enclosing_classifier(declaration).is_some()
            || header.flags.has(DeclarationFlags::COMPANION)
        {
            return Some(ExpansionReason::ClassMember);
        }
        if header.visibility != Visibility::Public {
            return Some(ExpansionReason::NonPublicCallee);
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
        if self.ir.signatures.get(&function).is_some_and(|signature| {
            signature
                .type_params
                .iter()
                .any(|parameter| parameter.reified)
        }) {
            return Some(ExpansionReason::ReifiedTypeParameter);
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
        if self.inline_declaration.is_some() {
            return Some(ExpansionReason::InlineCaller);
        }
        if self.declaration_lambda.is_some() || !self.control_path.is_empty() {
            return Some(ExpansionReason::LocalCallable);
        }
        if self.in_default_argument {
            return Some(ExpansionReason::DefaultArgument);
        }
        if !matches!(self.expansion_enclosure, Some(IrEnclosure::Function(_))) {
            return Some(ExpansionReason::NotInFunctionBody);
        }
        let caller = DeclarationId::from_raw(self.body.owner().raw());
        let suspend = self
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
            if self.ir.inline_regions.contains(&expression) {
                return Some(ExpansionReason::NestedInlineExpansion);
            }
            if self.ir.enum_switch_when(expression).is_some() {
                return Some(ExpansionReason::EnumSwitchInBody);
            }
            match self.ir.expr(expression) {
                IrExpr::Lambda { .. } | IrExpr::CallableReference(_) => {
                    return Some(ExpansionReason::LambdaInBody);
                }
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

    /// Whether the literal lambda `lambda` leaves its inline body by a `return` (every `return` an
    /// inline body keeps is non-local), or by a `break` or `continue` of a loop outside it.
    fn lambda_leaves_non_locally(&self, lambda: ExprId) -> bool {
        let IrExpr::Lambda {
            inline_body: Some(body),
            ..
        } = self.ir.expr(lambda)
        else {
            return false;
        };
        leaves_non_locally(self.ir, *body, &mut Vec::new())
    }
}

/// Whether `expression` holds a `return`, or a `break` or `continue` of no loop in `loops` or
/// inside `expression`. A nested lambda's inline body is spliced in place, so its jumps count.
fn leaves_non_locally(ir: &crate::ir::IrFile, expression: ExprId, loops: &mut Vec<String>) -> bool {
    let label = match ir.expr(expression) {
        IrExpr::Return(_) => return true,
        IrExpr::Break { label } | IrExpr::Continue { label } => {
            return match label {
                None => loops.is_empty(),
                Some(label) => !loops.contains(label),
            };
        }
        IrExpr::Lambda {
            inline_body: Some(body),
            ..
        } => return leaves_non_locally(ir, *body, loops),
        IrExpr::While { label, .. } => Some(label.clone().unwrap_or_default()),
        IrExpr::Checked(crate::ir::IrCheckedOperation::RangeLoop { label, .. }) => {
            Some(label.clone())
        }
        _ => None,
    };
    let entered = label.is_some();
    loops.extend(label);
    let mut leaves = false;
    crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
        leaves = leaves || leaves_non_locally(ir, child, loops);
    });
    if entered {
        loops.pop();
    }
    leaves
}

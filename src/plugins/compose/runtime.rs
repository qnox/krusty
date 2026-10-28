//! The Compose runtime API the generated code calls: the declarations kotlinc's plugin binds
//! (`ComposeCallableIds` and the `Composer` members it names), selected at the handoff by their
//! Kotlin signatures and called by their selected identities.

use crate::ir::{
    AnnoRetention, AnnoValue, AppliedAnnotation, Callee, ExprId, IrConst, IrExpr, IrFile,
    RetainedAnnotation,
};
use crate::plugins::runtime_declarations::{
    BoundRuntimeFunction, PluginRuntime, RuntimeDeclaration, RuntimeFunction,
};
use crate::types::{type_name, Ty};

use super::source_facts::RestartableFunction;

const RUNTIME: &str = "androidx/compose/runtime";
const COMPOSER: &str = "androidx/compose/runtime/Composer";
const SCOPE_UPDATE_SCOPE: &str = "androidx/compose/runtime/ScopeUpdateScope";
const FUNCTION_KEY_META: &str = "androidx/compose/runtime/internal/FunctionKeyMeta";

pub(super) fn composer() -> Ty {
    Ty::obj(COMPOSER)
}

/// The runtime functions a restartable function calls.
pub(super) struct ComposeRuntime {
    pub(super) start_restart_group: BoundRuntimeFunction,
    pub(super) end_restart_group: BoundRuntimeFunction,
    pub(super) should_execute: BoundRuntimeFunction,
    pub(super) skip_to_group_end: BoundRuntimeFunction,
    pub(super) update_scope: BoundRuntimeFunction,
    pub(super) source_information: BoundRuntimeFunction,
    pub(super) is_trace_in_progress: BoundRuntimeFunction,
    pub(super) trace_event_start: BoundRuntimeFunction,
    pub(super) trace_event_end: BoundRuntimeFunction,
    pub(super) update_changed_flags: BoundRuntimeFunction,
}

impl ComposeRuntime {
    /// Select every runtime function the pass calls, or say which one the classpath lacks.
    pub(super) fn bind(runtime: &mut dyn PluginRuntime) -> Result<Self, String> {
        let composer = composer();
        let package = RuntimeDeclaration::Package(type_name(RUNTIME));
        let member = RuntimeDeclaration::Member(type_name(COMPOSER));
        let mut function = |declaration, name, params: Vec<Ty>, ret| {
            runtime.function(&RuntimeFunction {
                declaration,
                name,
                params,
                ret,
            })
        };
        Ok(Self {
            start_restart_group: function(member, "startRestartGroup", vec![Ty::Int], composer)?,
            end_restart_group: function(
                member,
                "endRestartGroup",
                Vec::new(),
                Ty::nullable(Ty::obj(SCOPE_UPDATE_SCOPE)),
            )?,
            should_execute: function(
                member,
                "shouldExecute",
                vec![Ty::Boolean, Ty::Int],
                Ty::Boolean,
            )?,
            skip_to_group_end: function(member, "skipToGroupEnd", Vec::new(), Ty::Unit)?,
            update_scope: function(
                RuntimeDeclaration::Member(type_name(SCOPE_UPDATE_SCOPE)),
                "updateScope",
                vec![Ty::fun(vec![composer, Ty::Int], Ty::Unit)],
                Ty::Unit,
            )?,
            source_information: function(
                package,
                "sourceInformation",
                vec![composer, Ty::String],
                Ty::Unit,
            )?,
            is_trace_in_progress: function(package, "isTraceInProgress", Vec::new(), Ty::Boolean)?,
            trace_event_start: function(
                package,
                "traceEventStart",
                vec![Ty::Int, Ty::Int, Ty::Int, Ty::String],
                Ty::Unit,
            )?,
            trace_event_end: function(package, "traceEventEnd", Vec::new(), Ty::Unit)?,
            update_changed_flags: function(package, "updateChangedFlags", vec![Ty::Int], Ty::Int)?,
        })
    }
}

/// A call of the selected runtime function `function` on `receiver`, or of a top-level one.
pub(super) fn call(
    ir: &mut IrFile,
    function: &BoundRuntimeFunction,
    receiver: Option<ExprId>,
    args: Vec<ExprId>,
) -> ExprId {
    ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::External {
            target: function.target,
            default_provider: None,
            params: function.params.clone(),
            ret: function.ret,
            substitutions: Vec::new(),
            defaults: Vec::new(),
            extension_receiver_parameter: None,
        }),
        dispatch_receiver: receiver,
        args,
    })
}

/// `@FunctionKeyMeta(key = …, startOffset = …, endOffset = …)`, binary-retained like the runtime
/// declares it.
pub(super) fn function_key_meta(function: &RestartableFunction) -> RetainedAnnotation {
    let (start, end) = function.offsets;
    RetainedAnnotation {
        retention: AnnoRetention::Binary,
        annotation: AppliedAnnotation {
            internal: type_name(FUNCTION_KEY_META),
            values: vec![
                (
                    "key".to_string(),
                    AnnoValue::Const(IrConst::Int(function.key)),
                ),
                (
                    "startOffset".to_string(),
                    AnnoValue::Const(IrConst::Int(start)),
                ),
                ("endOffset".to_string(), AnnoValue::Const(IrConst::Int(end))),
            ],
        },
        facts: Default::default(),
    }
}

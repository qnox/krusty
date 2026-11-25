//! The Compose runtime API the generated code calls: the declarations kotlinc's plugin binds
//! (`ComposeCallableIds` and the `Composer` members it names), selected at the handoff by their
//! Kotlin signatures and called by their selected identities.

use crate::ir::{
    AnnoRetention, AnnoValue, AppliedAnnotation, Callee, ExprId, IrConst, IrExpr, IrFile,
    RetainedAnnotation,
};
use crate::plugins::runtime_declarations::{
    BoundRuntimeFunction, PluginRuntime, RuntimeDeclaration, RuntimeFunction, RuntimeRequirement,
};
use crate::types::{type_name, Ty};

const RUNTIME: &str = "androidx/compose/runtime";
const INTERNAL: &str = "androidx/compose/runtime/internal";
const COMPOSABLE_LAMBDA: &str = "androidx/compose/runtime/internal/ComposableLambda";
pub(super) const COMPOSER: &str = "androidx/compose/runtime/Composer";
const SCOPE_UPDATE_SCOPE: &str = "androidx/compose/runtime/ScopeUpdateScope";
const FUNCTION_KEY_META: &str = "androidx/compose/runtime/internal/FunctionKeyMeta";
const COMPOSABLE_INFERRED_TARGET: &str = "androidx/compose/runtime/ComposableInferredTarget";

pub(super) fn composer() -> Ty {
    Ty::obj(COMPOSER)
}

/// The type `ComposerParamTransformer` gives a composable `() -> Unit` value: a function of the
/// caller's composer and `$changed`.
pub(super) fn composable_lambda() -> Ty {
    composable_function(&[])
}

/// The type `ComposerParamTransformer` gives a composable function value that takes `parameters`
/// (its receiver first): the same function of them, then the caller's composer and `$changed`.
pub(super) fn composable_function(parameters: &[Ty]) -> Ty {
    let mut physical = parameters.to_vec();
    physical.extend([composer(), Ty::Int]);
    Ty::fun(physical, Ty::Unit)
}

/// The runtime functions a restartable function calls.
pub(super) struct ComposeRuntime {
    pub(super) start_restart_group: BoundRuntimeFunction,
    pub(super) end_restart_group: BoundRuntimeFunction,
    pub(super) should_execute: BoundRuntimeFunction,
    pub(super) skip_to_group_end: BoundRuntimeFunction,
    pub(super) update_scope: BoundRuntimeFunction,
    pub(super) source_information: BoundRuntimeFunction,
    /// `sourceInformationMarkerStart(composer, key, information)` and
    /// `sourceInformationMarkerEnd(composer)`, which bracket a function without a group.
    pub(super) source_information_marker_start: BoundRuntimeFunction,
    pub(super) source_information_marker_end: BoundRuntimeFunction,
    pub(super) is_trace_in_progress: BoundRuntimeFunction,
    pub(super) trace_event_start: BoundRuntimeFunction,
    pub(super) trace_event_end: BoundRuntimeFunction,
    pub(super) update_changed_flags: BoundRuntimeFunction,
    /// `Composer.changed`, by the declared parameter type each overload compares: `Any?` and one
    /// for each primitive.
    pub(super) changed: Vec<(Ty, BoundRuntimeFunction)>,
    /// `Composer.changedInstance`, which compares a function value by identity.
    pub(super) changed_instance: BoundRuntimeFunction,
    /// `composableLambdaInstance(key, tracked, block)`, the runtime's wrapper of a composable
    /// lambda that captures nothing.
    pub(super) composable_lambda_instance: BoundRuntimeFunction,
    /// `rememberComposableLambda`, itself composable: it takes the caller's composer and
    /// `$changed` after its declared parameters.
    pub(super) remember_composable_lambda: BoundRuntimeFunction,
}

/// Every runtime function [`ComposeRuntime::bind`] selects, which the pass declares up front.
pub(super) fn requirements() -> Vec<RuntimeRequirement> {
    let package = RuntimeDeclaration::Package(type_name(RUNTIME));
    let internal = RuntimeDeclaration::Package(type_name(INTERNAL));
    let composer = RuntimeDeclaration::Member(type_name(COMPOSER));
    let scope = RuntimeDeclaration::Member(type_name(SCOPE_UPDATE_SCOPE));
    let named = |declaration, names: &[&'static str]| {
        names
            .iter()
            .map(|name| RuntimeRequirement { declaration, name })
            .collect::<Vec<_>>()
    };
    [
        named(
            composer,
            &[
                "startRestartGroup",
                "endRestartGroup",
                "shouldExecute",
                "skipToGroupEnd",
                "changed",
                "changedInstance",
            ],
        ),
        named(scope, &["updateScope"]),
        named(
            package,
            &[
                "sourceInformation",
                "sourceInformationMarkerStart",
                "sourceInformationMarkerEnd",
                "isTraceInProgress",
                "traceEventStart",
                "traceEventEnd",
                "updateChangedFlags",
            ],
        ),
        named(
            internal,
            &["composableLambdaInstance", "rememberComposableLambda"],
        ),
    ]
    .concat()
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
                appended: Vec::new(),
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
                vec![composable_lambda()],
                Ty::Unit,
            )?,
            source_information: function(
                package,
                "sourceInformation",
                vec![composer, Ty::String],
                Ty::Unit,
            )?,
            source_information_marker_start: function(
                package,
                "sourceInformationMarkerStart",
                vec![composer, Ty::Int, Ty::String],
                Ty::Unit,
            )?,
            source_information_marker_end: function(
                package,
                "sourceInformationMarkerEnd",
                vec![composer],
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
            changed: std::iter::once(nullable_any())
                .chain(PRIMITIVES)
                .map(|compared| {
                    function(member, "changed", vec![compared], Ty::Boolean)
                        .map(|bound| (compared, bound))
                })
                .collect::<Result<_, _>>()?,
            changed_instance: function(
                member,
                "changedInstance",
                vec![nullable_any()],
                Ty::Boolean,
            )?,
            composable_lambda_instance: function(
                RuntimeDeclaration::Package(type_name(INTERNAL)),
                "composableLambdaInstance",
                vec![Ty::Int, Ty::Boolean, Ty::obj("kotlin/Any")],
                Ty::obj(COMPOSABLE_LAMBDA),
            )?,
            // Itself composable: compiled with the transform's `$composer` and `$changed`.
            remember_composable_lambda: runtime.function(&RuntimeFunction {
                declaration: RuntimeDeclaration::Package(type_name(INTERNAL)),
                name: "rememberComposableLambda",
                params: vec![Ty::Int, Ty::Boolean, Ty::obj("kotlin/Any")],
                ret: Ty::obj(COMPOSABLE_LAMBDA),
                appended: vec![Ty::nullable(composer), Ty::Int],
            })?,
        })
    }

    /// The `changed` overload kotlinc's `irChanged` calls for a value of the stable type `ty`: the
    /// primitive one that type has, or the one taking `Any?`.
    pub(super) fn changed_for(&self, ty: Ty) -> &BoundRuntimeFunction {
        let compared = if PRIMITIVES.contains(&ty) {
            ty
        } else {
            nullable_any()
        };
        &self
            .changed
            .iter()
            .find(|(declared, _)| *declared == compared)
            .expect("every compared type has a bound `changed` overload")
            .1
    }
}

fn nullable_any() -> Ty {
    Ty::nullable(Ty::obj("kotlin/Any"))
}

/// The primitive types `Composer.changed` has an overload for, beside its `Any?` one.
pub(super) const PRIMITIVES: [Ty; 8] = [
    Ty::Boolean,
    Ty::Char,
    Ty::Byte,
    Ty::Short,
    Ty::Int,
    Ty::Float,
    Ty::Long,
    Ty::Double,
];

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

/// `@ComposableInferredTarget(scheme)`, the applier scheme inferred for a function's lambdas.
pub(super) fn composable_inferred_target(scheme: &str) -> RetainedAnnotation {
    RetainedAnnotation {
        retention: AnnoRetention::Binary,
        annotation: AppliedAnnotation {
            written_order: None,
            internal: type_name(COMPOSABLE_INFERRED_TARGET),
            values: vec![(
                "scheme".to_string(),
                AnnoValue::Const(IrConst::String(scheme.into())),
            )],
        },
        facts: Default::default(),
    }
}

/// `@Composable`, binary-retained like the runtime declares it, which the function a composable
/// lambda is lowered to carries.
pub(super) fn composable() -> RetainedAnnotation {
    RetainedAnnotation {
        retention: AnnoRetention::Binary,
        annotation: AppliedAnnotation {
            written_order: None,
            internal: type_name(super::COMPOSABLE),
            values: Vec::new(),
        },
        facts: Default::default(),
    }
}

/// `@FunctionKeyMeta(key = …, startOffset = …, endOffset = …)`, binary-retained like the runtime
/// declares it.
pub(super) fn function_key_meta(key: i32, (start, end): (i32, i32)) -> RetainedAnnotation {
    RetainedAnnotation {
        retention: AnnoRetention::Binary,
        annotation: AppliedAnnotation {
            written_order: None,
            internal: type_name(FUNCTION_KEY_META),
            values: vec![
                ("key".to_string(), AnnoValue::Const(IrConst::Int(key))),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::runtime_declarations::PluginClassifier;

    /// A runtime that answers every request and records what was asked.
    struct Recording(Vec<RuntimeRequirement>);

    impl PluginRuntime for Recording {
        fn function(&mut self, function: &RuntimeFunction) -> Result<BoundRuntimeFunction, String> {
            self.0.push(RuntimeRequirement {
                declaration: function.declaration,
                name: function.name,
            });
            Ok(BoundRuntimeFunction {
                target: crate::ir::ExternalCallableId::from_raw(0),
                params: function.params.clone(),
                ret: function.ret,
            })
        }

        fn classifier(&self, _classifier: crate::types::TypeName) -> Option<PluginClassifier> {
            None
        }

        fn appended_parameters(
            &mut self,
            _target: crate::ir::ExternalCallableId,
            _appended: Vec<Ty>,
        ) {
        }
    }

    #[test]
    fn the_declared_requirements_are_every_function_the_pass_binds() {
        let mut runtime = Recording(Vec::new());
        ComposeRuntime::bind(&mut runtime).expect("the recording runtime answers every request");
        let declared = requirements();
        for requested in &runtime.0 {
            assert!(declared.contains(requested), "undeclared: {requested:?}");
        }
        for requirement in &declared {
            assert!(
                runtime.0.contains(requirement),
                "declared but never requested: {requirement:?}"
            );
        }
    }
}

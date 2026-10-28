//! The Compose runtime API the generated code calls, by the exact JVM declarations kotlinc's plugin
//! binds (`ComposeCallableIds` and the `Composer` interface members it names).

use crate::ir::{
    AnnoRetention, AnnoValue, AppliedAnnotation, Callee, ExprId, IrConst, IrExpr, IrFile,
    RetainedAnnotation,
};
use crate::libraries::InlineKind;
use crate::types::{type_name, Ty, TypeName};

use super::source_facts::RestartableFunction;

const COMPOSER: &str = "androidx/compose/runtime/Composer";
const COMPOSER_KT: &str = "androidx/compose/runtime/ComposerKt";
const RECOMPOSE_SCOPE_IMPL_KT: &str = "androidx/compose/runtime/RecomposeScopeImplKt";
const SCOPE_UPDATE_SCOPE: &str = "androidx/compose/runtime/ScopeUpdateScope";
const FUNCTION_KEY_META: &str = "androidx/compose/runtime/internal/FunctionKeyMeta";

/// One runtime method: its owner, name and descriptor.
pub(super) struct RuntimeMethod {
    owner: &'static str,
    name: &'static str,
    descriptor: &'static str,
}

pub(super) const START_RESTART_GROUP: RuntimeMethod = RuntimeMethod {
    owner: COMPOSER,
    name: "startRestartGroup",
    descriptor: "(I)Landroidx/compose/runtime/Composer;",
};
pub(super) const END_RESTART_GROUP: RuntimeMethod = RuntimeMethod {
    owner: COMPOSER,
    name: "endRestartGroup",
    descriptor: "()Landroidx/compose/runtime/ScopeUpdateScope;",
};
pub(super) const SHOULD_EXECUTE: RuntimeMethod = RuntimeMethod {
    owner: COMPOSER,
    name: "shouldExecute",
    descriptor: "(ZI)Z",
};
pub(super) const SKIP_TO_GROUP_END: RuntimeMethod = RuntimeMethod {
    owner: COMPOSER,
    name: "skipToGroupEnd",
    descriptor: "()V",
};
pub(super) const SOURCE_INFORMATION: RuntimeMethod = RuntimeMethod {
    owner: COMPOSER_KT,
    name: "sourceInformation",
    descriptor: "(Landroidx/compose/runtime/Composer;Ljava/lang/String;)V",
};
pub(super) const IS_TRACE_IN_PROGRESS: RuntimeMethod = RuntimeMethod {
    owner: COMPOSER_KT,
    name: "isTraceInProgress",
    descriptor: "()Z",
};
pub(super) const TRACE_EVENT_START: RuntimeMethod = RuntimeMethod {
    owner: COMPOSER_KT,
    name: "traceEventStart",
    descriptor: "(IIILjava/lang/String;)V",
};
pub(super) const TRACE_EVENT_END: RuntimeMethod = RuntimeMethod {
    owner: COMPOSER_KT,
    name: "traceEventEnd",
    descriptor: "()V",
};
pub(super) const UPDATE_CHANGED_FLAGS: RuntimeMethod = RuntimeMethod {
    owner: RECOMPOSE_SCOPE_IMPL_KT,
    name: "updateChangedFlags",
    descriptor: "(I)I",
};

pub(super) fn composer() -> Ty {
    Ty::obj(COMPOSER)
}

pub(super) fn scope_update_scope_name() -> TypeName {
    type_name(SCOPE_UPDATE_SCOPE)
}

pub(super) fn scope_update_scope() -> Ty {
    Ty::obj(SCOPE_UPDATE_SCOPE)
}

/// `receiver.method(args)` on the `Composer` interface.
pub(super) fn composer_call(
    ir: &mut IrFile,
    receiver: ExprId,
    method: RuntimeMethod,
    args: Vec<ExprId>,
) -> ExprId {
    ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::realized_virtual(
            type_name(method.owner),
            method.name.to_string(),
            method.descriptor.to_string(),
            None,
            true,
        )),
        dispatch_receiver: Some(receiver),
        args,
    })
}

/// `method(args)` on a runtime file facade.
pub(super) fn static_call(ir: &mut IrFile, method: RuntimeMethod, args: Vec<ExprId>) -> ExprId {
    ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Static {
            owner: type_name(method.owner),
            name: method.name.to_string(),
            descriptor: method.descriptor.to_string(),
            inline: InlineKind::None,
        }),
        dispatch_receiver: None,
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

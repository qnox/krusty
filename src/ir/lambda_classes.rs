//! A lambda a target realizes as a class of its own, and the facts its erased `invoke` bridge is
//! written from.
//!
//! A backend that builds closures at run time (the JVM's `LambdaMetafactory`) still compiles some
//! lambdas to a class: one whose signature the runtime factory cannot adapt. The lambda's function
//! becomes the class's `invoke` and its captured values the class's fields. The record below is a
//! target realization record, filled by the pass that makes the class and completed by the
//! representation passes that erase `invoke`; emission executes it.

use super::*;

/// A lambda realized as a class implementing its function type. `invoke` is the lambda's own
/// function, now an instance method of the class over the lambda's parameters. The class's fields
/// are the captured values, in capture order; `receiver_capture` names the one holding the
/// enclosing class's instance.
#[derive(Clone, Debug)]
pub struct IrLambdaClass {
    pub invoke: FunId,
    /// The lambda's function type: the class's `FunctionN` and its generic supertype.
    pub function_type: Ty,
    pub receiver_capture: Option<u32>,
    /// What the erased `FunctionN.invoke` bridge adapts, once `invoke` has its physical signature.
    pub bridge: IrInvokeBridge,
}

/// How an erased `FunctionN.invoke(Object…)Object` reaches a class's own specialized `invoke`.
/// `param_tys`/`ret_ty` are the logical function-type signature. Per parameter, `unbox_params`
/// names the value class whose box arrives where `invoke` takes its carrier, and
/// `unbox_param_nullable` whether that box may be null. `box_ret` names the value class whose
/// carrier `invoke` returns where the bridge returns its box. `invoke_renamed` records that the
/// specialized `invoke` took a mangled name, so a bridge is needed even when the descriptors agree.
#[derive(Clone, Debug)]
pub struct IrInvokeBridge {
    pub param_tys: Vec<Ty>,
    pub ret_ty: Ty,
    pub unbox_params: Vec<Option<TypeName>>,
    pub unbox_param_nullable: Vec<bool>,
    pub box_ret: Option<TypeName>,
    pub invoke_renamed: bool,
}

impl IrInvokeBridge {
    /// A bridge over the logical signature alone, before any representation pass adapts it.
    pub fn logical(param_tys: Vec<Ty>, ret_ty: Ty) -> Self {
        Self {
            param_tys,
            ret_ty,
            unbox_params: Vec::new(),
            unbox_param_nullable: Vec::new(),
            box_ret: None,
            invoke_renamed: false,
        }
    }
}

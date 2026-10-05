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
/// are the captured values, in capture order; `captures` keeps what each one was in source, and a
/// target spells the field and constructor parameter from it.
#[derive(Clone, Debug)]
pub struct IrLambdaClass {
    /// A checked public inline declaration exposes this closure to callers in other packages.
    pub public_inline: bool,
    pub invoke: FunId,
    /// The lambda's function type: the class's `FunctionN` and its generic supertype.
    pub function_type: Ty,
    /// What each field captures, in field order.
    pub captures: Vec<IrLambdaCapture>,
    /// The origins of the implicit receivers the lambda captured, which
    /// [`IrLambdaCapture::Receiver`] indexes; the lambda's own record, which `invoke` no longer has.
    pub captured_receivers: Vec<IrCapturedReceiver>,
    /// Where the lambda was lifted from, which `invoke` no longer records: a target that realizes
    /// that declaration differently (a value-class member as a static) names a captured receiver
    /// after that realization.
    pub(crate) lifting_root: Option<IrLiftingRoot>,
    /// What the erased `FunctionN.invoke` bridge adapts, once `invoke` has its physical signature.
    pub bridge: IrInvokeBridge,
}

/// One captured value a lambda class stores in a field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IrLambdaCapture {
    /// A captured local value, by its source name.
    Value(Box<str>),
    /// A captured implicit receiver, by its position among [`IrLambdaClass::captured_receivers`].
    Receiver(u32),
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

/// The class a target writes for function values converted to one fun interface: shared by every
/// such conversion in the file, it holds the value in its one field, and `method` implements the
/// interface's single method by calling the value's `invoke`.
#[derive(Clone, Debug)]
pub struct IrSamWrapperClass {
    pub interface: TypeName,
    pub method: FunId,
    /// This generated method's primitive semantic result is a boxed JVM result.
    pub boxes_primitive_result: bool,
    /// Logical value-parameter count of a suspend method; its function field adds a continuation.
    pub suspend_arity: Option<u8>,
    /// An inline declaration exposes this generated class to its inlined call sites.
    pub public_inline: bool,
    /// A Kotlin fun interface also implements `FunctionAdapter` so two wrappers of one function
    /// compare equal. A Java interface under `-Xsam-conversions=class` implements only itself.
    pub function_adapter: bool,
}

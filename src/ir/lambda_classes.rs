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

/// The class a target writes for function values converted to one fun interface: shared by every
/// such conversion in the file, it holds the value in its one field, and `method` implements the
/// interface's single method by calling the value's `invoke`.
#[derive(Clone, Debug)]
pub struct IrSamWrapperClass {
    pub interface: TypeName,
    pub method: FunId,
}

/// Checked functional-interface target attached to a lambda after SAM conversion.
///
/// Both the call-site-specialized shape and the declaration shape are retained: the former types
/// the implementation while the latter determines the platform method that the closure implements.
/// This is frontend semantic data; platform owner spellings and descriptors do not belong here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrSamTarget {
    pub classifier: TypeName,
    pub method: String,
    pub parameters: Vec<Ty>,
    pub result: Ty,
    pub declared_parameters: Vec<Ty>,
    pub declared_result: Ty,
    pub context_count: u32,
    pub has_receiver: bool,
    pub suspend: bool,
    /// The method's primitive result replaces a non-primitive result it overrides.
    pub overrides_non_primitive_result: bool,
    /// A fun-interface conversion of a callable reference delegates equality/hashCode through
    /// Kotlin's `FunctionAdapter` contract. Ordinary lambdas remain identity objects.
    pub function_adapter: bool,
    /// The conversion wraps an existing function value (neither a lambda literal nor a callable
    /// reference): the lambda only forwards to the captured value's `invoke`.
    pub wraps_function_value: bool,
    /// The interface is a Kotlin declaration, not a Java one.
    pub kotlin_interface: bool,
    /// The method's declared parameter names.
    pub parameter_names: Vec<String>,
}

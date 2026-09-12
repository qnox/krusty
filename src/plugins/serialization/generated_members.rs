//! Construction of methods owned by serialization-plugin generated classes.

use crate::ir::{ExprId, FnParamInfo, IrFile, IrFunction};
use crate::types::{Ty, TypeName};

/// A non-null generated-method parameter whose source-visible name is also used by the JVM entry
/// guard. Keeping the type and name paired prevents the guard, debug table, and Kotlin metadata
/// from drifting to different physical parameter positions.
pub(super) struct GuardedParameter {
    ty: Ty,
    name: &'static str,
}

impl GuardedParameter {
    pub(super) fn new(ty: Ty, name: &'static str) -> Self {
        Self { ty, name }
    }
}

/// Add an instance method to a plugin-generated class and return its `FunId`.
pub(super) fn add_instance_method(
    ir: &mut IrFile,
    owner: TypeName,
    name: &str,
    params: Vec<Ty>,
    ret: Ty,
    body: Option<ExprId>,
) -> u32 {
    ir.add_fun(IrFunction {
        name: name.to_string(),
        params,
        ret,
        body,
        is_static: false,
        dispatch_receiver: Some(owner),
        param_checks: Vec::new(),
    })
}

/// Add an instance method whose non-null parameters have kotlinc-compatible entry guards.
///
/// The same names are recorded for Kotlin metadata and debug-table emission. A generated member is
/// public API, so a Java caller can pass `null`; kotlinc guards it just like a source declaration.
pub(super) fn add_guarded_instance_method(
    ir: &mut IrFile,
    owner: TypeName,
    name: &str,
    params: Vec<GuardedParameter>,
    ret: Ty,
    body: Option<ExprId>,
) -> u32 {
    let parameter_types = params.iter().map(|parameter| parameter.ty).collect();
    let parameter_names = params
        .iter()
        .map(|parameter| parameter.name.to_string())
        .collect::<Vec<_>>();
    let param_checks = parameter_names.iter().cloned().map(Some).collect();
    let function = ir.add_fun(IrFunction {
        name: name.to_string(),
        params: parameter_types,
        ret,
        body,
        is_static: false,
        dispatch_receiver: Some(owner),
        param_checks,
    });
    ir.fn_params
        .insert(function, FnParamInfo::names(parameter_names));
    function
}

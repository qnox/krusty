//! A top-level function whose whole body is `js("…")`, which kotlinc's `JsCodeCallsLowering`
//! turns into a JavaScript function the wasm-js host supplies (see `crate::js_code`).

use krusty::backend::{BackendCompilerIntrinsic, CheckedBackendCallables};
use krusty::ir::{Callee, ExprId, FunId, IrConst, IrExpr, IrFile};
use krusty::types::Ty;

use super::super::super::js_code::{arrow_function, Crossing};
use super::Unsupported;

/// The JavaScript a function's body is, and how its parameters reach it.
pub(super) struct JsCodeFunction {
    pub(super) code: String,
    pub(super) crossings: Vec<Crossing>,
}

/// The JavaScript function `function` is, when its body is one `js(code)` call that it returns or
/// only makes. `None` for any other body.
pub(super) fn js_code_function(
    ir: &IrFile,
    callables: &CheckedBackendCallables,
    function: FunId,
) -> Result<Option<JsCodeFunction>, Unsupported> {
    let declaration = &ir.functions[function as usize];
    let Some(root) = declaration.body else {
        return Ok(None);
    };
    let Some((code, returns_the_call)) = body_code(ir, callables, root) else {
        return Ok(None);
    };
    if declaration.dispatch_receiver.is_some() {
        return Err("a member function whose body is `js(…)`".to_string());
    }
    let parameters = ir
        .fn_params
        .get(&function)
        .map(|info| {
            info.identities
                .iter()
                .map(|identity| identity.source_name.clone())
                .collect::<Option<Vec<_>>>()
        })
        .unwrap_or_default()
        .filter(|names| names.len() == declaration.params.len())
        .ok_or_else(|| {
            "a function whose body is `js(…)` without its parameters' source names".to_string()
        })?;
    let crossings = declaration
        .params
        .iter()
        .map(|ty| crossing(*ty))
        .collect::<Result<Vec<_>, _>>()?;
    if declaration.ret != Ty::Unit && !is_number(declaration.ret) {
        return Err(format!(
            "a JavaScript function returning `{:?}` to Kotlin",
            declaration.ret
        ));
    }
    Ok(Some(JsCodeFunction {
        code: arrow_function(&parameters, &code, returns_the_call),
        crossings,
    }))
}

/// The constant `code` of a body that is exactly `return js(code)` or `js(code)`, and whether it
/// returns the call.
fn body_code(
    ir: &IrFile,
    callables: &CheckedBackendCallables,
    root: ExprId,
) -> Option<(String, bool)> {
    // Common lowering keeps the block of the body and the block it was written as.
    let mut statement = root;
    while let IrExpr::Block { stmts, value } = ir.expr(statement) {
        statement = match (stmts.as_slice(), value) {
            ([statement], None) | ([], Some(statement)) => *statement,
            _ => return None,
        };
    }
    let (call, returns_the_call) = match ir.expr(statement) {
        IrExpr::Return(Some(value)) => (*value, true),
        _ => (statement, false),
    };
    let call = match ir.expr(call) {
        IrExpr::BottomValue { producer, .. } => *producer,
        _ => call,
    };
    let IrExpr::Call {
        callee,
        dispatch_receiver: None,
        args,
    } = ir.expr(call)
    else {
        return None;
    };
    let Callee::External { target, .. } = &**callee else {
        return None;
    };
    if callables
        .callable(*target)
        .and_then(|fact| fact.compiler_operation())
        != Some(BackendCompilerIntrinsic::JsCode)
    {
        return None;
    }
    let [code] = args.as_slice() else {
        return None;
    };
    let IrExpr::Const(IrConst::String(code)) = ir.expr(*code) else {
        return None;
    };
    Some((
        String::from_utf16_lossy(&code.units().collect::<Vec<_>>()),
        returns_the_call,
    ))
}

fn crossing(ty: Ty) -> Result<Crossing, Unsupported> {
    if ty.non_null() == Ty::String {
        Ok(Crossing::String)
    } else if is_number(ty) {
        Ok(Crossing::Number)
    } else {
        Err(format!("a `{ty:?}` parameter of a JavaScript function"))
    }
}

/// A number the JavaScript API converts both ways itself.
fn is_number(ty: Ty) -> bool {
    matches!(ty, Ty::Byte | Ty::Short | Ty::Int | Ty::Float | Ty::Double)
}

//! Another module's call to an overridable member dispatches on its receiver.
//!
//! A member's dependency export is its body: the code a class implementing it in another module
//! inherits, and what a `super` call runs. An ordinary call from another module selects the
//! declaration but must still run the receiver's override, so each exported overridable member
//! also gets an entry, an ordinary function of its file taking the receiver and the member's
//! parameters, that calls the member through its dispatch slot. A target publishes that entry
//! beside the export and realizes calls through it.

use super::DependencyExportSignatures;
use crate::ir::{ExprId, FunId, IrExpr, IrFile, IrFunction};
use crate::types::Ty;

/// Build the dispatching entry of every overridable member `exports` publishes from `ir`.
pub fn realize(ir: &mut IrFile, exports: &DependencyExportSignatures) {
    let members = exports
        .callables()
        .map(|(function, _)| function)
        .filter(|&function| {
            exports.member_receiver(function).is_some() && ir.open_methods.contains(&function)
        })
        .collect::<Vec<_>>();
    for function in members {
        if ir.dispatch_entries.contains_key(&function) {
            continue;
        }
        let receiver = exports
            .member_receiver(function)
            .expect("filtered to members")
            .semantic_type();
        if let Some(entry) = build(ir, function, receiver) {
            ir.dispatch_entries.insert(function, entry);
        }
    }
}

/// The entry of `function`, when it is one of its class's methods and so has a dispatch slot. It
/// takes `receiver`, the export's own receiver operand.
fn build(ir: &mut IrFile, function: FunId, receiver: Ty) -> Option<FunId> {
    let declaration = &ir.functions[function as usize];
    let owner = declaration.dispatch_receiver?;
    let class = ir.class_id_by_name(owner)?;
    if ir.classes[class as usize].is_value {
        return None;
    }
    // An interface member dispatches through its module-wide member number, which a function
    // declaration has and a property accessor does not: an accessor's call goes through the
    // property's own dispatch, which a method call does not express.
    if ir.classes[class as usize].is_interface
        && !ir
            .checked_callable_functions
            .values()
            .any(|&declared| declared == function)
    {
        return None;
    }
    let index = ir.classes[class as usize]
        .methods
        .iter()
        .position(|&method| method == function)?;
    let name = format!("{}$dispatch", declaration.name);
    let parameters = declaration.params.clone();
    let result = declaration.ret;
    let this = read(ir, 0, receiver);
    let arguments = parameters
        .iter()
        .enumerate()
        .map(|(ordinal, &ty)| {
            Some(read(
                ir,
                u32::try_from(ordinal + 1).expect("too many parameters"),
                ty,
            ))
        })
        .collect();
    let call = ir.add_expr(IrExpr::MethodCall {
        class,
        index: u32::try_from(index).expect("too many methods"),
        receiver: this,
        args: arguments,
    });
    ir.set_logical_type(call, result);
    let last = match result {
        Ty::Unit => call,
        _ => ir.add_expr(IrExpr::Return(Some(call))),
    };
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![last],
        value: None,
    });
    let entry = ir.add_fun(IrFunction {
        name,
        params: std::iter::once(receiver).chain(parameters).collect(),
        ret: result,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    // Compiler-generated: no source declaration of its own, and no public name.
    ir.synthetic_methods.insert(entry);
    Some(entry)
}

fn read(ir: &mut IrFile, slot: u32, ty: Ty) -> ExprId {
    let read = ir.add_expr(IrExpr::GetValue(slot));
    ir.set_logical_type(read, ty);
    read
}

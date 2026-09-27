//! The boxed result kotlinc gives a primitive override of a non-primitive declaration.
//!
//! kotlinc's JVM signature mapper boxes a function's primitive result when any declaration it
//! overrides returns something else (`forceBoxedReturnTypeOnOverride`): `override fun next(): Int`
//! of `Iterator<T>.next(): T` is `next()Ljava/lang/Integer;`, and so is `invoke` of a
//! `() -> Int` object. The override's own JVM result is the wrapper, so its body boxes once, a call
//! through the class unboxes, and the bridge to the erased declaration returns the box as it is.
//!
//! The Kotlin declaration still returns the primitive; only the JVM carrier of its result changes,
//! so this runs with the other JVM representation passes, before bridges are derived from it.

use crate::ir::{Callee, FunId, IrExpr, IrFile, IrTypeOp};
use crate::jvm::backend::SkipReason;
use crate::types::Ty;

/// The functions whose JVM result this pass boxed.
#[derive(Default)]
pub(super) struct BoxedResults(std::collections::HashSet<FunId>);

impl BoxedResults {
    /// Whether the module declaration `callable`, when realized in this file, returns a wrapper
    /// where it declares a primitive.
    pub(super) fn boxes(&self, ir: &IrFile, callable: crate::fir::CallableId) -> bool {
        ir.checked_callable_functions
            .get(&callable)
            .is_some_and(|function| self.0.contains(function))
    }
}

/// Give every source override whose primitive result replaces a non-primitive one the wrapper as
/// its JVM result, and adapt the calls that reach it.
///
/// Only a class declared in executable code takes it yet: every call to, and every override of,
/// its members is in this file. A member of a class other files can see keeps its primitive result
/// until the choice is visible to them too.
pub(super) fn box_primitive_override_results(
    ir: &mut IrFile,
    classpath: &crate::jvm::classpath::Classpath,
) -> Result<BoxedResults, SkipReason> {
    let mut boxed = BoxedResults::default();
    for class in 0..ir.classes.len() {
        // The classes whose override edges the bridge pass reads; see `derive_bridges`.
        let declaration = &ir.classes[class];
        if declaration.enclosure.is_none()
            || (!declaration.is_source_declared && declaration.enum_entry_of.is_none())
        {
            continue;
        }
        let owner = declaration.fq_name;
        let edges = ir
            .function_overrides
            .get(&owner)
            .cloned()
            .unwrap_or_default();
        for edge in edges {
            let Some(function) = crate::jvm::bridges::implementation_function(ir, &edge) else {
                continue;
            };
            if edge.implementation_owner != owner
                || !ir.classes[class].methods.contains(&function)
                || boxed.0.contains(&function)
                || !is_primitive(ir.functions[function as usize].ret)
            {
                continue;
            }
            let (_, overridden_result) =
                crate::jvm::bridges::overridden_declaration(&edge, classpath)?;
            if !is_primitive(overridden_result) {
                box_result(ir, function);
                boxed.0.insert(function);
            }
        }
    }
    if !boxed.0.is_empty() {
        box_super_call_results(ir, &boxed);
        unbox_call_results(ir, &boxed);
    }
    Ok(boxed)
}

/// Make `function`'s JVM result the wrapper of its primitive one: each value it returns is boxed.
/// A call through the class reads the wrapper and unboxes it where it wants the primitive.
fn box_result(ir: &mut IrFile, function: FunId) {
    let declaration = &mut ir.functions[function as usize];
    let boxed = Ty::nullable(declaration.ret);
    declaration.ret = boxed;
    if let Some(body) = declaration.body {
        box_returns(ir, body, boxed);
    }
}

/// A `super` call was realized with the declaration's descriptor before this pass; one reaching a
/// boxed function names the wrapper result it now has.
fn box_super_call_results(ir: &mut IrFile, boxed: &BoxedResults) {
    for expression in 0..ir.exprs.len() {
        let IrExpr::Call {
            callee:
                Callee::Special {
                    source: Some(callable),
                    ..
                },
            ..
        } = ir.exprs[expression]
        else {
            continue;
        };
        if !boxed.boxes(ir, callable) {
            continue;
        }
        let function = ir.checked_callable_functions[&callable];
        let descriptor = crate::jvm::ir_emit::function_descriptor(ir, function);
        if let IrExpr::Call {
            callee:
                Callee::Special {
                    descriptor: special,
                    ..
                },
            ..
        } = &mut ir.exprs[expression]
        {
            *special = descriptor;
        }
    }
}

/// A call through the class reads the wrapper, and unboxes it: the Kotlin call still yields the
/// primitive.
fn unbox_call_results(ir: &mut IrFile, boxed: &BoxedResults) {
    for expression in 0..ir.exprs.len() {
        let function = match &ir.exprs[expression] {
            IrExpr::MethodCall { class, index, .. } => {
                ir.classes[*class as usize].methods[*index as usize]
            }
            IrExpr::Call {
                callee:
                    Callee::Special {
                        source: Some(callable),
                        ..
                    },
                ..
            } if boxed.boxes(ir, *callable) => ir.checked_callable_functions[callable],
            _ => continue,
        };
        if !boxed.0.contains(&function) {
            continue;
        }
        let expression = crate::ir::ExprId::try_from(expression).expect("expression ids fit u32");
        let call =
            crate::jvm::operation_relocation::clone_below_representation_wrapper(ir, expression);
        let primitive = ir.functions[function as usize].ret.non_null();
        ir.exprs[expression as usize] = IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: call,
            type_operand: primitive,
        };
    }
}

/// Wrap every value returned from the function whose body contains `expression` in a coercion to
/// `boxed`. A lambda's retained inline body is walked too: a `return` surviving there leaves the
/// enclosing function.
fn box_returns(ir: &mut IrFile, expression: crate::ir::ExprId, boxed: Ty) {
    if let IrExpr::Return(Some(value)) = ir.exprs[expression as usize] {
        let coerced = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: value,
            type_operand: boxed,
        });
        ir.exprs[expression as usize] = IrExpr::Return(Some(coerced));
        box_returns(ir, value, boxed);
        return;
    }
    let mut children = Vec::new();
    crate::ir::for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
    for child in children {
        box_returns(ir, child, boxed);
    }
}

/// Kotlin's primitive types: a non-null built-in scalar, never an unsigned or a type parameter.
fn is_primitive(ty: Ty) -> bool {
    matches!(
        ty,
        Ty::Int | Ty::Byte | Ty::Short | Ty::Long | Ty::Float | Ty::Double | Ty::Boolean | Ty::Char
    )
}

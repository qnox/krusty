//! Calls whose selected source declaration has a value-class-mangled JVM realization.

use std::collections::{HashMap, HashSet};

use super::{erase_descriptor, ir_method_desc, Under};
use crate::ir::{Callee, ExprId, IrExpr, IrFile};
use crate::types::Ty;

pub(super) fn rename(
    ir: &mut IrFile,
    renamed_functions: &HashSet<u32>,
    lowered_value_members: &HashSet<u32>,
    suspend_functions: &HashSet<u32>,
    under: &Under,
) {
    // A physical owner can be the receiver's subclass rather than the selected declaration's
    // owner. Never reinterpret that owner plus the source spelling as a declaration: an inherited
    // external generic method can otherwise collide with a same-named source override whose value-
    // class result has a different JVM representation. Resolve only the exact checked module
    // identity that common IR already records on the call.
    let declarations: HashMap<crate::fir::CallableId, (String, Option<(String, Vec<Ty>)>)> = ir
        .checked_callable_functions
        .iter()
        .filter_map(|(&callable, &function)| {
            renamed_functions.contains(&function).then(|| {
                let declaration = &ir.functions[function as usize];
                let exact_descriptor = (!lowered_value_members.contains(&function)
                    && !suspend_functions.contains(&function))
                .then(|| {
                    (
                        ir_method_desc(&declaration.params, &declaration.ret),
                        declaration.params.clone(),
                    )
                });
                (callable, (declaration.name.clone(), exact_descriptor))
            })
        })
        .collect();
    // A call to a renamed declaration of this file passes that exact declaration's parameters,
    // whose value classes it takes unboxed.
    let mut declared_parameters = Vec::new();
    for (index, e) in ir.exprs.iter_mut().enumerate() {
        if let IrExpr::Call { callee, .. } = e {
            let selected = match &*callee {
                Callee::Special {
                    source: Some(source),
                    ..
                } => Some(*source),
                Callee::Virtual {
                    target:
                        Some(crate::ir::IrVirtualTarget::Function(
                            crate::fir::ResolvedFunctionOverrideTarget::Module(source),
                        )),
                    ..
                } => Some(*source),
                _ => None,
            };
            if let Some((mangled, declaration)) =
                selected.and_then(|selected| declarations.get(&selected))
            {
                match callee {
                    Callee::Special {
                        name,
                        descriptor: call_descriptor,
                        ..
                    }
                    | Callee::Virtual {
                        name,
                        descriptor: call_descriptor,
                        ..
                    } => {
                        *name = mangled.clone();
                        if let Some((descriptor, parameters)) = declaration {
                            *call_descriptor = descriptor.clone();
                            declared_parameters.push((index as ExprId, parameters.clone()));
                        } else {
                            *call_descriptor = erase_descriptor(call_descriptor, under);
                        }
                    }
                    _ => unreachable!("selected mangled calls are virtual or special"),
                }
            }
        }
    }
    for (call, parameters) in declared_parameters {
        ir.call_declared_params
            .insert(call, parameters.into_boxed_slice());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fir::{CallableId, ExternalCallableId, ResolvedFunctionOverrideTarget};
    use crate::ir::{IrFunction, IrVirtualTarget};
    use crate::types::{type_name, Ty};

    #[test]
    fn a_retargeted_external_call_does_not_adopt_a_source_override_abi() {
        let owner = type_name("review/ConcreteReceiver");
        let source = CallableId::from_raw(7);
        let external = ExternalCallableId::from_raw(11);
        let mut ir = IrFile::default();
        let function = ir.add_fun(IrFunction {
            name: "selected-mangled".to_string(),
            params: Vec::new(),
            ret: Ty::String,
            body: None,
            is_static: false,
            dispatch_receiver: Some(owner),
            param_checks: Vec::new(),
        });
        ir.checked_callable_functions.insert(source, function);

        let source_call = ir.add_expr(IrExpr::Call {
            callee: Callee::Virtual {
                owner,
                name: "selected".to_string(),
                descriptor: "()Ljava/lang/Object;".to_string(),
                params: None,
                interface: false,
                module_target: Some(source),
                target: Some(IrVirtualTarget::Function(
                    ResolvedFunctionOverrideTarget::Module(source),
                )),
            },
            dispatch_receiver: None,
            args: Vec::new(),
        });
        let external_call = ir.add_expr(IrExpr::Call {
            callee: Callee::Virtual {
                // Physical dispatch was retargeted to the source receiver class, but the selected
                // declaration remains the dependency callable and owns this descriptor.
                owner,
                name: "selected".to_string(),
                descriptor: "()Ljava/lang/Object;".to_string(),
                params: None,
                interface: false,
                module_target: None,
                target: Some(IrVirtualTarget::Function(
                    ResolvedFunctionOverrideTarget::External(external),
                )),
            },
            dispatch_receiver: None,
            args: Vec::new(),
        });

        rename(
            &mut ir,
            &[function].into_iter().collect(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );

        let call = |expression| match ir.expr(expression) {
            IrExpr::Call {
                callee: Callee::Virtual {
                    name, descriptor, ..
                },
                ..
            } => (name.as_str(), descriptor.as_str()),
            other => panic!("expected a virtual call, got {other:?}"),
        };
        assert_eq!(
            call(source_call),
            ("selected-mangled", "()Ljava/lang/String;")
        );
        assert_eq!(call(external_call), ("selected", "()Ljava/lang/Object;"));
        assert_eq!(
            ir.call_declared_params.get(&source_call).map(Box::as_ref),
            Some([].as_slice())
        );
        assert!(!ir.call_declared_params.contains_key(&external_call));
    }
}

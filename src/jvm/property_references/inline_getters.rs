//! A property reference whose getter is spliced at each read (an inline getter).
//!
//! Such a getter is never called, so the carrier cannot read the property through it. kotlinc
//! writes the splice into the carrier's own `get` instead, and the carrier stays a property
//! reference: it reflects the property and equals every other reference to it. Function-reference
//! realization hands each such reference's splicing adapter here, and the carrier takes its body.

use crate::fx_hash::HashMap;
use crate::ir::{ClassId, FunId, IrExpr, IrFile, PropRef};
use crate::types::Ty;

/// The adapter that splices each such reference's getter, by the reference expression.
#[derive(Debug, Default)]
pub(crate) struct SplicedGetters {
    adapters: HashMap<u32, FunId>,
}

impl SplicedGetters {
    pub(in crate::jvm) fn record(&mut self, expression: u32, adapter: FunId) {
        let previous = self.adapters.insert(expression, adapter);
        assert!(previous.is_none(), "one splicing adapter per reference");
    }

    pub(super) fn take(&mut self, expression: u32) -> Option<FunId> {
        self.adapters.remove(&expression)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }
}

/// Where the carrier's `get` finds the property's receiver.
#[derive(Clone, Copy, Debug)]
pub(super) enum GetReceiver {
    /// An unbound reference: `get(Object)` takes it.
    Parameter,
    /// A bound reference: the runtime base class's `receiver` field holds it.
    Bound,
    /// A top-level property: there is none.
    None,
}

impl GetReceiver {
    pub(super) fn of(bound: bool, property: &PropRef) -> Self {
        match (bound, property.static_dispatch) {
            (true, _) => Self::Bound,
            (false, false) => Self::Parameter,
            (false, true) => Self::None,
        }
    }
}

/// Turn `adapter` into `class`'s own `get`, the method kotlinc's property-reference lowering
/// writes for a spliced getter: an instance method returning the value as an object. The adapter
/// takes the receiver first (bound or not); the carrier reads it from `get`'s parameter, cast to
/// the receiver's type, or from the inherited `receiver` field.
pub(super) fn realize_own_get(
    ir: &mut IrFile,
    expression: usize,
    class: ClassId,
    adapter: FunId,
    receiver: GetReceiver,
) {
    let internal = ir.classes[class as usize].fq_name;
    let body = ir.functions[adapter as usize]
        .body
        .expect("a splicing adapter has a body");
    let expressions = crate::ir::value_namespace_expressions(ir, body);
    let receiver_reads = expressions
        .iter()
        .copied()
        .filter(|&current| matches!(ir.exprs[current as usize], IrExpr::GetValue(0)))
        .collect::<Vec<_>>();
    let receiver_ty = ir.functions[adapter as usize].params.first().copied();
    let any = Ty::nullable(Ty::obj("kotlin/Any"));
    let params = match receiver {
        GetReceiver::Parameter => {
            // `this` takes value 0, and the receiver parameter moves to value 1.
            crate::ir::shift_value_indices(ir, body, 0, 1);
            let receiver_ty =
                receiver_ty.expect("an unbound reference's adapter takes its receiver");
            for &current in &receiver_reads {
                let read = ir.add_expr(IrExpr::GetValue(1));
                ir.exprs[current as usize] = IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::ImplicitCoercion,
                    arg: read,
                    type_operand: receiver_ty,
                };
            }
            vec![any]
        }
        GetReceiver::Bound => {
            // `this` takes the receiver's value 0; the receiver is read from the base class field.
            let receiver_ty = receiver_ty.expect("a bound reference's adapter takes its receiver");
            let field = ir.classes[class as usize].fields.len();
            ir.classes[class as usize].fields.push(crate::ir::IrField {
                name: "receiver".to_string(),
                ty: any,
                constructor_store_line: 0,
                type_param: None,
                default: None,
                flags: crate::ir::IrfFlags::default(),
            });
            for &current in &receiver_reads {
                let this = ir.add_expr(IrExpr::GetValue(0));
                let read = ir.add_expr(IrExpr::GetField {
                    receiver: this,
                    class,
                    index: u32::try_from(field).expect("a reference carrier has few fields"),
                });
                ir.exprs[current as usize] = IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::ImplicitCoercion,
                    arg: read,
                    type_operand: receiver_ty,
                };
            }
            Vec::new()
        }
        GetReceiver::None => {
            crate::ir::shift_value_indices(ir, body, 0, 1);
            Vec::new()
        }
    };
    // The adapter returns the property's value; `get` returns it as an object.
    for &current in &expressions {
        let IrExpr::Return(Some(value)) = ir.exprs[current as usize] else {
            continue;
        };
        let boxed = ir.add_expr(IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg: value,
            type_operand: any,
        });
        ir.exprs[current as usize] = IrExpr::Return(Some(boxed));
    }
    // kotlinc attributes the body to the reference's line until the splice's own lines begin:
    // the receiver's read is on it.
    if let Some(line) = ir.source_line(expression as u32) {
        ir.fn_decl_lines.insert(adapter, line);
        for &current in &receiver_reads {
            ir.set_source_line(current, line);
        }
    }
    let identities = (0..params.len())
        .map(|ordinal| {
            crate::ir::IrParameterIdentity::generated(
                crate::ir::IrGeneratedParameterRole::PropertyReferenceGetReceiver {
                    ordinal: u32::try_from(ordinal).expect("a property reference has one receiver"),
                },
                None,
            )
        })
        .collect();
    ir.fn_params
        .insert(adapter, crate::ir::FnParamInfo::identities(identities));
    let owners = ir
        .classes
        .iter()
        .enumerate()
        .filter(|(_, owner)| owner.methods.contains(&adapter))
        .map(|(owner, _)| owner)
        .collect::<Vec<_>>();
    for owner in owners {
        ir.classes[owner]
            .methods
            .retain(|&method| method != adapter);
    }
    let function = &mut ir.functions[adapter as usize];
    function.name = "get".to_string();
    function.params = params;
    function.ret = any;
    function.is_static = false;
    function.dispatch_receiver = Some(internal);
    function.param_checks = vec![None; function.params.len()];
    ir.set_method_visibility(adapter, crate::types::Visibility::Public);
    // It overrides the runtime base class's `get` and stays open, as kotlinc writes it.
    ir.open_methods.insert(adapter);
    ir.synthetic_methods.remove(&adapter);
    ir.class_static_local_functions.remove(&adapter);
    ir.lambda_own_params_from.remove(&adapter);
    ir.fn_debug_locals.insert(adapter);
    // kotlinc writes no nullability annotations on a reference carrier's `get`.
    ir.jvm_nullability_unannotated_methods.insert(adapter);
    let carrier = &mut ir.classes[class as usize];
    carrier.methods.push(adapter);
    carrier
        .prop_ref
        .as_mut()
        .expect("the carrier was just built as a property reference")
        .get = Some(adapter);
}

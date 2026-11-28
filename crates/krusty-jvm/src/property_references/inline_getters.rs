//! A property reference whose getter is spliced at each read (an inline getter).
//!
//! Such a getter is never called, so the carrier cannot read the property through it. kotlinc
//! writes the splice into the carrier's own `get` instead, and the carrier stays a property
//! reference: it reflects the property and equals every other reference to it. Function-reference
//! realization hands each such reference's splicing adapter here, and the carrier takes its body.

use krusty::fx_hash::HashMap;
use krusty::ir::{ClassId, FunId, IrExpr, IrFile, PropRef};
use krusty::types::Ty;

/// The adapter that splices each such reference's getter, by the reference expression.
#[derive(Debug, Default)]
pub(crate) struct SplicedGetters {
    adapters: HashMap<u32, FunId>,
}

impl SplicedGetters {
    pub(crate) fn record(&mut self, expression: u32, adapter: FunId) {
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
    let expressions = krusty::ir::value_namespace_expressions(ir, body);
    // The adapter takes the bound context arguments first, then the receiver, if it has one.
    let contexts = ir.classes[class as usize]
        .prop_ref
        .as_ref()
        .map(|property| property.context_types.clone())
        .expect("a carrier with a spliced getter is a property reference");
    let context_count = u32::try_from(contexts.len()).expect("few context parameters");
    let has_receiver = !matches!(receiver, GetReceiver::None);
    let adapter_parameters = context_count + u32::from(has_receiver);
    let reads_of = |ir: &IrFile, slot: u32| {
        expressions
            .iter()
            .copied()
            .filter(|&current| matches!(ir.exprs[current as usize], IrExpr::GetValue(index) if index == slot))
            .collect::<Vec<_>>()
    };
    let receiver_reads = if has_receiver {
        reads_of(ir, context_count)
    } else {
        Vec::new()
    };
    let context_reads = (0..context_count)
        .map(|slot| reads_of(ir, slot))
        .collect::<Vec<_>>();
    let receiver_ty = has_receiver
        .then(|| {
            ir.functions[adapter as usize]
                .params
                .get(context_count as usize)
        })
        .flatten()
        .copied();
    let any = Ty::nullable(Ty::obj("kotlin/Any"));
    // `this` takes value 0 and an unbound reference's `get(Object)` parameter value 1: the
    // body's locals follow them where its parameters were.
    let first_local = 1 + u32::from(matches!(receiver, GetReceiver::Parameter));
    krusty::ir::remap_forest_value_indices(ir, &[body], adapter_parameters, &|index| {
        index - adapter_parameters + first_local
    });
    // The contexts live in the runtime base class's `boundContextArguments`.
    if !contexts.is_empty() {
        let array = Ty::array(Ty::obj("kotlin/Any"));
        let field = ir.classes[class as usize].fields.len();
        ir.classes[class as usize].fields.push(krusty::ir::IrField {
            name: "boundContextArguments".to_string(),
            ty: array,
            constructor_store_line: 0,
            type_param: None,
            default: None,
            flags: krusty::ir::IrfFlags::default(),
        });
        for (index, (reads, &context_ty)) in context_reads.iter().zip(&contexts).enumerate() {
            for &current in reads {
                let this = ir.add_expr(IrExpr::GetValue(0));
                let held = ir.add_expr(IrExpr::GetField {
                    receiver: this,
                    class,
                    index: u32::try_from(field).expect("a reference carrier has few fields"),
                });
                let position = ir.add_expr(IrExpr::Const(krusty::ir::IrConst::Int(
                    i32::try_from(index).expect("few context parameters"),
                )));
                let element = ir.add_expr(IrExpr::Call {
                    callee: Box::new(krusty::ir::Callee::Intrinsic {
                        operation: krusty::ir::IrIntrinsic::ArrayGet,
                        ret: any,
                    }),
                    dispatch_receiver: Some(held),
                    args: vec![position],
                });
                ir.exprs[current as usize] =
                    IrExpr::type_op(krusty::ir::IrTypeOp::ImplicitCoercion, element, context_ty);
            }
        }
    }
    let params = match receiver {
        GetReceiver::Parameter => {
            let receiver_ty =
                receiver_ty.expect("an unbound reference's adapter takes its receiver");
            for &current in &receiver_reads {
                let read = ir.add_expr(IrExpr::GetValue(1));
                ir.exprs[current as usize] =
                    IrExpr::type_op(krusty::ir::IrTypeOp::ImplicitCoercion, read, receiver_ty);
            }
            vec![any]
        }
        GetReceiver::Bound => {
            // The receiver is read from the base class field.
            let receiver_ty = receiver_ty.expect("a bound reference's adapter takes its receiver");
            let field = ir.classes[class as usize].fields.len();
            ir.classes[class as usize].fields.push(krusty::ir::IrField {
                name: "receiver".to_string(),
                ty: any,
                constructor_store_line: 0,
                type_param: None,
                default: None,
                flags: krusty::ir::IrfFlags::default(),
            });
            for &current in &receiver_reads {
                let this = ir.add_expr(IrExpr::GetValue(0));
                let read = ir.add_expr(IrExpr::GetField {
                    receiver: this,
                    class,
                    index: u32::try_from(field).expect("a reference carrier has few fields"),
                });
                ir.exprs[current as usize] =
                    IrExpr::type_op(krusty::ir::IrTypeOp::ImplicitCoercion, read, receiver_ty);
            }
            Vec::new()
        }
        GetReceiver::None => Vec::new(),
    };
    // The adapter returns the property's value; `get` returns it as an object.
    for &current in &expressions {
        let IrExpr::Return(Some(value)) = ir.exprs[current as usize] else {
            continue;
        };
        let boxed = ir.add_expr(IrExpr::type_op(
            krusty::ir::IrTypeOp::ImplicitCoercion,
            value,
            any,
        ));
        ir.exprs[current as usize] = IrExpr::Return(Some(boxed));
    }
    // kotlinc attributes the body to the reference's line until the splice's own lines begin:
    // the receiver's read is on it.
    if let Some(line) = ir.source_line(expression as u32) {
        ir.function_position_mut(adapter).decl_line = Some(line);
        for &current in &receiver_reads {
            ir.set_source_line(current, line);
        }
    }
    let identities = (0..params.len())
        .map(|ordinal| {
            krusty::ir::IrParameterIdentity::generated(
                krusty::ir::IrGeneratedParameterRole::PropertyReferenceGetReceiver {
                    ordinal: u32::try_from(ordinal).expect("a property reference has one receiver"),
                },
                None,
            )
        })
        .collect();
    ir.fn_params
        .insert(adapter, krusty::ir::FnParamInfo::identities(identities));
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
    ir.set_method_visibility(adapter, krusty::types::Visibility::Public);
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

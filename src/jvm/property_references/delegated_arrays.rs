//! kotlinc's `$$delegatedProperties`: the reflected properties a class's delegated-property
//! operators receive, kept in one static array per class instead of one field per property.
//!
//! kotlinc (`PropertyReferenceLowering`) gives every class whose delegated properties need their
//! `KProperty` a `static final synthetic` array, filled first in its `<clinit>` in the order the
//! properties are declared, and reads each operand as `$$delegatedProperties[i]`. This module
//! chooses that representation for the delegated references checked common IR records; the
//! emitter declares and fills the arrays it publishes.

use crate::fx_hash::HashMap;

use crate::fir::{LocalDelegatedPropertyId, PropertyId};
use crate::ir::{Callee, ExprId, IrConst, IrExpr, IrFile, IrIntrinsic};
use crate::jvm::inline::MethodBodies;
use crate::jvm::method_node::MethodNode;
use crate::types::{Ty, TypeName};

/// The access `MethodNode::read` decodes a body with; a receiver is counted separately.
const ACC_STATIC: u16 = 0x0008;

pub(crate) const FIELD: &str = "$$delegatedProperties";
pub(crate) const DESCRIPTOR: &str = "[Lkotlin/reflect/KProperty;";
pub(crate) const SIGNATURE: &str = "[Lkotlin/reflect/KProperty<Ljava/lang/Object;>;";

/// The array's type: kotlinc declares it `Array<KProperty<*>>`, written `KProperty<Object>[]`.
pub(crate) fn array_type() -> Ty {
    Ty::array(element_type())
}

fn element_type() -> Ty {
    Ty::obj_args(
        "kotlin/reflect/KProperty",
        &[Ty::nullable(Ty::obj("kotlin/Any"))],
    )
}

/// Each class's array: the expressions building its elements, in index order.
#[derive(Default)]
pub(crate) struct DelegatedPropertyArrays {
    by_owner: HashMap<TypeName, Vec<ExprId>>,
    field_names: HashMap<TypeName, String>,
}

impl DelegatedPropertyArrays {
    pub(super) fn remap_owners(&mut self, names: &HashMap<TypeName, TypeName>) {
        self.by_owner = std::mem::take(&mut self.by_owner)
            .into_iter()
            .map(|(owner, elements)| (names.get(&owner).copied().unwrap_or(owner), elements))
            .collect();
        self.field_names = std::mem::take(&mut self.field_names)
            .into_iter()
            .map(|(owner, field)| (names.get(&owner).copied().unwrap_or(owner), field))
            .collect();
    }

    /// The elements of `owner`'s array, or `None` when it has none.
    pub(crate) fn elements(&self, owner: TypeName) -> Option<&[ExprId]> {
        self.by_owner.get(&owner).map(Vec::as_slice)
    }

    /// Physical field selected for `owner`'s generated array. A source `@JvmField` owns its ABI
    /// spelling, so the generated field takes the first free `$N` suffix in that one collision.
    pub(crate) fn field_name(&self, owner: TypeName) -> &str {
        self.field_names.get(&owner).map_or(FIELD, String::as_str)
    }
}

fn generated_field_name(ir: &IrFile, owner: TypeName) -> String {
    // Fields already owned by the class, plus companion fields with a fixed public ABI, keep their
    // source spelling. An ordinary hoisted companion field remains suffixable and is deliberately
    // left for the later physical-name allocator, so the generated array keeps kotlinc's canonical
    // spelling in that case.
    let occupied = ir
        .statics
        .iter()
        .enumerate()
        .filter(|(index, field)| {
            field.owner == Some(owner)
                && (!ir.is_jvm_companion_hoisted_static(*index as u32)
                    || ir.is_jvm_field_static(*index as u32)
                    || field.is_const)
        })
        .map(|(index, _)| ir.static_field_jvm_name(index as u32))
        .collect::<Vec<_>>();
    if !occupied.contains(&FIELD) {
        return FIELD.to_string();
    }
    (1usize..)
        .map(|suffix| format!("{FIELD}${suffix}"))
        .find(|candidate| !occupied.contains(&candidate.as_str()))
        .expect("an unused delegated-property array suffix always exists")
}

/// One delegated reference operand awaiting its slot.
pub(super) struct DelegatedOperand {
    /// The operand expression, rewritten to read its slot.
    pub(super) operand: ExprId,
    /// The class whose array holds the reference: the class declaring the property's accessors.
    pub(super) owner: TypeName,
    pub(super) property: DelegatedProperty,
    /// The property's position among its class's delegated properties: its declaration's source
    /// order, then, for a local one, its ordinal within the member declaring it.
    pub(super) source_order: (u32, u32),
    /// The reflected property value, built once per property in its class's `<clinit>`.
    pub(super) element: IrExpr,
}

/// A delegated property of a class: a member or top-level one, or a local one by its stable
/// frontend declaration identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum DelegatedProperty {
    Declared(PropertyId),
    Local(LocalDelegatedPropertyId),
}

/// Pass `null` for a delegated reference whose inline operator never reads it, as kotlinc does
/// (`PropertyReferenceLowering.visitCall`): such a property takes no slot.
///
/// A current-module operator common lowering expanded records the operands its body never read.
/// A dependency's inline operator is judged by its bytecode. kotlinc always inlines, so it ignores
/// the operator's own null check of the parameter; here a callee with a legal call fallback may
/// still be called, so only one that must be inlined may null-check the parameter it is given
/// `null` for. Discovering an unread dependency operand makes its inline splice mandatory; the
/// emitter fails closed if it cannot perform that splice. A current-module operator kept as a call
/// can fall back the same way, and keeps its slot.
pub(super) fn elide_unread(ir: &mut IrFile, bodies: &dyn MethodBodies) {
    for argument in compiled_body_unread_operands(ir) {
        if ir.is_delegated_property_operand(argument) {
            ir.mark_unread_inline_operand(argument);
        }
    }
    for raw in 0..ir.exprs.len() as ExprId {
        if ir.is_unread_inline_operand(raw) && ir.is_delegated_property_operand(raw) {
            elide(ir, raw);
        }
    }
    for raw in 0..ir.exprs.len() {
        let IrExpr::Call {
            callee: call_callee,
            dispatch_receiver,
            args,
        } = &ir.exprs[raw]
        else {
            continue;
        };
        let Callee::Static {
            owner,
            name,
            descriptor,
            inline,
        } = &**call_callee
        else {
            continue;
        };
        if !inline.can_inline()
            || !args
                .iter()
                .any(|&arg| ir.is_delegated_property_operand(arg))
        {
            continue;
        }
        let unread = dependency_unread_arguments(
            bodies,
            (&owner.render(), name, descriptor),
            dispatch_receiver.is_some(),
            args,
        );
        for argument in unread {
            if ir.is_delegated_property_operand(argument) {
                ir.mark_unread_inline_operand(argument);
                elide(ir, argument);
            }
        }
    }
}

/// The operands of calls inlined from a current-module compiled body that the body never reads.
/// The route is final, so the body is always inlined and such an operand is never passed. The
/// operands number like the body's values: a member's dispatch receiver first.
fn compiled_body_unread_operands(ir: &IrFile) -> Vec<ExprId> {
    let mut unread = Vec::new();
    for (call, function) in ir.compiled_inline_calls() {
        let Some(body) = ir
            .functions
            .get(function as usize)
            .and_then(|function| function.body)
        else {
            continue;
        };
        let Some(operands) = ir.compiled_inline_operands(call) else {
            continue;
        };
        let read = crate::ir::read_values(ir, body);
        unread.extend(
            operands
                .receiver
                .into_iter()
                .chain(operands.arguments)
                .enumerate()
                .filter(|&(value, _)| !read.contains(&(value as u32)))
                .map(|(_, operand)| operand),
        );
    }
    unread
}

/// Pass `null` in place of a delegated-property operand, adaptations included, so no reflected
/// value is left behind to materialize.
fn elide(ir: &mut IrFile, expression: ExprId) {
    if let IrExpr::TypeOp { arg, .. } = *ir.expr(expression) {
        elide(ir, arg);
    }
    ir.exprs[expression as usize] = IrExpr::Const(IrConst::Null);
}

/// The arguments of a dependency's inline function `owner.name descriptor` its body never reads,
/// null checks aside: inlining removes them, as kotlinc's does. The emitter must then splice the
/// call, since a real call would check the `null` passed instead. An instance method's receiver
/// takes local 0 ahead of the descriptor's parameters.
fn dependency_unread_arguments(
    bodies: &dyn MethodBodies,
    (owner, name, descriptor): (&str, &str, &str),
    instance: bool,
    args: &[ExprId],
) -> Vec<ExprId> {
    let Some(body) = bodies.body(owner, name, descriptor) else {
        return Vec::new();
    };
    let Ok(callee) = MethodNode::read(ACC_STATIC, name, descriptor, &body) else {
        return Vec::new();
    };
    let Some(slots) = crate::jvm::inline::param_offsets(descriptor) else {
        return Vec::new();
    };
    let receiver = u16::from(instance);
    args.iter()
        .zip(slots)
        .filter(|&(_, slot)| !crate::jvm::inliner::reads_local(&callee, slot + receiver))
        .map(|(&arg, _)| arg)
        .collect()
}

/// Give every property one slot in its owner's array, in declaration order, and rewrite each
/// operand to read it. An operand [`elide_unread`] replaced with `null` takes no slot.
pub(super) fn place(ir: &mut IrFile, operands: Vec<DelegatedOperand>) -> DelegatedPropertyArrays {
    let operands: Vec<_> = operands
        .into_iter()
        .filter(|operand| ir.is_delegated_property_operand(operand.operand))
        .collect();
    // Owners in first-operand order, so the element expressions are allocated deterministically;
    // each property once, however many operands read it.
    let mut slots: Vec<(TypeName, Vec<&DelegatedOperand>)> = Vec::new();
    for operand in &operands {
        let position = match slots.iter().position(|(owner, _)| *owner == operand.owner) {
            Some(position) => position,
            None => {
                slots.push((operand.owner, Vec::new()));
                slots.len() - 1
            }
        };
        let owned = &mut slots[position].1;
        if !owned.iter().any(|slot| slot.property == operand.property) {
            owned.push(operand);
        }
    }
    let mut arrays = DelegatedPropertyArrays::default();
    let mut index = HashMap::default();
    let mut allocated = Vec::new();
    for (owner, mut owned) in slots {
        owned.sort_by_key(|operand| operand.source_order);
        let elements = owned
            .iter()
            .enumerate()
            .map(|(slot, operand)| {
                index.insert((owner, operand.property), slot);
                let line = match operand.property {
                    DelegatedProperty::Declared(property) => ir
                        .checked_properties
                        .get(&property)
                        .map_or(0, |declaration| declaration.decl_line),
                    DelegatedProperty::Local(_) => 0,
                };
                (operand.element.clone(), line)
            })
            .collect::<Vec<_>>();
        allocated.push((owner, elements));
    }
    for (owner, elements) in allocated {
        let field_name = generated_field_name(ir, owner);
        // A public source field can displace the generated array from its canonical spelling.
        // kotlinc then retains the declaration line of each delegated property whose reflected
        // value is constructed; a canonical array suppresses the complete `<clinit>` line table.
        let displaced = field_name != FIELD;
        let elements = elements
            .into_iter()
            .map(|(element, line)| {
                let expression = ir.add_expr(element);
                if displaced && line != 0 {
                    ir.expr_source_lines.insert(expression, line);
                }
                expression
            })
            .collect();
        arrays.by_owner.insert(owner, elements);
        arrays.field_names.insert(owner, field_name);
    }
    for operand in operands {
        let slot = index[&(operand.owner, operand.property)];
        let array = ir.add_expr(IrExpr::ExternalStaticField {
            owner: operand.owner,
            name: arrays.field_name(operand.owner).to_string(),
            descriptor: DESCRIPTOR.to_string(),
        });
        let slot = ir.add_expr(IrExpr::Const(IrConst::Int(slot as i32)));
        ir.exprs[operand.operand as usize] = IrExpr::Call {
            callee: Box::new(Callee::Intrinsic {
                operation: IrIntrinsic::ArrayGet,
                ret: element_type(),
            }),
            dispatch_receiver: Some(array),
            args: vec![slot],
        };
    }
    arrays
}

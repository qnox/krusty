//! Objects that live in the frame of the function that creates them.
//!
//! A construction bound to a local `val` that is only ever read for its fields cannot be seen once
//! the function returns: nothing stores it, passes it, returns it or captures it. Such an object
//! is given a stack slot instead of a heap allocation. The collector needs nothing new for it: it
//! scans every frame word by word, so the references the object's fields hold are found the same
//! way a local's are, and the object's own address resolves to no heap chunk.
//!
//! The analysis is one walk over the body and decides only from the IR the lowering already has:
//! which local each construction initializes, and the node that reads each use of that local.

use super::*;
use crate::ir::{ClassId, ExprId, IrCheckedOperation};
use std::collections::{HashMap, HashSet};

/// Where a construction puts its object.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Placement {
    Heap,
    Frame,
}

impl BodyLowering<'_, '_, '_> {
    /// The constructions in `body` whose object may live in this frame.
    pub(super) fn frame_constructions(&self, body: ExprId) -> HashSet<ExprId> {
        let ir = self.file.ir;
        // Each local's declarations, its uses with the node that reads them, and whether anything
        // assigns it.
        let mut declarations: HashMap<u32, Vec<Option<ExprId>>> = HashMap::new();
        let mut uses: HashMap<u32, Vec<(ExprId, ExprId)>> = HashMap::new();
        let mut assigned: HashSet<u32> = HashSet::new();
        let mut pending = vec![body];
        let mut seen = HashSet::new();
        while let Some(parent) = pending.pop() {
            if !seen.insert(parent) {
                continue;
            }
            match ir.expr(parent) {
                IrExpr::Variable { index, init, .. } => {
                    declarations.entry(*index).or_default().push(*init);
                }
                IrExpr::SetValue { var, .. } => {
                    assigned.insert(*var);
                }
                _ => {}
            }
            crate::ir::for_each_child(&ir.exprs, parent, &mut |child| {
                if let IrExpr::GetValue(slot) = ir.expr(child) {
                    uses.entry(*slot).or_default().push((child, parent));
                }
                pending.push(child);
            });
        }
        let mut constructions = HashSet::new();
        for (slot, inits) in declarations {
            let [Some(construction)] = inits[..] else {
                continue;
            };
            let Some(class) = self.frame_class(construction) else {
                continue;
            };
            if assigned.contains(&slot) {
                continue;
            }
            let only_fields_read = uses.get(&slot).is_none_or(|uses| {
                uses.iter()
                    .all(|&(read, reader)| self.reads_a_field(reader, read, class))
            });
            if only_fields_read {
                constructions.insert(construction);
            }
        }
        constructions
    }

    /// The class `construction` builds, when its object could live in a frame: a final class
    /// directly under `Any`, built through its primary constructor with every argument supplied,
    /// whose constructor does nothing but store its parameters. Such a constructor never lets
    /// `this` out, so the object is reachable only through the local it initializes.
    fn frame_class(&self, construction: ExprId) -> Option<ClassId> {
        let IrExpr::New {
            internal,
            ctor_params: None,
            defaults,
            default_prefix_count: 0,
            ..
        } = self.file.ir.expr(construction)
        else {
            return None;
        };
        if !defaults.is_empty() || super::type_checks::is_runtime_constructed(self.file, *internal)
        {
            return None;
        }
        let class = self.file.ir.class_id_by_name(*internal)?;
        let declaration = &self.file.ir.classes[class as usize];
        let plain = !(declaration.is_open
            || declaration.is_abstract
            || declaration.is_sealed
            || declaration.is_interface
            || declaration.is_object
            || declaration.is_enum
            || declaration.is_value
            || declaration.is_inner_class
            || declaration.is_local_class
            || declaration.is_anonymous_object);
        let only_stores_parameters = declaration.init_body.is_none()
            && !declaration.explicit_param_stores
            && declaration.constructor_prefix_count == 0
            && declaration.pre_super_param_fields.is_empty()
            && declaration.super_args.is_empty()
            && super::super::super::intrinsics::is_any(declaration.superclass);
        (plain
            && only_stores_parameters
            && !self.file.values.is_value_class(*internal)
            && self.file.classes[class as usize].constructor.is_some())
        .then_some(class)
    }

    /// Whether `reader` reads one of `class`'s fields out of `read` and nothing else: a field
    /// read, or a property read that the lowering realizes as one.
    fn reads_a_field(&self, reader: ExprId, read: ExprId, class: ClassId) -> bool {
        match self.file.ir.expr(reader) {
            IrExpr::GetField {
                receiver,
                class: owner,
                ..
            } => *receiver == read && *owner == class,
            IrExpr::Checked(IrCheckedOperation::PropertyRead {
                target,
                dispatch_receiver: Some(receiver),
                extension_receiver: None,
                context_arguments,
                ..
            }) => {
                *receiver == read
                    && context_arguments.is_empty()
                    && self.checked_property(target).is_ok_and(|(owner, index)| {
                        owner == class && self.property_field(owner, index).is_some()
                    })
            }
            _ => false,
        }
    }

    /// The field a read of `class`'s property `index` loads directly, when it does: the property
    /// is stored as its own type, reached through no dispatch slot, and has no getter.
    pub(super) fn property_field(&self, class: ClassId, index: usize) -> Option<u32> {
        let property = &self.file.ir.classes[class as usize].properties[index];
        if property
            .storage_ty
            .is_some_and(|storage| self.carrier(storage) != self.carrier(property.ty))
            || property.getter.is_some()
        {
            return None;
        }
        let through_slot = model::local_property_target(self.file.ir, class, index)
            .and_then(|target| self.file.model.slot(class, &model::SlotKey::Getter(target)));
        if through_slot.is_some() {
            return None;
        }
        property.backing_field
    }

    /// A zeroed object of `size` bytes stamped with `descriptor`, in a slot of this frame.
    pub(super) fn frame_object(&mut self, descriptor: DataId, size: u32) -> Value {
        let size = size.next_multiple_of(16);
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            size,
            4,
        ));
        let object = self.builder.ins().stack_addr(types::I64, slot, 0);
        let zero = self.builder.ins().iconst(types::I64, 0);
        for offset in (0..size).step_by(8) {
            self.builder
                .ins()
                .store(trusted(), zero, object, offset as i32);
        }
        let descriptor = self.data_address(descriptor);
        self.builder.ins().store(trusted(), descriptor, object, 0);
        object
    }
}

//! Objects that live in the frame of the function that creates them.
//!
//! A construction bound to a local `val` that is only ever read for its fields cannot be seen once
//! the function returns: nothing stores it, passes it, returns it or captures it. Such an object
//! is never materialized: each of its fields becomes a variable of the function, the construction
//! assigns them, and a read of a field reads its variable. The collector needs nothing new: a
//! reference a field holds is a value of the function like any local's, found the same way.
//!
//! The analysis is one walk over the body and decides only from the IR the lowering already has:
//! which local each construction initializes, and the node that reads each use of that local.

use super::objects::CheckedProperty;
use super::*;
use crate::ir::{ClassId, ExprId, IrCheckedOperation};
use std::collections::{HashMap, HashSet};

/// Where a construction puts its object.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Placement {
    Heap,
    /// In the variables of this frame, as the object `local` holds.
    Frame {
        local: u32,
    },
}

impl BodyLowering<'_, '_, '_> {
    /// The constructions in `body` whose object may live in this frame, each with the local it
    /// initializes.
    pub(super) fn frame_constructions(&self, body: ExprId) -> HashMap<ExprId, u32> {
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
        let mut constructions = HashMap::new();
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
                constructions.insert(construction, slot);
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
                    && matches!(
                        self.checked_property(target),
                        Ok(CheckedProperty::Member(owner, index))
                            if owner == class && self.property_field(owner, index).is_some()
                    )
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

    /// Build a frame object of `class` held by `local`: one variable per field, zero to begin
    /// with, then assigned the constructor's `arguments` (in the physical types `params`) exactly
    /// as its constructor stores them. The local itself holds no object, only null: every use of
    /// it is a field read, and those read the variables.
    pub(super) fn frame_object(
        &mut self,
        class: ClassId,
        local: u32,
        arguments: &[Value],
        params: &[Ty],
    ) -> Result<Option<Value>, Unsupported> {
        // What constructing the class would run first: its companion's creation.
        if let Some(getter) = self.file.ir.classes[class as usize]
            .companion_class
            .and_then(|companion| self.file.ir.class_id_by_name(companion))
            .and_then(|companion| self.file.classes[companion as usize].singleton)
            .map(|(_, getter)| getter)
        {
            let func_ref = self.func_ref(getter);
            self.emit_call(func_ref, &[])?;
        }
        let declaration = &self.file.ir.classes[class as usize];
        let field_count = declaration.fields.len() as u32;
        let stored: Vec<(Option<u32>, bool)> = declaration
            .ctor_args
            .iter()
            .map(|argument| (argument.field_index, argument.is_field))
            .collect();
        let mut fields = Vec::new();
        for field in 0..field_count {
            let ty = model::field_storage_ty(self.file.values, self.file.ir, class, field)?;
            let Some(clif) = self.carrier(ty).clif() else {
                return Err("a `Unit` field".into());
            };
            let variable = self.builder.declare_var(clif);
            let zero = self.zero_of(ty);
            self.builder.def_var(variable, zero);
            fields.push(variable);
        }
        let mut next_field = 0;
        for (index, (field_index, is_field)) in stored.into_iter().enumerate() {
            if !is_field {
                continue;
            }
            let field = field_index.unwrap_or(next_field);
            next_field = field + 1;
            let field_type = model::field_storage_ty(self.file.values, self.file.ir, class, field)?;
            let Some(value) = self.convert(arguments[index], Some(params[index]), field_type)?
            else {
                return Err("a `Unit` field".into());
            };
            self.builder.def_var(fields[field as usize], value);
        }
        self.frame_fields.insert(local, fields);
        Ok(Some(self.builder.ins().iconst(types::I64, 0)))
    }

    /// Field `index` of `class` read out of `receiver` at `ty`, when `receiver` is a local
    /// holding a frame object.
    pub(super) fn frame_field_read(
        &mut self,
        receiver: ExprId,
        class: ClassId,
        index: u32,
        ty: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let IrExpr::GetValue(local) = self.file.ir.expr(receiver) else {
            return Ok(None);
        };
        let Some(fields) = self.frame_fields.get(local) else {
            return Ok(None);
        };
        let value = self.builder.use_var(fields[index as usize]);
        self.field_value(value, class, index, ty).map(Some)
    }
}

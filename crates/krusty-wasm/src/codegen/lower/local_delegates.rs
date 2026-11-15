//! Checked local delegated-property conventions.
//!
//! Common lowering records each accessor as an expression template. A `return` in that template
//! returns from the accessor, so each accessor is its own function. The template's property
//! argument is one object per declaration; the object's field is the property's name. A later
//! call to a member of that `KProperty` is a separate expression.

use krusty::fir::LocalDelegatedPropertyId;
use krusty::fx_hash::HashMap;
use krusty::ir::{IrExpr, IrFile, IrLocalDelegateAccess, IrLocalPropertyReference};

use super::super::super::encode::{
    BlockType, CompositeType, FieldType, Function, Module, StorageType, SubType, ValType,
};
use super::super::super::runtime::Runtime;
use super::{carrier, Body, FileContext, Unsupported};

/// Accessors reserved for one file, and the object each local delegated property is.
pub(super) struct Prepared {
    helpers: HashMap<(u32, bool), u32>,
    property_type: Option<u32>,
    property_objects: HashMap<LocalDelegatedPropertyId, u32>,
}

/// Reserve an accessor function per plan and one global per property object.
pub(super) fn prepare(ir: &IrFile, module: &mut Module, runtime: &Runtime) -> Prepared {
    let mut property_objects = HashMap::default();
    let mut property_type = None;
    for expression in &ir.exprs {
        let IrExpr::LocalPropertyReference(reference) = expression else {
            continue;
        };
        let struct_type = *property_type.get_or_insert_with(|| {
            module.subtype(SubType {
                composite: CompositeType::Struct(vec![FieldType {
                    storage: StorageType::Val(runtime.string_type()),
                    mutable: false,
                }]),
                supertype: None,
                open: false,
            })
        });
        property_objects
            .entry(reference.declaration)
            .or_insert_with(|| module.global(ValType::nullable(struct_type)));
    }
    let mut helpers = HashMap::default();
    for (plan, declaration) in ir.local_delegate_plans.iter().enumerate() {
        let plan = plan as u32;
        helpers.insert((plan, false), module.declare());
        if declaration.setter.is_some() {
            helpers.insert((plan, true), module.declare());
        }
    }
    Prepared {
        helpers,
        property_type,
        property_objects,
    }
}

/// Define every accessor reserved by [`prepare`].
pub(super) fn define(
    context: &FileContext<'_>,
    module: &mut Module,
    runtime: &mut Runtime,
) -> Result<(), Unsupported> {
    for plan in 0..context.ir.local_delegate_plans.len() {
        let declaration = &context.ir.local_delegate_plans[plan];
        let mut accessors = vec![(false, declaration.getter.clone())];
        if let Some(setter) = declaration.setter.clone() {
            accessors.push((true, setter));
        }
        for (setter, accessor) in accessors {
            let index = context.delegates.helpers[&(plan as u32, setter)];
            let params = accessor
                .parameters
                .iter()
                .map(|ty| carrier(*ty)?.ok_or_else(|| "a `Unit` parameter".to_string()))
                .collect::<Result<Vec<_>, Unsupported>>()?;
            let results = carrier(accessor.result)?.into_iter().collect::<Vec<_>>();
            let signature = module.func_type(params, results);
            let declared = context.ir.declared_value_slot_types(accessor.body);
            let mut body = Body::new(
                context,
                module,
                runtime,
                &accessor.parameters,
                declared,
                accessor.result,
            );
            body.function_body(accessor.body, accessor.result)?;
            let Body { code, locals, .. } = body;
            module.define(
                index,
                Function {
                    ty: signature,
                    locals,
                    code,
                },
            );
        }
    }
    Ok(())
}

impl Body<'_> {
    pub(super) fn local_delegate_access(
        &mut self,
        access: IrLocalDelegateAccess,
    ) -> Result<(), Unsupported> {
        let setter = access.value.is_some();
        let parameters = {
            let plan = self
                .ir
                .local_delegate_plans
                .get(access.plan as usize)
                .ok_or_else(|| {
                    "a local delegated-property access without its checked plan".to_string()
                })?;
            let accessor = if setter {
                plan.setter.as_ref().ok_or_else(|| {
                    "a local delegated-property write without its setter".to_string()
                })?
            } else {
                &plan.getter
            };
            accessor.parameters.clone()
        };
        let function = *self
            .delegates
            .helpers
            .get(&(access.plan, setter))
            .ok_or_else(|| {
                "a local delegated-property accessor without its function".to_string()
            })?;
        let mut arguments = Vec::new();
        if let Some(receiver) = access.dispatch_receiver {
            arguments.push(receiver);
        }
        arguments.push(access.delegate);
        if let Some(value) = access.value {
            arguments.push(value);
        }
        if arguments.len() != parameters.len() {
            return Err(
                "a local delegated-property access whose operands do not match its accessor".into(),
            );
        }
        for (argument, ty) in arguments.into_iter().zip(parameters) {
            self.value(argument, ty)?;
        }
        self.code.call(function);
        Ok(())
    }

    pub(super) fn local_property_reference(
        &mut self,
        reference: &IrLocalPropertyReference,
    ) -> Result<(), Unsupported> {
        let global = *self
            .delegates
            .property_objects
            .get(&reference.declaration)
            .ok_or_else(|| "a local property reference without its object".to_string())?;
        let struct_type = self
            .delegates
            .property_type
            .ok_or_else(|| "a local property reference without its type".to_string())?;
        let units = reference.name.encode_utf16().collect::<Vec<_>>();
        self.code
            .global_get(global)
            .ref_is_null()
            .if_(BlockType::Empty);
        self.runtime
            .string_constant(self.module, &mut self.code, &units);
        self.code
            .struct_new(struct_type)
            .global_set(global)
            .end()
            .global_get(global);
        Ok(())
    }
}

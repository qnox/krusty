//! Checked local delegated-property conventions.
//!
//! Common lowering records each accessor as an expression template. A `return` in that template
//! returns from the accessor, so each accessor is its own function. The template's property
//! argument is one object per declaration; the object's field is the property's name. A later
//! call to a member of that `KProperty` is a separate expression.

use krusty::fir::LocalDelegatedPropertyId;
use krusty::fx_hash::HashMap;
use krusty::ir::{IrExpr, IrFile, IrLocalDelegateAccess, IrLocalPropertyReference, PropertyId};

use super::super::super::encode::{BlockType, CompositeType, Function, Module, SubType, ValType};
use super::super::super::runtime::Runtime;
use super::{carrier, Body, FileContext, Unsupported};

/// Accessors reserved for one file, and the object each local delegated property is.
pub(super) struct Prepared {
    helpers: HashMap<(u32, bool), u32>,
    property_type: Option<u32>,
    /// The table global of the property objects' class, once a use has declared it.
    property_table: std::cell::Cell<Option<u32>>,
    property_objects: HashMap<LocalDelegatedPropertyId, u32>,
    /// One object per delegated member property, the value `getValue` receives.
    delegated_properties: HashMap<PropertyId, u32>,
}

/// Reserve an accessor function per plan and one global per property object.
pub(super) fn prepare(ir: &IrFile, module: &mut Module, runtime: &Runtime) -> Prepared {
    let mut property_objects = HashMap::default();
    let mut delegated_properties = HashMap::default();
    let mut property_type = None;
    let mut property_struct = |module: &mut Module| {
        *property_type.get_or_insert_with(|| {
            module.subtype(SubType {
                composite: CompositeType::Struct(vec![
                    super::super::super::objects::immutable(ValType::reference(
                        runtime.objects.any_table,
                    )),
                    super::super::super::objects::immutable(ValType::I32),
                    super::super::super::objects::immutable(runtime.string_type()),
                ]),
                supertype: Some(runtime.objects.identified),
                open: false,
            })
        })
    };
    for expression in &ir.exprs {
        let IrExpr::LocalPropertyReference(reference) = expression else {
            continue;
        };
        let struct_type = property_struct(module);
        property_objects
            .entry(reference.declaration)
            .or_insert_with(|| module.global(ValType::nullable(struct_type)));
    }
    for expression in &ir.exprs {
        let IrExpr::Checked(operation) = expression else {
            continue;
        };
        let Some(reference) = operation.module_property_reference() else {
            continue;
        };
        if !reference.delegated {
            continue;
        }
        let struct_type = property_struct(module);
        delegated_properties
            .entry(reference.property)
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
        property_table: std::cell::Cell::new(None),
        property_objects,
        delegated_properties,
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
                .map(|ty| super::parameter_carrier_of(*ty))
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
            self.argument(argument, ty)?;
        }
        self.code.call(function);
        Ok(())
    }

    pub(super) fn delegated_property_reference(
        &mut self,
        property: PropertyId,
    ) -> Result<(), Unsupported> {
        let name = self
            .ir
            .checked_properties
            .get(&property)
            .map(|property| property.name.as_str())
            .ok_or_else(|| "a delegated property reference without its declaration".to_string())?;
        let global = *self
            .delegates
            .delegated_properties
            .get(&property)
            .ok_or_else(|| "a delegated property reference without its object".to_string())?;
        let struct_type = self
            .delegates
            .property_type
            .ok_or_else(|| "a delegated property reference without its type".to_string())?;
        self.property_reference_object(name, global, struct_type)
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
        self.property_reference_object(&reference.name, global, struct_type)
    }

    /// The class of this file's property objects, declared on first use. Its instances are
    /// `KProperty`s whose `name` is their own field; they compare and hash by identity.
    fn property_object_table(&mut self, struct_type: u32) -> Result<u32, Unsupported> {
        if let Some(table) = self.delegates.property_table.get() {
            return Ok(table);
        }
        let objects = &mut self.runtime.objects;
        let [_, _, to_string_ty] = objects.any_slot_types;
        let (class, itable) =
            objects
                .interfaces
                .class(self.module, objects.itable, vec![struct_type]);
        for interface in [
            krusty::types::type_name("kotlin/reflect/KProperty"),
            krusty::types::type_name("kotlin/reflect/KCallable"),
        ] {
            objects.interfaces.instance_of(class, interface);
        }
        let equals = objects.identity_equals;
        let to_string = objects.trap(self.module, to_string_ty);
        let hash_code = self.runtime.identity.hash_code;
        let mut init = super::super::super::encode::Code::default();
        for function in [equals, hash_code, to_string] {
            self.module.reference_function(function);
            init.ref_func(function);
        }
        init.global_get(itable);
        self.runtime.objects.push_type_info(&mut init, None);
        init.struct_new(self.runtime.objects.any_table);
        let table = self
            .module
            .constant_global(ValType::reference(self.runtime.objects.any_table), init);
        self.runtime
            .reference_name_field(self.module, class, struct_type, NAME_FIELD)?;
        self.delegates.property_table.set(Some(table));
        Ok(table)
    }

    fn property_reference_object(
        &mut self,
        name: &str,
        global: u32,
        struct_type: u32,
    ) -> Result<(), Unsupported> {
        let table = self.property_object_table(struct_type)?;
        let units = name.encode_utf16().collect::<Vec<_>>();
        self.code
            .global_get(global)
            .ref_is_null()
            .if_(BlockType::Empty)
            .global_get(table)
            .call(self.runtime.identity.allocate);
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

/// The field of a property object holding its name, after the table and the identity hash.
const NAME_FIELD: u32 = 2;

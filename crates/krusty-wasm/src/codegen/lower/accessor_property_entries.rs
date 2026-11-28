//! The defining file's side of a property another file reaches through accessor entries, when
//! the entry forwards to the property's accessors.
//!
//! A computed or delegated package property, an extension or context package property, or a
//! property of a `companion { }` block has no global another file could read. A member extension
//! property, or a member property with context parameters, has no storage either. Its defining
//! file realizes it through its accessor functions, and these entries forward their parameters (a
//! member's receiver, an extension or context property's declared operands, then a setter's
//! value) to them under the indices the module's files share for the property.

use krusty::backend::class_tables::SlotKey;
use krusty::backend::receiver_property_accessors::{self, ReceiverAccessors};
use krusty::fir::ResolvedPropertyOverrideTarget;
use krusty::ir::IrLocalPropertyLayout;

use super::super::super::encode::{Code, Function, Module};
use super::super::super::objects::REFERENCE;
use super::super::super::runtime::Runtime;
use super::super::classes::FileClasses;
use super::super::property_registry::{AccessorPropertyDefinition, MemberPropertyDefinition};
use super::{carrier, parameter_carrier_of, FileContext, Unsupported};

/// Define the program-wide entries reserved for this file's receiver-less accessor properties.
pub(super) fn define(
    context: &FileContext<'_>,
    definitions: &[AccessorPropertyDefinition],
    module: &mut Module,
) -> Result<(), Unsupported> {
    for definition in definitions {
        let layout = context
            .ir
            .local_property_layouts
            .get(&definition.property)
            .ok_or_else(|| "an accessor property entry with no local layout".to_string())?;
        let (getter, setter) = match layout {
            IrLocalPropertyLayout::TopLevelAccessor { getter, setter, .. } => {
                (Some(*getter), *setter)
            }
            IrLocalPropertyLayout::TopLevelStorage { getter, setter, .. } => (*getter, *setter),
            _ => return Err("an accessor property entry with a receiver layout".to_string()),
        };
        let entry = &definition.entry;
        let value = carrier(entry.ty)?;
        let operands = entry
            .parameters
            .iter()
            .map(|parameter| parameter_carrier_of(*parameter))
            .collect::<Result<Vec<_>, _>>()?;
        let getter =
            getter.ok_or_else(|| "an accessor property entry with no getter".to_string())?;
        check_accessor(context, getter, &entry.parameters, None, Some(entry.ty))?;
        let getter = accessor(context, getter)?;
        let mut code = Code::default();
        forward(&mut code, operands.len());
        code.call(getter);
        let ty = module.func_type(operands.clone(), value.into_iter().collect());
        module.define(
            entry.getter,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
        let Some(index) = entry.setter else {
            continue;
        };
        let value =
            value.ok_or_else(|| "a mutable accessor property with no Wasm value".to_string())?;
        let setter =
            setter.ok_or_else(|| "a mutable accessor property entry with no setter".to_string())?;
        check_accessor(context, setter, &entry.parameters, Some(entry.ty), None)?;
        let setter = accessor(context, setter)?;
        let mut code = Code::default();
        forward(&mut code, operands.len() + 1);
        code.call(setter);
        let mut parameters = operands;
        parameters.push(value);
        let ty = module.func_type(parameters, Vec::new());
        module.define(
            index,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }
    Ok(())
}

/// Define the entries of a member extension property, or a member property with context
/// parameters: each forwards its receiver, its declared operands and a setter's value to the
/// accessor, through the owner's dispatch slot when the accessor is overridable.
pub(super) fn define_receiver_member(
    context: &FileContext<'_>,
    classes: &FileClasses,
    definition: &MemberPropertyDefinition,
    layout: &IrLocalPropertyLayout,
    module: &mut Module,
    runtime: &mut Runtime,
) -> Result<(), Unsupported> {
    let entry = &definition.entry;
    let accessors = ReceiverAccessors::of_layout(layout)?;
    if accessors.owner != Some(entry.owner)
        || entry.setter.is_some() != accessors.setter.is_some()
        || *receiver_property_accessors::layout_parameters(layout) != *entry.parameters
    {
        return Err("a module member property entry disagrees with its local layout".into());
    }
    let class = context
        .ir
        .class_id_by_name(entry.owner)
        .ok_or("a module member property of a class this file does not lay out")?;
    let target = ResolvedPropertyOverrideTarget::Module(definition.property);
    let mut operands = vec![REFERENCE];
    for parameter in entry.parameters.iter() {
        operands.push(parameter_carrier_of(*parameter)?);
    }
    let value = carrier(entry.ty)?;
    let roles = std::iter::once((accessors.getter, entry.getter, false)).chain(
        accessors
            .setter
            .zip(entry.setter)
            .map(|(function, index)| (function, index, true)),
    );
    for (function, index, setter) in roles {
        check_accessor(
            context,
            function,
            &entry.parameters,
            setter.then_some(entry.ty),
            None,
        )?;
        let mut parameters = operands.clone();
        let results = if setter {
            parameters.push(value.ok_or_else(|| {
                "a mutable module member property with no Wasm value".to_string()
            })?);
            Vec::new()
        } else {
            value.into_iter().collect()
        };
        let mut code = Code::default();
        forward(&mut code, parameters.len());
        let key = if setter {
            SlotKey::Setter(target)
        } else {
            SlotKey::Getter(target)
        };
        let slot = (!context.ir.classes[class as usize].is_value)
            .then(|| classes.tables.slot(class, &key))
            .flatten();
        match slot {
            Some(slot) => super::objects::emit_module_property_dispatch(
                classes, runtime, class, slot, &mut code,
            )?,
            None => {
                let emitted = context
                    .functions
                    .get(function as usize)
                    .copied()
                    .flatten()
                    .ok_or("a module property accessor with no emitted function")?;
                code.call(emitted);
            }
        }
        let ty = module.func_type(parameters, results);
        module.define(
            index,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }
    Ok(())
}

/// Push an entry's first `count` parameters, in order, as the accessor's arguments.
pub(super) fn forward(code: &mut Code, count: usize) {
    for parameter in 0..count {
        code.local_get(parameter as u32);
    }
}

/// Check that `function`, an accessor an entry forwards to, declares the entry's operands: the
/// declared `parameters`, then a setter's `value`, and a getter's `result`. Both files derived the
/// entry from one declaration header, so a disagreement is an internal error.
pub(super) fn check_accessor(
    context: &FileContext<'_>,
    function: krusty::ir::FunId,
    parameters: &[krusty::types::Ty],
    value: Option<krusty::types::Ty>,
    result: Option<krusty::types::Ty>,
) -> Result<(), Unsupported> {
    if parameters.is_empty() {
        // A receiver-less property's entry carries its declared type, which its accessors realize
        // as the same carrier.
        return Ok(());
    }
    let declared = &context.ir.functions[function as usize];
    let expected = parameters.iter().copied().chain(value);
    if !declared.params.iter().copied().eq(expected)
        || result.is_some_and(|result| declared.ret != result)
    {
        return Err("an accessor property entry disagrees with its accessor".to_string());
    }
    Ok(())
}

/// The emitted Wasm function of an accessor this file declares.
fn accessor(context: &FileContext<'_>, function: krusty::ir::FunId) -> Result<u32, Unsupported> {
    context
        .functions
        .get(function as usize)
        .copied()
        .flatten()
        .ok_or_else(|| "an accessor property entry whose accessor was not emitted".to_string())
}

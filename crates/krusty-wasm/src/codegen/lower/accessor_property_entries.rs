//! The defining file's side of a receiver-less property another file reaches through accessors.
//!
//! A computed or delegated package property, or a property of a `companion { }` block, has no
//! global another file could read. Its defining file realizes it through its accessor functions,
//! and these entries forward to them under the indices the module's files share for the property.

use krusty::ir::IrLocalPropertyLayout;

use super::super::super::encode::{Code, Function, Module};
use super::super::property_registry::AccessorPropertyDefinition;
use super::{carrier, FileContext, Unsupported};

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
        let entry = definition.entry;
        let value = carrier(entry.ty)?;
        let getter = getter
            .ok_or_else(|| "an accessor property entry with no getter".to_string())
            .and_then(|getter| accessor(context, getter))?;
        let mut code = Code::default();
        code.call(getter);
        let ty = module.func_type(Vec::new(), value.into_iter().collect());
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
        let setter = setter
            .ok_or_else(|| "a mutable accessor property entry with no setter".to_string())
            .and_then(|setter| accessor(context, setter))?;
        let mut code = Code::default();
        code.local_get(0).call(setter);
        let ty = module.func_type(vec![value], Vec::new());
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

/// The emitted Wasm function of an accessor this file declares.
fn accessor(context: &FileContext<'_>, function: krusty::ir::FunId) -> Result<u32, Unsupported> {
    context
        .functions
        .get(function as usize)
        .copied()
        .flatten()
        .ok_or_else(|| "an accessor property entry whose accessor was not emitted".to_string())
}

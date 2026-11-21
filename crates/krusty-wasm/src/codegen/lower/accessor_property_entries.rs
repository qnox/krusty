//! The defining file's side of a receiver-less property another file reaches through accessors.
//!
//! A computed or delegated package property, or a property of a `companion { }` block, has no
//! global another file could read. Its defining file realizes it through its accessor functions,
//! and these entries forward to them under the indices the module's files share for the property.

use krusty::ir::IrLocalPropertyLayout;

use super::super::super::encode::{Code, Function, Module};
use super::super::super::runtime::Runtime;
use super::super::property_registry::AccessorPropertyDefinition;
use super::{carrier, Body, FileContext, Unsupported};

/// Define the program-wide entries reserved for this file's receiver-less accessor properties.
pub(super) fn define(
    context: &FileContext<'_>,
    definitions: &[AccessorPropertyDefinition],
    module: &mut Module,
    runtime: &mut Runtime,
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
        if let IrLocalPropertyLayout::TopLevelStorage {
            storage,
            getter: None,
            ..
        } = layout
        {
            storage_entries(context, *storage, entry, module, runtime)?;
            continue;
        }
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

/// The entries of a private block property's storage: its class is initialized first, as a read
/// or write of the storage inside the class does.
fn storage_entries(
    context: &FileContext<'_>,
    storage: u32,
    entry: super::super::property_registry::AccessorProperty,
    module: &mut Module,
    runtime: &mut Runtime,
) -> Result<(), Unsupported> {
    let value = carrier(entry.ty)?;
    let global = context.globals[storage as usize];
    let mut body = Body::new(context, module, runtime, &[], Default::default(), entry.ty);
    body.initialize_static_owner(storage)?;
    body.code.global_get(global);
    let Body { code, locals, .. } = body;
    let ty = module.func_type(Vec::new(), value.into_iter().collect());
    module.define(entry.getter, Function { ty, locals, code });
    let Some(setter) = entry.setter else {
        return Ok(());
    };
    let value =
        value.ok_or_else(|| "a mutable accessor property with no Wasm value".to_string())?;
    let mut body = Body::new(
        context,
        module,
        runtime,
        &[entry.ty],
        Default::default(),
        krusty::types::Ty::Unit,
    );
    body.initialize_static_owner(storage)?;
    body.code.local_get(0).global_set(global);
    let Body { code, locals, .. } = body;
    let ty = module.func_type(vec![value], Vec::new());
    module.define(setter, Function { ty, locals, code });
    Ok(())
}

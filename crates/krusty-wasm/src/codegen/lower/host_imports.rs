//! A bodyless dependency function the module imports from its host (`IrFile::host_imports`).

use krusty::ir::{FunId, IrFile};
use krusty::types::Ty;

use super::super::super::encode::{FunctionImport, Module, ValType};
use super::{carrier, Unsupported};

/// Supply `function` (reserved as `index`, of type `ty`) from the host function `import`. Only
/// numbers cross into a host the module shares no objects with.
pub(super) fn host_import(
    module: &mut Module,
    ir: &IrFile,
    function: FunId,
    index: u32,
    ty: u32,
    import: &krusty::ir::IrHostImport,
) -> Result<(), Unsupported> {
    let declaration = &ir.functions[function as usize];
    let number = |carried: Result<Option<ValType>, Unsupported>| {
        matches!(
            carried,
            Ok(Some(
                ValType::I32 | ValType::I64 | ValType::F32 | ValType::F64
            ))
        )
    };
    if !declaration.params.iter().all(|ty| number(carrier(*ty)))
        || !(declaration.ret == Ty::Unit || number(carrier(declaration.ret)))
    {
        return Err(format!(
            "the host function `{}.{}` with a parameter or result that is not a number",
            import.module, import.name
        ));
    }
    module.define_import(
        index,
        FunctionImport {
            module: import.module.clone(),
            name: import.name.clone(),
            ty,
        },
    );
    Ok(())
}

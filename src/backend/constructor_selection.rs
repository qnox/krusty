//! Which of the constructors of a class this file declares a construction runs.

use crate::ir::{ClassId, ExprId, IrFile};
use crate::types::Ty;

/// The constructor of `class` the construction at `site` runs: 0 for the primary, `n` for the
/// `n`th secondary constructor, or `None` for one the class does not declare.
///
/// The checker's exact selection decides whenever it was recorded. A parameter list is not an
/// identity: a default-argument entry takes its constructor's parameters and then mask words, a
/// list a declared constructor may also have. Only a construction a compiler pass built without a
/// selection is matched by the list it names, `selected`, and one naming none runs the primary.
pub(crate) fn selected_constructor(
    ir: &IrFile,
    site: ExprId,
    class: ClassId,
    selected: Option<&[Ty]>,
) -> Option<u32> {
    let declaration = &ir.classes[class as usize];
    if let Some(target) = ir.construction_targets.get(&site) {
        let declared = match target.ordinal {
            0 => declaration.has_primary_ctor,
            ordinal => declaration.secondary_ctors.len() >= ordinal as usize,
        };
        return declared.then_some(target.ordinal);
    }
    let Some(selected) = selected else {
        return Some(0);
    };
    // Either spelling of a secondary: its own parameters, or those behind the prefix every
    // constructor of an inner or local class leads with.
    let secondary = declaration.secondary_ctors.iter().position(|candidate| {
        candidate.params == selected
            || (!candidate.prefix_params.is_empty()
                && candidate.prefix_params.len() + candidate.params.len() == selected.len()
                && selected.starts_with(&candidate.prefix_params)
                && selected.ends_with(&candidate.params))
    });
    match secondary {
        Some(index) => Some(u32::try_from(index + 1).expect("too many constructors")),
        None => declaration
            .ctor_args
            .iter()
            .map(|argument| argument.ty)
            .eq(selected.iter().copied())
            .then_some(0),
    }
}

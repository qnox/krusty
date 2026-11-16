//! Current-file initializer declarations awaiting a target-owned physical acknowledgement.

use super::{CheckedIrFile, KlibDependencyCallable};
use crate::libraries::KlibClassIdentity;
use crate::types::Ty;

/// An exact source declaration and its declared initializer frame. Parameters belong only to
/// the current checked file; an acknowledgement persists canonical identities, never these types.
#[derive(Clone, Debug)]
pub struct ConstructorInitializerDeclaration {
    pub constructor: KlibDependencyCallable,
    pub class: KlibClassIdentity,
    pub parameters: Box<[Ty]>,
}

pub(crate) fn initializer_declarations(
    file: &CheckedIrFile<'_>,
) -> Result<Vec<ConstructorInitializerDeclaration>, &'static str> {
    let mut requests = Vec::new();
    for (constructor, identity) in file.dependency_exports.constructor_declarations() {
        let class = file
            .ir
            .classes
            .get(constructor.class as usize)
            .ok_or("a source initializer lost its declared class")?;
        if !class.is_abstract && !class.is_sealed {
            continue;
        }
        let owner = file
            .dependency_exports
            .classifier(constructor.class)
            .ok_or("a source initializer lost its canonical class identity")?;
        if owner.module != identity.module {
            return Err("a source initializer and its class belong to different modules");
        }
        let parameters = match constructor.secondary {
            Some(ordinal) => {
                let secondary = class
                    .secondary_ctors
                    .get(ordinal)
                    .ok_or("a source initializer lost its declared constructor ordinal")?;
                secondary
                    .prefix_params
                    .iter()
                    .chain(&secondary.params)
                    .copied()
                    .collect()
            }
            None if class.has_primary_ctor => {
                class.ctor_args.iter().map(|argument| argument.ty).collect()
            }
            None => return Err("a source initializer names a missing primary constructor"),
        };
        requests.push(ConstructorInitializerDeclaration {
            constructor: identity.clone(),
            class: owner.clone(),
            parameters,
        });
    }
    Ok(requests)
}

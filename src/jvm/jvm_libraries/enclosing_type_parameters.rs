//! The enclosing-class type parameters a nested class's metadata references by bare id.
//!
//! kotlinc writes a reference to an ENCLOSING class's type parameter as `Type.type_parameter`
//! (f7) alone, where the id counts every enclosing class's own parameters outermost-first before
//! the class's own. The per-classfile metadata reader records such a reference as a placeholder;
//! rebinding it needs the enclosing classes' metadata, reachable only through the classpath at
//! this provider boundary.

use crate::jvm::metadata;
use crate::types::{type_name, Ty, TypeName};

/// The own type parameters (declaration identities with their declared bounds) of every class
/// enclosing `internal`, outermost first — the id space a nested class's `Type.type_parameter`
/// (f7) counts into.
pub(super) fn enclosing_type_parameters(
    cp: &crate::jvm::classpath::Classpath,
    internal: TypeName,
) -> Vec<(String, Vec<Ty>)> {
    // The structural owner chain, from the classfile `InnerClasses` entries — never from `$`
    // boundaries in the name, which a literal `$` in a source spelling would misread.
    let mut owners = Vec::new();
    let mut nested = internal;
    loop {
        let Some(class) = cp.find_name(nested) else {
            // Without one structural link the joint metadata id space is unknowable. Keep the
            // decoded placeholders unresolved rather than shifting them onto a different owner.
            return Vec::new();
        };
        let Some(owner) = class
            .inner_class_self()
            .and_then(|entry| entry.outer.as_deref())
            .map(type_name)
        else {
            break;
        };
        owners.push(owner);
        nested = owner;
    }
    owners.reverse();
    let mut collected = Vec::new();
    for owner in owners {
        let class = cp
            .find_name(owner)
            .expect("the structural enclosing-class walk validated every owner");
        let parameters = &class.meta.class_type_parameters;
        // The metadata decode already folded each parameter's declaring classifier and ordinal
        // into its identity; a bound may still reference an OUTER level's parameter by
        // placeholder (`class Outer<E> { inner class Inner<T : E> }`), which the levels collected
        // so far rebind.
        let level = parameters
            .type_params()
            .iter()
            .zip(parameters.type_param_bounds())
            .map(|(identity, bounds)| {
                let bounds = bounds
                    .iter()
                    .map(|bound| metadata::rebind_enclosing_type_parameters(*bound, &collected))
                    .collect();
                (identity.clone(), bounds)
            })
            .collect::<Vec<_>>();
        collected.extend(level);
    }
    collected
}

//! Each provider numbers the properties it declares itself, so one dependency's accessor names
//! its property differently in each module of a program. Support for the whole program lowers
//! with one provider, so every selected accessor is renumbered into that provider's numbering.

use crate::fx_hash::HashMap;
use crate::libraries::{
    KlibBodyAccessor, KlibBodyCallable, KlibDependencyCallable, SemanticPlatform,
};

/// The accessor facets `platform` declares for the accessors among `selected`.
pub(super) fn program_accessors(
    platform: &dyn SemanticPlatform,
    selected: &[KlibBodyCallable<'_>],
) -> Result<HashMap<KlibDependencyCallable, KlibBodyAccessor>, &'static str> {
    let mut accessors = HashMap::default();
    for callable in selected {
        if callable.accessor().is_none() || accessors.contains_key(callable.identity()) {
            continue;
        }
        let (_, realization) = platform
            .external_callable_by_dependency(callable.identity())
            .ok_or("a selected dependency accessor is absent from the program's provider")?;
        let accessor = realization
            .klib_body_accessor
            .ok_or("the program's provider declares a selected accessor as another callable")?;
        accessors.insert(callable.identity().clone(), accessor);
    }
    Ok(accessors)
}

/// `callable` in the numbering of `accessors`, the program provider's accessor facets.
pub(super) fn program_numbered<'b>(
    callable: KlibBodyCallable<'b>,
    accessors: &'b HashMap<KlibDependencyCallable, KlibBodyAccessor>,
) -> Result<KlibBodyCallable<'b>, &'static str> {
    let Some(own) = callable.accessor() else {
        return Ok(callable);
    };
    let accessor = accessors
        .get(callable.identity())
        .ok_or("a dependency accessor root is not among the program's selected accessors")?;
    if accessor.role != own.role {
        return Err("the program's provider declares another accessor of a selected property");
    }
    Ok(callable.with_accessor(accessor))
}

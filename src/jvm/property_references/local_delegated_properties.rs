//! The local delegated properties Kotlin metadata lists for each class and file facade.
//!
//! kotlinc records every local delegated property of a class (a top-level declaration's, of its
//! file facade) in `<v#N>` order, the order their reflected references are numbered in. Each has
//! at least its getter's reference, so the references common IR carries name them all.

use std::collections::HashMap;

use crate::ir::{IrExpr, IrFile};
use crate::metadata::local_properties::LocalPropertyMeta;
use crate::types::TypeName;

/// Each container's local delegated properties, in `<v#N>` order.
#[derive(Default)]
pub(crate) struct LocalDelegatedProperties {
    by_container: HashMap<TypeName, Vec<LocalPropertyMeta>>,
}

impl LocalDelegatedProperties {
    /// The local delegated properties `ir` references, by the class or facade declaring them, or
    /// `None` when a top-level one's source has no facade among `stems`.
    pub(crate) fn collect(ir: &IrFile, stems: &[String]) -> Option<Self> {
        let mut numbered: HashMap<TypeName, HashMap<u32, LocalPropertyMeta>> = HashMap::new();
        for expression in &ir.exprs {
            let IrExpr::LocalPropertyReference(reference) = expression else {
                continue;
            };
            let container = match reference.class {
                Some(class) => class,
                None => super::super::module_calls::facade_for(reference.source, stems)?,
            };
            numbered
                .entry(container)
                .or_default()
                .entry(reference.ordinal)
                .or_insert_with(|| LocalPropertyMeta {
                    name: reference.name.to_string(),
                    ty: reference.property_type,
                    mutable: reference.mutable,
                });
        }
        let by_container = numbered
            .into_iter()
            .map(|(container, properties)| {
                let mut properties = properties.into_iter().collect::<Vec<_>>();
                properties.sort_by_key(|&(ordinal, _)| ordinal);
                let properties = properties.into_iter().map(|(_, property)| property);
                (container, properties.collect())
            })
            .collect();
        Some(Self { by_container })
    }

    /// `container`'s local delegated properties, in `<v#N>` order.
    ///
    /// kotlinc gives a property whose type mentions a type parameter its own copies of the
    /// enclosing declarations' type parameters, which this list does not record yet. Reflection
    /// resolves `<v#N>` by position, so such a container lists none rather than a shifted list.
    pub(crate) fn of(&self, container: TypeName) -> &[LocalPropertyMeta] {
        match self.by_container.get(&container) {
            Some(properties) if !properties.iter().any(|p| p.ty.mentions_ty_param()) => properties,
            _ => &[],
        }
    }
}

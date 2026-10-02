//! The local delegated properties Kotlin metadata lists for each class and file facade.
//!
//! kotlinc records every local delegated property of a class (a top-level declaration's, of its
//! file facade) in `<v#N>` order, the order their reflected references are numbered in. Each has
//! at least its getter's reference, so the references common IR carries name them all.

use std::collections::HashMap;

use crate::fir::LocalDelegatedPropertyId;
use crate::ir::{IrExpr, IrFile};
use crate::metadata::local_properties::{LocalPropertyMeta, LocalPropertyTypeParameter};
use crate::types::{Ty, TypeName};

/// Each container's local delegated properties, in `<v#N>` order.
#[derive(Default)]
pub(crate) struct LocalDelegatedProperties {
    by_container: HashMap<TypeName, Vec<LocalPropertyMeta>>,
}

impl LocalDelegatedProperties {
    pub(super) fn remap_owners(&mut self, names: &HashMap<TypeName, TypeName>) {
        self.by_container = std::mem::take(&mut self.by_container)
            .into_iter()
            .map(|(owner, properties)| (names.get(&owner).copied().unwrap_or(owner), properties))
            .collect();
    }

    /// The local delegated properties `ir` references, by the class or facade declaring them, or
    /// `None` when a top-level one's source has no facade among `stems`.
    pub(crate) fn collect(ir: &IrFile, stems: &[String]) -> Option<Self> {
        let mut numbered: HashMap<
            TypeName,
            HashMap<LocalDelegatedPropertyId, (u32, LocalPropertyMeta)>,
        > = HashMap::new();
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
                .entry(reference.declaration)
                .or_insert_with(|| {
                    (
                        reference.ordinal,
                        LocalPropertyMeta {
                            name: reference.name.to_string(),
                            ty: reference.property_type,
                            mutable: reference.mutable,
                            type_parameters: local_type_parameters(reference.property_type),
                        },
                    )
                });
        }
        let by_container = numbered
            .into_iter()
            .map(|(container, properties)| {
                let mut properties = properties.into_iter().collect::<Vec<_>>();
                properties.sort_by_key(|&(_, (ordinal, _))| ordinal);
                let properties = properties.into_iter().map(|(_, (_, property))| property);
                (container, properties.collect())
            })
            .collect();
        Some(Self { by_container })
    }

    /// `container`'s local delegated properties, in `<v#N>` order.
    ///
    pub(crate) fn of(&self, container: TypeName) -> &[LocalPropertyMeta] {
        self.by_container.get(&container).map_or(&[], Vec::as_slice)
    }
}

fn local_type_parameters(ty: Ty) -> Vec<LocalPropertyTypeParameter> {
    fn collect(ty: Ty, out: &mut Vec<LocalPropertyTypeParameter>) {
        match ty {
            Ty::TyParam(name, bound) => {
                if !out.iter().any(|parameter| parameter.semantic_name == name) {
                    out.push(LocalPropertyTypeParameter {
                        source_name: crate::types::type_parameter_source_name(name).to_owned(),
                        semantic_name: name.to_owned(),
                        upper_bound: *bound,
                    });
                    collect(*bound, out);
                }
            }
            Ty::Nullable(inner)
            | Ty::PlatformNullable(inner)
            | Ty::DefinitelyNotNull(inner)
            | Ty::InProjection(inner)
            | Ty::OutProjection(inner) => collect(*inner, out),
            Ty::StarProjection(_) => {}
            Ty::Obj(_, arguments) => {
                for &argument in arguments {
                    collect(argument, out);
                }
            }
            Ty::Fun(signature) => {
                for &parameter in &signature.params {
                    collect(parameter, out);
                }
                collect(signature.ret, out);
            }
            _ => {}
        }
    }

    let mut parameters = Vec::new();
    collect(ty, &mut parameters);
    parameters
}

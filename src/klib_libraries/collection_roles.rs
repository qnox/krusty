//! Collection interface roles authenticated by the selected platform stdlib and its class headers.
//!
//! This is the KLIB platform's builtin collection map. Downstream consumers use the published
//! declaration fact, never a classifier spelling. Ordinary dependencies cannot acquire a role by
//! copying a canonical declaration name or a stdlib manifest.

use std::sync::OnceLock;

use crate::libraries::{LibraryType, TypeKind};
use crate::types::{type_name, CollectionKind, MappedCollection, TypeName};

type CollectionFace = (TypeName, MappedCollection, usize);

fn builtin_faces() -> &'static [CollectionFace] {
    static FACES: OnceLock<Vec<CollectionFace>> = OnceLock::new();
    FACES.get_or_init(|| {
        [
            ("Iterable", "MutableIterable", CollectionKind::Iterable, 1),
            ("Iterator", "MutableIterator", CollectionKind::Iterator, 1),
            (
                "Collection",
                "MutableCollection",
                CollectionKind::Collection,
                1,
            ),
            ("List", "MutableList", CollectionKind::List, 1),
            (
                "ListIterator",
                "MutableListIterator",
                CollectionKind::ListIterator,
                1,
            ),
            ("Set", "MutableSet", CollectionKind::Set, 1),
            ("Map", "MutableMap", CollectionKind::Map, 2),
            (
                "Map.Entry",
                "MutableMap.MutableEntry",
                CollectionKind::MapEntry,
                2,
            ),
        ]
        .into_iter()
        .flat_map(|(read_only, mutable, kind, arity)| {
            [(read_only, false), (mutable, true)].map(|(name, mutable)| {
                (
                    type_name(&format!("kotlin/collections/{name}")),
                    MappedCollection { kind, mutable },
                    arity,
                )
            })
        })
        .collect()
    })
}

pub(super) fn normalize(
    identity: TypeName,
    compiler_builtin_owner: bool,
    shape: &LibraryType,
) -> Result<Option<MappedCollection>, String> {
    if !compiler_builtin_owner {
        return Ok(None);
    }
    let Some((_, role, arity)) = builtin_faces()
        .iter()
        .find(|(name, _, _)| *name == identity)
    else {
        return Ok(None);
    };
    let own = shape.own_type_parameter_count;
    let total = shape.type_params().len();
    if shape.kind != TypeKind::Interface
        || own != *arity
        || total != *arity
        || shape.outer_instance.is_some()
    {
        return Err(format!(
            "authenticated collection {} requires an interface with {arity} own and total \
             type parameters and no outer instance; found {:?} with {own} own and {total} total \
             type parameters and outer instance {}",
            identity.render(),
            shape.kind,
            shape.outer_instance.is_some()
        ));
    }
    Ok(Some(*role))
}

//! The type parameters an inner class's metadata addresses by id without declaring them.
//!
//! kotlinc serializes an inner class under its outer classes' serializers, so every outer class's
//! type parameters hold the ids before the inner class's own and are written by id alone
//! (`Type.type_parameter`). The reader resolves those ids the way kotlinc's `TypeDeserializer`
//! does, through the outer class's own type-parameter context.

use crate::types::Ty;

/// `Class.flags.IS_INNER` (bit 9).
const IS_INNER: u64 = 1 << 9;

pub(super) fn is_inner(class_flags: u64) -> bool {
    class_flags & IS_INNER != 0
}

/// One type parameter a declaration may address by its metadata id.
#[derive(Clone, Debug, PartialEq)]
pub struct ScopedTypeParameter {
    pub id: u64,
    pub name: String,
    pub bounds: Vec<Ty>,
}

/// A classfile's packed `@Metadata` (`d1`, `d2`, `k`, `pn`), kept for an inner class whose
/// declarations decode completely only once its outer class's scope is known.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawMetadata {
    pub d1: Vec<String>,
    pub d2: Vec<String>,
    pub k: Option<i32>,
    pub package_name: Option<String>,
}

/// The scope a class's members decode in: the `enclosing` parameters, then the class's own
/// `(id, name)` parameters with their `bounds` (parallel).
pub(super) fn class_scope(
    enclosing: &[ScopedTypeParameter],
    own: &[(u64, String)],
    bounds: &[Vec<Ty>],
) -> Vec<ScopedTypeParameter> {
    enclosing
        .iter()
        .cloned()
        .chain(
            own.iter()
                .enumerate()
                .map(|(index, (id, name))| ScopedTypeParameter {
                    id: *id,
                    name: name.clone(),
                    bounds: bounds.get(index).cloned().unwrap_or_default(),
                }),
        )
        .collect()
}

/// `scope` as the parallel `(id, name)` and bounds lists the declaration decoders take.
pub(super) fn split(scope: &[ScopedTypeParameter]) -> (Vec<(u64, String)>, Vec<Vec<Ty>>) {
    scope
        .iter()
        .map(|parameter| {
            (
                (parameter.id, parameter.name.clone()),
                parameter.bounds.clone(),
            )
        })
        .unzip()
}

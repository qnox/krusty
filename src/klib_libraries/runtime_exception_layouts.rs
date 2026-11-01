//! Platform exception storage roles, authenticated before declarations enter the common model.

use crate::libraries::{LibraryType, RuntimeExceptionKind, RuntimeExceptionLayout, TypeKind};
use crate::types::{wk, TypeName};

pub(super) fn normalize(
    identity: TypeName,
    compiler_builtin_owner: bool,
    shape: &LibraryType,
    has_declared_property: bool,
) -> Option<RuntimeExceptionLayout> {
    if !compiler_builtin_owner {
        return None;
    }
    let kind = [
        RuntimeExceptionKind::Throwable,
        RuntimeExceptionKind::Exception,
        RuntimeExceptionKind::RuntimeException,
        RuntimeExceptionKind::IndexOutOfBounds,
        RuntimeExceptionKind::NullPointer,
    ]
    .into_iter()
    .find(|kind| kind.classifier() == identity)?;
    if shape.kind != TypeKind::Class
        || !shape.type_params().is_empty()
        || shape.outer_instance.is_some()
        || (kind != RuntimeExceptionKind::Throwable && has_declared_property)
    {
        return None;
    }
    let parents = shape.supertypes.iter_ids().collect::<Vec<_>>();
    let parent = if kind == RuntimeExceptionKind::Throwable {
        if parents.iter().any(|parent| *parent != wk::any()) {
            return None;
        }
        None
    } else {
        let [parent] = parents.as_slice() else {
            return None;
        };
        Some(*parent)
    };
    Some(RuntimeExceptionLayout { kind, parent })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identical_dependency_header_does_not_acquire_runtime_storage() {
        let mut shape = LibraryType::declaration_header();
        let identity = RuntimeExceptionKind::IndexOutOfBounds.classifier();
        let parent = RuntimeExceptionKind::RuntimeException.classifier();
        shape.supertypes.push_name(parent);
        assert_eq!(normalize(identity, false, &shape, false), None);
        assert_eq!(
            normalize(identity, true, &shape, false),
            Some(RuntimeExceptionLayout {
                kind: RuntimeExceptionKind::IndexOutOfBounds,
                parent: Some(parent),
            })
        );
    }

    #[test]
    fn runtime_subclass_storage_rejects_added_state_and_missing_parent() {
        let identity = RuntimeExceptionKind::IndexOutOfBounds.classifier();
        let mut shape = LibraryType::declaration_header();
        assert_eq!(normalize(identity, true, &shape, false), None);
        shape
            .supertypes
            .push_name(RuntimeExceptionKind::RuntimeException.classifier());
        assert_eq!(normalize(identity, true, &shape, true), None);
    }

    #[test]
    fn null_pointer_storage_requires_the_platform_owner_and_declared_parent() {
        let identity = RuntimeExceptionKind::NullPointer.classifier();
        let parent = RuntimeExceptionKind::RuntimeException.classifier();
        let mut shape = LibraryType::declaration_header();
        assert_eq!(normalize(identity, true, &shape, false), None);
        shape.supertypes.push_name(parent);
        assert_eq!(normalize(identity, false, &shape, false), None);
        assert_eq!(normalize(identity, true, &shape, true), None);
        assert_eq!(
            normalize(identity, true, &shape, false),
            Some(RuntimeExceptionLayout {
                kind: RuntimeExceptionKind::NullPointer,
                parent: Some(parent),
            }),
        );
    }
}

//! Positive declaration capabilities shared by dependency planning and Native emission.

use krusty::libraries::{CompilerIntrinsic, ListStorageConstruction};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DependencyOperation {
    ListStorage(ListStorageConstruction),
    StringPlus,
    /// A builtin scalar's own `hashCode()`, which Kotlin fixes per scalar type.
    PrimitiveHashCode,
}

pub(super) fn operation_plan(operation: CompilerIntrinsic) -> Option<DependencyOperation> {
    match operation {
        CompilerIntrinsic::ListStorageConstruction(plan) => {
            Some(DependencyOperation::ListStorage(plan))
        }
        CompilerIntrinsic::StringPlus => Some(DependencyOperation::StringPlus),
        CompilerIntrinsic::PrimitiveHashCode => Some(DependencyOperation::PrimitiveHashCode),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CraneliftBackend, NativeTarget};
    use krusty::backend::Backend;

    #[test]
    fn klib_target_string_plus_is_a_native_dependency_capability() {
        for &target in NativeTarget::ALL {
            assert!(CraneliftBackend::new(target)
                .realizes_dependency_intrinsic(CompilerIntrinsic::StringPlus));
        }
    }

    #[test]
    fn klib_primitive_hash_code_is_a_native_dependency_capability() {
        for &target in NativeTarget::ALL {
            assert!(CraneliftBackend::new(target)
                .realizes_dependency_intrinsic(CompilerIntrinsic::PrimitiveHashCode));
        }
    }

    #[test]
    fn library_algorithm_adapters_are_not_native_dependency_capabilities() {
        for &target in NativeTarget::ALL {
            let backend = CraneliftBackend::new(target);
            for operation in [
                CompilerIntrinsic::TextTrim,
                CompilerIntrinsic::CollectionSizeOrDefault,
                CompilerIntrinsic::ResultFailure(krusty::libraries::ResultFailureIntrinsic::Create),
                CompilerIntrinsic::UnsignedDivide {
                    operand: krusty::types::Ty::Int,
                },
                CompilerIntrinsic::UnsignedRemainder {
                    operand: krusty::types::Ty::Int,
                },
            ] {
                assert!(
                    !backend.realizes_dependency_intrinsic(operation),
                    "{target:?}: {operation:?}"
                );
            }
        }
    }
}

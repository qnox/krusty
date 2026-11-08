//! Positive declaration capabilities shared by dependency planning and Native emission.

use crate::libraries::{CompilerIntrinsic, ListStorageConstruction};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DependencyOperation {
    ListStorage(ListStorageConstruction),
    StringPlus,
    /// A builtin scalar's own `hashCode()`, which Kotlin fixes per scalar type.
    PrimitiveHashCode,
    /// A primitive iterator's own `next()` (`IntIterator.next(): Int`). The runtime's iteration
    /// protocol answers it through the receiver's descriptor, as it answers `Iterator.next`.
    PrimitiveIteratorNext,
}

pub(super) fn operation_plan(operation: CompilerIntrinsic) -> Option<DependencyOperation> {
    match operation {
        CompilerIntrinsic::ListStorageConstruction(plan) => {
            Some(DependencyOperation::ListStorage(plan))
        }
        CompilerIntrinsic::StringPlus => Some(DependencyOperation::StringPlus),
        CompilerIntrinsic::PrimitiveHashCode => Some(DependencyOperation::PrimitiveHashCode),
        CompilerIntrinsic::PrimitiveIteratorNext => {
            Some(DependencyOperation::PrimitiveIteratorNext)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Backend;
    use crate::native::{CraneliftBackend, NativeTarget};

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
    fn klib_primitive_iterator_next_is_a_native_dependency_capability() {
        for &target in NativeTarget::ALL {
            assert!(CraneliftBackend::new(target)
                .realizes_dependency_intrinsic(CompilerIntrinsic::PrimitiveIteratorNext));
        }
    }

    #[test]
    fn library_algorithm_adapters_are_not_native_dependency_capabilities() {
        for &target in NativeTarget::ALL {
            let backend = CraneliftBackend::new(target);
            for operation in [
                CompilerIntrinsic::TextTrim,
                CompilerIntrinsic::CollectionSizeOrDefault,
                CompilerIntrinsic::ResultFailure(crate::libraries::ResultFailureIntrinsic::Create),
                CompilerIntrinsic::UnsignedDivide {
                    operand: crate::types::Ty::Int,
                },
                CompilerIntrinsic::UnsignedRemainder {
                    operand: crate::types::Ty::Int,
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

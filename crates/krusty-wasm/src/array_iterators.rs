//! A walker over `Array<T>`, the iterator `kotlin.Array.iterator` returns.
//!
//! `Array` is generic, so a support unit does not materialize it, and the stdlib iterator body
//! is not emitted. `putAll` walks an array of pairs through `Iterator.hasNext` and `Iterator.next`.
//! Those calls dispatch on the interface; this struct is one representation they test for.

use super::array_objects::STORAGE_FIELD;
use super::encode::{
    Code, CompositeType, FieldType, Function, HeapType, Module, StorageType, SubType, ValType,
};
use super::objects::REFERENCE;
use super::runtime::{ADD32, LT_S32};

const ARRAY_FIELD: u32 = 0;
const INDEX_FIELD: u32 = 1;

/// Which `Iterator` member a walk answers.
#[derive(Clone, Copy)]
pub(super) enum WalkStep {
    HasNext,
    Next,
}

/// The iterator struct and the functions that implement its `Iterator` members, defined on first
/// use so a program that never walks an array does not carry them.
pub(super) struct ArrayIterators {
    pub(super) object: u32,
    pub(super) array: u32,
    storage: u32,
    has_next: Option<u32>,
    next: Option<u32>,
}

impl ArrayIterators {
    pub(super) fn declare(module: &mut Module, array: u32, storage: u32) -> Self {
        let object = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                FieldType {
                    storage: StorageType::Val(ValType::nullable(array)),
                    mutable: false,
                },
                FieldType {
                    storage: StorageType::Val(ValType::I32),
                    mutable: true,
                },
            ]),
            supertype: None,
            open: false,
        });
        Self {
            object,
            array,
            storage,
            has_next: None,
            next: None,
        }
    }

    pub(super) fn function(&self, step: WalkStep) -> Option<u32> {
        match step {
            WalkStep::HasNext => self.has_next,
            WalkStep::Next => self.next,
        }
    }

    pub(super) fn remember(&mut self, step: WalkStep, function: u32) {
        match step {
            WalkStep::HasNext => self.has_next = Some(function),
            WalkStep::Next => self.next = Some(function),
        }
    }

    /// The function carried as `member` is, reading this iterator.
    pub(super) fn define(&self, module: &mut Module, member_type: u32, step: WalkStep) -> u32 {
        let mut code = Code::default();
        let locals = match step {
            WalkStep::HasNext => {
                let length = 1;
                code.local_get(0)
                    .ref_cast(false, HeapType::Concrete(self.object))
                    .struct_get(self.object, ARRAY_FIELD)
                    .ref_cast(false, HeapType::Concrete(self.array))
                    .struct_get(self.array, STORAGE_FIELD)
                    .array_len()
                    .local_set(length);
                code.local_get(0)
                    .ref_cast(false, HeapType::Concrete(self.object))
                    .struct_get(self.object, INDEX_FIELD)
                    .local_get(length)
                    .op(LT_S32);
                vec![ValType::I32]
            }
            WalkStep::Next => {
                let element = 1;
                code.local_get(0)
                    .ref_cast(false, HeapType::Concrete(self.object))
                    .struct_get(self.object, ARRAY_FIELD)
                    .ref_cast(false, HeapType::Concrete(self.array))
                    .struct_get(self.array, STORAGE_FIELD)
                    .ref_as_non_null()
                    .local_get(0)
                    .ref_cast(false, HeapType::Concrete(self.object))
                    .struct_get(self.object, INDEX_FIELD)
                    .array_get(self.storage)
                    .local_set(element);
                code.local_get(0)
                    .ref_cast(false, HeapType::Concrete(self.object))
                    .local_get(0)
                    .ref_cast(false, HeapType::Concrete(self.object))
                    .struct_get(self.object, INDEX_FIELD)
                    .i32_const(1)
                    .op(ADD32)
                    .struct_set(self.object, INDEX_FIELD);
                code.local_get(element);
                vec![REFERENCE]
            }
        };
        let function = module.declare();
        module.define(
            function,
            Function {
                ty: member_type,
                locals,
                code,
            },
        );
        function
    }
}

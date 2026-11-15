//! Runtime `ArrayList`, and the array wrapper `asArrayList` builds one from.
//!
//! `ArrayList` is generic, so a support unit does not materialize it. `mutableListOf` and
//! `listOf` construct it from `Array.asCollection`, then `List.get`, `isEmpty` and
//! `Iterable.iterator` read that list. The wrapper keeps the array; the list copies its storage.

use super::array_objects::STORAGE_FIELD;
use super::encode::{
    BlockType, Code, CompositeType, FieldType, Function, HeapType, Module, StorageType, SubType,
    ValType,
};
use super::objects::REFERENCE;
use super::runtime::{ADD32, EQZ32, LT_S32};

const ELEMENTS: u32 = 0;
const SIZE: u32 = 1;
const ITER_LIST: u32 = 0;
const ITER_INDEX: u32 = 1;
const INITIAL: i32 = 4;

/// Which list operation a call asks the runtime list for.
#[derive(Clone, Copy)]
pub(super) enum ListSlot {
    Get,
    IsEmpty,
    Size,
    Iterator,
    Add,
}

/// The list, the array wrapper it is built from, and the iterator a walk uses.
pub(super) struct Lists {
    pub(super) list: u32,
    pub(super) wrapper: u32,
    pub(super) iterator: u32,
    storage: u32,
    pub(super) array: u32,
    from_elements: u32,
    get: u32,
    is_empty: u32,
    size: u32,
    iterator_fn: u32,
    add: u32,
    has_next: u32,
    next: u32,
}

impl Lists {
    pub(super) fn declare(module: &mut Module, array: u32, storage: u32) -> Self {
        let list = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                FieldType {
                    storage: StorageType::Val(ValType::reference(storage)),
                    mutable: true,
                },
                FieldType {
                    storage: StorageType::Val(ValType::I32),
                    mutable: true,
                },
            ]),
            supertype: None,
            open: false,
        });
        let wrapper = module.subtype(SubType {
            composite: CompositeType::Struct(vec![FieldType {
                storage: StorageType::Val(ValType::nullable(array)),
                mutable: false,
            }]),
            supertype: None,
            open: false,
        });
        let iterator = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                FieldType {
                    storage: StorageType::Val(ValType::reference(list)),
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
            list,
            wrapper,
            iterator,
            storage,
            array,
            from_elements: module.declare(),
            get: module.declare(),
            is_empty: module.declare(),
            size: module.declare(),
            iterator_fn: module.declare(),
            add: module.declare(),
            has_next: module.declare(),
            next: module.declare(),
        }
    }

    pub(super) fn function(&self, slot: ListSlot) -> u32 {
        match slot {
            ListSlot::Get => self.get,
            ListSlot::IsEmpty => self.is_empty,
            ListSlot::Size => self.size,
            ListSlot::Iterator => self.iterator_fn,
            ListSlot::Add => self.add,
        }
    }

    pub(super) fn walk(&self, has_next: bool) -> u32 {
        if has_next {
            self.has_next
        } else {
            self.next
        }
    }

    pub(super) fn define(&self, module: &mut Module) {
        self.define_from_elements(module);
        self.define_get(module);
        self.define_is_empty(module);
        self.define_size(module);
        self.define_iterator(module);
        self.define_add(module);
        self.define_has_next(module);
        self.define_next(module);
    }

    /// `(collection) -> list`. A wrapper or a reference array is copied; anything else is empty.
    fn define_from_elements(&self, module: &mut Module) {
        let ty = module.func_type(vec![REFERENCE], vec![REFERENCE]);
        let mut code = Code::default();
        let source = 1;
        code.local_get(0)
            .ref_test(false, HeapType::Concrete(self.wrapper));
        code.if_(BlockType::Empty);
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(self.wrapper))
            .struct_get(self.wrapper, 0)
            .local_set(source);
        self.copy_array(&mut code, source);
        code.return_().end();
        code.local_get(0)
            .ref_test(false, HeapType::Concrete(self.array));
        code.if_(BlockType::Empty);
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(self.array))
            .local_set(source);
        self.copy_array(&mut code, source);
        code.return_().end();
        self.empty(&mut code);
        module.define(
            self.from_elements,
            Function {
                ty,
                locals: vec![
                    ValType::nullable(self.array),
                    ValType::reference(self.storage),
                    ValType::I32,
                    ValType::reference(self.storage),
                ],
                code,
            },
        );
    }

    /// Copy the reference array in local `source` into a new list, left on the stack.
    fn copy_array(&self, code: &mut Code, source: u32) {
        let (storage, length, grown) = (2, 3, 4);
        code.local_get(source)
            .ref_as_non_null()
            .struct_get(self.array, STORAGE_FIELD)
            .local_tee(storage)
            .array_len()
            .local_set(length);
        code.local_get(length).i32_const(0).op(super::runtime::EQ32);
        code.if_(BlockType::Value(ValType::I32))
            .i32_const(INITIAL)
            .else_()
            .local_get(length)
            .end();
        code.array_new_default(self.storage).local_tee(grown);
        code.i32_const(0);
        code.local_get(storage);
        code.i32_const(0);
        code.local_get(length);
        code.array_copy(self.storage, self.storage);
        code.local_get(grown);
        code.local_get(length);
        code.struct_new(self.list);
    }

    fn empty(&self, code: &mut Code) {
        code.i32_const(INITIAL).array_new_default(self.storage);
        code.i32_const(0);
        code.struct_new(self.list);
    }

    /// An empty list. Used when the constructor carries no elements.
    pub(super) fn construct_empty(&self, code: &mut Code) {
        self.empty(code);
    }

    /// `ArrayList(collection)`, copying a wrapper or a reference array.
    pub(super) fn construct_from(&self, code: &mut Code) {
        code.call(self.from_elements);
    }

    /// The wrapper `Array.asCollection` returns. The array is already on the stack.
    pub(super) fn wrap_array(&self, code: &mut Code) {
        code.struct_new(self.wrapper);
    }

    fn define_get(&self, module: &mut Module) {
        let ty = module.func_type(vec![REFERENCE, ValType::I32], vec![REFERENCE]);
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(self.list))
            .struct_get(self.list, ELEMENTS)
            .local_get(1)
            .array_get(self.storage);
        module.define(
            self.get,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    fn define_is_empty(&self, module: &mut Module) {
        let ty = module.func_type(vec![REFERENCE], vec![ValType::I32]);
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(self.list))
            .struct_get(self.list, SIZE)
            .op(EQZ32);
        module.define(
            self.is_empty,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    fn define_size(&self, module: &mut Module) {
        let ty = module.func_type(vec![REFERENCE], vec![ValType::I32]);
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(self.list))
            .struct_get(self.list, SIZE);
        module.define(
            self.size,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    fn define_iterator(&self, module: &mut Module) {
        let ty = module.func_type(vec![REFERENCE], vec![REFERENCE]);
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(self.list));
        code.i32_const(0);
        code.struct_new(self.iterator);
        module.define(
            self.iterator_fn,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    fn define_add(&self, module: &mut Module) {
        let ty = module.func_type(vec![REFERENCE, REFERENCE], vec![ValType::I32]);
        let (receiver, element, size) = (0, 1, 2);
        let mut code = Code::default();
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.list))
            .struct_get(self.list, SIZE)
            .local_tee(size);
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.list))
            .struct_get(self.list, ELEMENTS)
            .array_len()
            .op(super::runtime::GE_S32);
        code.if_(BlockType::Empty);
        self.grow(&mut code, receiver);
        code.end();
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.list))
            .struct_get(self.list, ELEMENTS)
            .local_get(size)
            .local_get(element)
            .array_set(self.storage);
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.list))
            .local_get(size)
            .i32_const(1)
            .op(ADD32)
            .struct_set(self.list, SIZE);
        code.i32_const(1);
        module.define(
            self.add,
            Function {
                ty,
                locals: vec![
                    ValType::I32,
                    ValType::I32,
                    ValType::reference(self.storage),
                    ValType::reference(self.storage),
                ],
                code,
            },
        );
    }

    fn grow(&self, code: &mut Code, receiver: u32) {
        let (length, old, grown) = (3, 4, 5);
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.list))
            .struct_get(self.list, ELEMENTS)
            .local_tee(old)
            .array_len()
            .local_tee(length)
            .i32_const(2)
            .op(super::runtime::MUL32);
        code.array_new_default(self.storage).local_tee(grown);
        code.i32_const(0);
        code.local_get(old);
        code.i32_const(0);
        code.local_get(length);
        code.array_copy(self.storage, self.storage);
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.list))
            .local_get(grown)
            .struct_set(self.list, ELEMENTS);
    }

    fn define_has_next(&self, module: &mut Module) {
        let ty = module.func_type(vec![REFERENCE], vec![ValType::I32]);
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(self.iterator))
            .struct_get(self.iterator, ITER_INDEX);
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(self.iterator))
            .struct_get(self.iterator, ITER_LIST)
            .struct_get(self.list, SIZE);
        code.op(LT_S32);
        module.define(
            self.has_next,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    fn define_next(&self, module: &mut Module) {
        let ty = module.func_type(vec![REFERENCE], vec![REFERENCE]);
        let element = 1;
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(self.iterator))
            .struct_get(self.iterator, ITER_LIST)
            .struct_get(self.list, ELEMENTS);
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(self.iterator))
            .struct_get(self.iterator, ITER_INDEX)
            .array_get(self.storage)
            .local_set(element);
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(self.iterator));
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(self.iterator))
            .struct_get(self.iterator, ITER_INDEX)
            .i32_const(1)
            .op(ADD32)
            .struct_set(self.iterator, ITER_INDEX);
        code.local_get(element);
        module.define(
            self.next,
            Function {
                ty,
                locals: vec![REFERENCE],
                code,
            },
        );
    }
}

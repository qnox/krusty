//! Runtime `HashMap` and `LinkedHashMap`.
//!
//! Both classes are generic, so a support unit does not materialize them. `mapOf`, `hashMapOf`
//! and `associateWith` allocate one and then `put` through `MutableMap`; `getValue` reads it
//! back through `Map.get`. One struct answers every one of those: parallel key and value arrays
//! compared with Kotlin's structural equality.

use super::encode::{
    Code, CompositeType, FieldType, Function, HeapType, Module, StorageType, SubType, ValType,
};
use super::objects::REFERENCE;
use super::runtime::{ADD32, GE_S32, LT_S32, MUL32, NE32};

const KEYS: u32 = 0;
const VALUES: u32 = 1;
const SIZE: u32 = 2;
const INITIAL: i32 = 4;

/// Which entry operation a call asks the runtime map for.
#[derive(Clone, Copy)]
pub(super) enum MapSlot {
    Get,
    Put,
    ContainsKey,
}

/// The map struct and the functions that search and update it.
pub(super) struct Maps {
    pub(super) object: u32,
    storage: u32,
    find: u32,
    get: u32,
    put: u32,
    contains_key: u32,
}

impl Maps {
    pub(super) fn declare(module: &mut Module) -> Self {
        let storage = module.ty(CompositeType::Array(FieldType {
            storage: StorageType::Val(REFERENCE),
            mutable: true,
        }));
        let object = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                FieldType {
                    storage: StorageType::Val(ValType::reference(storage)),
                    mutable: true,
                },
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
        Self {
            object,
            storage,
            find: module.declare(),
            get: module.declare(),
            put: module.declare(),
            contains_key: module.declare(),
        }
    }

    pub(super) fn function(&self, slot: MapSlot) -> u32 {
        match slot {
            MapSlot::Get => self.get,
            MapSlot::Put => self.put,
            MapSlot::ContainsKey => self.contains_key,
        }
    }

    /// `(map, key) -> index`, or `-1` when `key` is absent. `map` is `(ref null eq)`.
    pub(super) fn define(&self, module: &mut Module, equals: u32) {
        self.define_find(module, equals);
        self.define_get(module);
        self.define_contains_key(module);
        self.define_put(module);
    }

    fn define_find(&self, module: &mut Module, equals: u32) {
        let ty = module.func_type(vec![REFERENCE, REFERENCE], vec![ValType::I32]);
        let (receiver, key, index, size) = (0, 1, 2, 3);
        let mut code = Code::default();
        code.i32_const(0).local_set(index);
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.object))
            .struct_get(self.object, SIZE)
            .local_set(size);
        code.loop_(super::encode::BlockType::Empty);
        code.local_get(index).local_get(size).op(GE_S32);
        code.if_(super::encode::BlockType::Empty)
            .i32_const(-1)
            .return_()
            .end();
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.object))
            .struct_get(self.object, KEYS)
            .local_get(index)
            .array_get(self.storage);
        code.local_get(key).call(equals);
        code.if_(super::encode::BlockType::Empty)
            .local_get(index)
            .return_()
            .end();
        code.local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index);
        code.br(0).end();
        code.unreachable();
        module.define(
            self.find,
            Function {
                ty,
                locals: vec![ValType::I32, ValType::I32],
                code,
            },
        );
    }

    fn define_get(&self, module: &mut Module) {
        let ty = module.func_type(vec![REFERENCE, REFERENCE], vec![REFERENCE]);
        let (receiver, key, index) = (0, 1, 2);
        let mut code = Code::default();
        code.local_get(receiver).local_get(key).call(self.find);
        code.local_tee(index).i32_const(0).op(LT_S32);
        code.if_(super::encode::BlockType::Empty)
            .ref_null(HeapType::Eq)
            .return_()
            .end();
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.object))
            .struct_get(self.object, VALUES)
            .local_get(index)
            .array_get(self.storage);
        module.define(
            self.get,
            Function {
                ty,
                locals: vec![ValType::I32],
                code,
            },
        );
    }

    fn define_contains_key(&self, module: &mut Module) {
        let ty = module.func_type(vec![REFERENCE, REFERENCE], vec![ValType::I32]);
        let mut code = Code::default();
        code.local_get(0)
            .local_get(1)
            .call(self.find)
            .i32_const(-1)
            .op(NE32);
        module.define(
            self.contains_key,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    fn define_put(&self, module: &mut Module) {
        let ty = module.func_type(vec![REFERENCE, REFERENCE, REFERENCE], vec![REFERENCE]);
        let (receiver, key, value, index, size) = (0, 1, 2, 3, 4);
        let mut code = Code::default();
        code.local_get(receiver).local_get(key).call(self.find);
        code.local_tee(index).i32_const(0).op(LT_S32);
        code.if_(super::encode::BlockType::Empty);
        self.append(&mut code, receiver, key, value, size);
        code.ref_null(HeapType::Eq).return_().end();
        let previous = 5;
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.object))
            .struct_get(self.object, VALUES)
            .local_get(index)
            .array_get(self.storage)
            .local_set(previous);
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.object))
            .struct_get(self.object, VALUES)
            .local_get(index)
            .local_get(value)
            .array_set(self.storage);
        code.local_get(previous);
        module.define(
            self.put,
            Function {
                ty,
                locals: vec![
                    ValType::I32,
                    ValType::I32,
                    REFERENCE,
                    ValType::I32,
                    ValType::reference(self.storage),
                    ValType::reference(self.storage),
                ],
                code,
            },
        );
    }

    /// Append `(key, value)` at `size`, growing the arrays when they are full.
    fn append(&self, code: &mut Code, receiver: u32, key: u32, value: u32, size: u32) {
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.object))
            .struct_get(self.object, SIZE)
            .local_tee(size);
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.object))
            .struct_get(self.object, KEYS)
            .array_len()
            .op(GE_S32);
        code.if_(super::encode::BlockType::Empty);
        self.grow(code, receiver, KEYS);
        self.grow(code, receiver, VALUES);
        code.end();
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.object))
            .struct_get(self.object, KEYS)
            .local_get(size)
            .local_get(key)
            .array_set(self.storage);
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.object))
            .struct_get(self.object, VALUES)
            .local_get(size)
            .local_get(value)
            .array_set(self.storage);
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.object))
            .local_get(size)
            .i32_const(1)
            .op(ADD32)
            .struct_set(self.object, SIZE);
    }

    /// Replace field `field` with an array twice as long, copying the entries it held.
    fn grow(&self, code: &mut Code, receiver: u32, field: u32) {
        let (length, old, grown) = (6, 7, 8);
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.object))
            .struct_get(self.object, field)
            .local_tee(old)
            .array_len()
            .local_tee(length)
            .i32_const(2)
            .op(MUL32);
        code.array_new_default(self.storage).local_tee(grown);
        code.i32_const(0);
        code.local_get(old);
        code.i32_const(0);
        code.local_get(length);
        code.array_copy(self.storage, self.storage);
        code.local_get(receiver)
            .ref_cast(false, HeapType::Concrete(self.object))
            .local_get(grown)
            .struct_set(self.object, field);
    }

    /// An empty map. Capacity arguments are hints; the arrays grow when a `put` fills them.
    pub(super) fn construct(&self, code: &mut Code) {
        code.i32_const(INITIAL).array_new_default(self.storage);
        code.i32_const(INITIAL).array_new_default(self.storage);
        code.i32_const(0);
        code.struct_new(self.object);
    }
}

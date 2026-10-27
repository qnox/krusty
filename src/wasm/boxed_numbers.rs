//! Full-width signed numeric references with the ordinary object/table prefix.
//!
//! Int and Long have distinct GC payload types. A checked generic result is unboxed through its
//! exact heap type; this is not the realization of an unchecked Kotlin source cast.

use super::encode::{BlockType, Code, CompositeType, Function, HeapType, Module, SubType, ValType};
use super::objects::{immutable, ObjectRoot, REFERENCE};

pub(super) struct BoxedNumbers {
    pub(super) int: NumericBox,
    pub(super) long: NumericBox,
}

pub(super) struct NumericBox {
    pub(super) box_value: u32,
    pub(super) unbox_value: u32,
}

#[derive(Clone, Copy)]
enum SignedNumber {
    Int,
    Long,
}

impl SignedNumber {
    fn carrier(self) -> ValType {
        match self {
            Self::Int => ValType::I32,
            Self::Long => ValType::I64,
        }
    }
}

impl BoxedNumbers {
    pub(super) fn declare(module: &mut Module, objects: &ObjectRoot, render: [u32; 2]) -> Self {
        Self {
            int: NumericBox::declare(module, objects, SignedNumber::Int, render[0]),
            long: NumericBox::declare(module, objects, SignedNumber::Long, render[1]),
        }
    }
}

impl NumericBox {
    fn declare(
        module: &mut Module,
        objects: &ObjectRoot,
        number: SignedNumber,
        render: u32,
    ) -> Self {
        let value = number.carrier();
        let structure = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(objects.any_table)),
                immutable(value),
            ]),
            supertype: Some(objects.object),
            open: false,
        });
        let heap = HeapType::Concrete(structure);
        let equals = module.declare();
        let hash = module.declare();
        let to_string = module.declare();
        let mut code = Code::default();
        code.local_get(1)
            .ref_test(false, heap)
            .if_(BlockType::Value(ValType::I32))
            .local_get(0)
            .ref_cast(false, heap)
            .struct_get(structure, 1)
            .local_get(1)
            .ref_cast(false, heap)
            .struct_get(structure, 1)
            .op(match number {
                SignedNumber::Int => 0x46,
                SignedNumber::Long => 0x51,
            })
            .else_()
            .i32_const(0)
            .end();
        module.define(
            equals,
            Function {
                ty: objects.any_slot_types[0],
                locals: Vec::new(),
                code,
            },
        );
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, heap)
            .struct_get(structure, 1);
        if matches!(number, SignedNumber::Long) {
            code.local_get(0)
                .ref_cast(false, heap)
                .struct_get(structure, 1)
                .i64_const(32)
                .op(0x88) // i64.shr_u
                .op(0x85) // i64.xor
                .op(0xa7); // i32.wrap_i64
        }
        module.define(
            hash,
            Function {
                ty: objects.any_slot_types[1],
                locals: Vec::new(),
                code,
            },
        );
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, heap)
            .struct_get(structure, 1)
            .call(render);
        module.define(
            to_string,
            Function {
                ty: objects.any_slot_types[2],
                locals: Vec::new(),
                code,
            },
        );
        let mut table = Code::default();
        for function in [equals, hash, to_string] {
            module.reference_function(function);
            table.ref_func(function);
        }
        table
            .ref_null(HeapType::Concrete(objects.itable))
            .struct_new(objects.any_table);
        let table = module.constant_global(ValType::reference(objects.any_table), table);
        let box_value = module.declare();
        let ty = module.func_type(vec![value], vec![REFERENCE]);
        let mut code = Code::default();
        code.global_get(table).local_get(0).struct_new(structure);
        module.define(
            box_value,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
        let unbox_value = module.declare();
        let ty = module.func_type(vec![REFERENCE], vec![value]);
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, heap)
            .struct_get(structure, 1);
        module.define(
            unbox_value,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
        Self {
            box_value,
            unbox_value,
        }
    }
}

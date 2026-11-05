//! Callable-reference objects compare their checked declaration, adaptation, and bound receiver.

mod declarations;

pub(super) use declarations::Declaration;

use crate::backend::DependencyExportSignatures;
use crate::fx_hash::HashMap;
use crate::ir::IrFile;
use crate::types::TypeName;

use super::encode::{BlockType, Code, CompositeType, Function, HeapType, Module, SubType, ValType};
use super::objects::{immutable, ObjectRoot, REFERENCE};

pub(super) const DECLARATION_FIELD: u32 = 1;
pub(super) const RECEIVER_FIELD: u32 = 2;
pub(super) const PREFIX_FIELDS: u32 = 3;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct ReferenceKind {
    pub(super) declaration: Declaration,
    pub(super) bound: bool,
    pub(super) reflection_owner: Option<TypeName>,
    pub(super) arity: usize,
    pub(super) adapted: bool,
    pub(super) adaptation_flags: u8,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Kind {
    declaration: u32,
    bound: bool,
    reflection_owner: Option<TypeName>,
    arity: usize,
    adapted: bool,
    adaptation_flags: u8,
}

pub(super) struct CallableReferences {
    pub(super) object: u32,
    pub(super) equals: u32,
    pub(super) hash_code: u32,
    declarations: declarations::Declarations,
    kinds: HashMap<Kind, i32>,
}

impl CallableReferences {
    pub(super) fn declare(module: &mut Module, objects: &ObjectRoot, object_equals: u32) -> Self {
        let object = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(objects.any_table)),
                immutable(ValType::I32),
                immutable(REFERENCE),
            ]),
            supertype: Some(objects.object),
            open: true,
        });
        let equals = module.declare();
        let mut code = Code::default();
        code.local_get(0)
            .local_get(1)
            .ref_eq()
            .if_(BlockType::Empty)
            .i32_const(1)
            .return_()
            .end()
            .local_get(1)
            .ref_test(false, HeapType::Concrete(object))
            .op(super::runtime::EQZ32)
            .if_(BlockType::Empty)
            .i32_const(0)
            .return_()
            .end();
        for value in [0, 1] {
            code.local_get(value)
                .ref_cast(false, HeapType::Concrete(object))
                .struct_get(object, DECLARATION_FIELD);
        }
        code.op(super::runtime::NE32)
            .if_(BlockType::Empty)
            .i32_const(0)
            .return_()
            .end();
        for value in [0, 1] {
            code.local_get(value)
                .ref_cast(false, HeapType::Concrete(object))
                .struct_get(object, RECEIVER_FIELD);
        }
        code.call(object_equals);
        module.define(
            equals,
            Function {
                ty: objects.any_slot_types[0],
                locals: Vec::new(),
                code,
            },
        );
        let hash_code = module.declare();
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, DECLARATION_FIELD);
        module.define(
            hash_code,
            Function {
                ty: objects.any_slot_types[1],
                locals: Vec::new(),
                code,
            },
        );
        Self {
            object,
            equals,
            hash_code,
            declarations: Default::default(),
            kinds: HashMap::default(),
        }
    }

    pub(super) fn begin_file(
        &mut self,
        module: &str,
        ir: &IrFile,
        exports: &DependencyExportSignatures,
    ) -> Result<u32, String> {
        self.declarations.begin_file(module, ir, exports)
    }

    pub(super) fn kind(&mut self, kind: ReferenceKind) -> Result<i32, String> {
        let kind = Kind {
            declaration: self.declarations.intern(kind.declaration)?,
            bound: kind.bound,
            reflection_owner: kind.reflection_owner,
            arity: kind.arity,
            adapted: kind.adapted,
            adaptation_flags: kind.adaptation_flags,
        };
        let next =
            i32::try_from(self.kinds.len()).map_err(|_| "too many callable-reference kinds")?;
        Ok(*self.kinds.entry(kind).or_insert(next))
    }
}

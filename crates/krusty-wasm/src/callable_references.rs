//! Callable-reference objects compare their checked declaration, adaptation, and bound receiver.
//!
//! A reference is still an object with an identity: kotlinc-wasm's `toString` prints its identity
//! hash, which its `hashCode`, the declaration's, is not. So the object extends `$Identified` and
//! is given its hash when it is created.

mod declarations;
mod names;
mod property_accessors;

pub(super) use declarations::Declaration;
use names::ReferenceNames;
use property_accessors::PropertyAccessors;
pub(crate) use property_accessors::{AccessorShape, PropertyAccess};

use krusty::backend::DependencyExportSignatures;
use krusty::fx_hash::HashMap;
use krusty::ir::IrFile;
use krusty::types::TypeName;

use super::encode::{BlockType, Code, CompositeType, Function, HeapType, Module, SubType, ValType};
use super::objects::{immutable, ObjectRoot, REFERENCE};

pub(super) const DECLARATION_FIELD: u32 = super::identity::PREFIX_FIELDS;
pub(super) const RECEIVER_FIELD: u32 = DECLARATION_FIELD + 1;
/// The values bound for the declaration's context parameters, in parameter order, as a reference
/// array; null when it binds none. Equality reads them as it reads the receiver.
pub(super) const CONTEXTS_FIELD: u32 = RECEIVER_FIELD + 1;
pub(super) const PREFIX_FIELDS: u32 = CONTEXTS_FIELD + 1;

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
    /// The array type the bound context values are carried in.
    pub(super) contexts: u32,
    pub(super) equals: u32,
    pub(super) hash_code: u32,
    declarations: declarations::Declarations,
    kinds: HashMap<Kind, i32>,
    names: ReferenceNames,
    accessors: PropertyAccessors,
}

impl CallableReferences {
    pub(super) fn declare(
        module: &mut Module,
        objects: &ObjectRoot,
        object_equals: u32,
        contexts: u32,
    ) -> Self {
        let object = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(objects.any_table)),
                immutable(ValType::I32),
                immutable(ValType::I32),
                immutable(REFERENCE),
                immutable(ValType::nullable(contexts)),
            ]),
            supertype: Some(objects.identified),
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
        // The same kind binds the same number of contexts, so the two arrays are both present or
        // both absent, and as long as each other.
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, CONTEXTS_FIELD)
            .ref_is_null()
            .op(super::runtime::EQZ32)
            .if_(BlockType::Empty)
            // Locals: 2 = this' contexts, 3 = the other's, 4 = the next index.
            .local_get(0)
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, CONTEXTS_FIELD)
            .local_set(2)
            .local_get(1)
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, CONTEXTS_FIELD)
            .local_set(3)
            .block(BlockType::Empty)
            .loop_(BlockType::Empty)
            .local_get(4)
            .local_get(2)
            .ref_as_non_null()
            .array_len()
            .op(super::runtime::GE_S32)
            .br_if(1)
            .local_get(2)
            .ref_as_non_null()
            .local_get(4)
            .array_get(contexts)
            .local_get(3)
            .ref_as_non_null()
            .local_get(4)
            .array_get(contexts)
            .call(object_equals)
            .op(super::runtime::EQZ32)
            .if_(BlockType::Empty)
            .i32_const(0)
            .return_()
            .end()
            .local_get(4)
            .i32_const(1)
            .op(super::runtime::ADD32)
            .local_set(4)
            .br(0)
            .end()
            .end()
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
                locals: vec![
                    ValType::nullable(contexts),
                    ValType::nullable(contexts),
                    ValType::I32,
                ],
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
            contexts,
            equals,
            hash_code,
            declarations: Default::default(),
            kinds: HashMap::default(),
            names: ReferenceNames::default(),
            accessors: PropertyAccessors::default(),
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

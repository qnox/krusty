//! Type-parameter serializer fields owned by a generated generic `$serializer`.

use super::{class_ty, KSERIALIZER_FQ};
use crate::ir::{ClassId, IrExpr, IrFile};
use crate::types::Ty;

/// The serializers a generic `$serializer` is constructed with, one per type parameter of the
/// class it serves. An element whose type is, or contains, one of those parameters reads it from
/// where the running code holds it: the `$serializer`'s own `this.typeSerial<k>`, or the
/// serialized class's `write$Self`, which receives them as trailing parameters.
#[derive(Clone, Copy)]
pub(super) struct TypeParameterSerializers<'a> {
    storage: TypeParameterSerializerStorage,
    /// The semantic identity of each type parameter, in declaration order.
    identities: &'a [&'static str],
}

/// Where code holds the type-parameter serializers it was given.
#[derive(Clone, Copy)]
enum TypeParameterSerializerStorage {
    /// Parameter `k`'s serializer is field `1 + k` of this `$serializer`; field 0 is the
    /// descriptor.
    SerializerFields(ClassId),
    /// Parameter `k`'s serializer is local `first + k` of a static helper.
    Parameters { first: u32 },
}

/// One type parameter's serializer, as the running code reads it.
#[derive(Clone, Copy)]
pub(super) enum TypeParameterSerializer {
    SerializerField {
        serializer_class: ClassId,
        field: u32,
    },
    Parameter(u32),
}

impl TypeParameterSerializer {
    pub(super) fn read(self, ir: &mut IrFile) -> crate::ir::ExprId {
        match self {
            TypeParameterSerializer::SerializerField {
                serializer_class,
                field,
            } => {
                let this = ir.add_expr(IrExpr::GetValue(0));
                ir.add_expr(IrExpr::GetField {
                    receiver: this,
                    class: serializer_class,
                    index: field,
                })
            }
            TypeParameterSerializer::Parameter(local) => ir.add_expr(IrExpr::GetValue(local)),
        }
    }
}

impl<'a> TypeParameterSerializers<'a> {
    /// No type parameter has a serializer: a non-generic class, or code that is not on the
    /// `$serializer` instance.
    pub(super) const NONE: TypeParameterSerializers<'static> = TypeParameterSerializers {
        storage: TypeParameterSerializerStorage::Parameters { first: 0 },
        identities: &[],
    };

    pub(super) fn new(
        serializer_class: ClassId,
        identities: &'a [&'static str],
    ) -> TypeParameterSerializers<'a> {
        TypeParameterSerializers {
            storage: TypeParameterSerializerStorage::SerializerFields(serializer_class),
            identities,
        }
    }

    /// The same serializers as a static helper receives them: parameter `k`'s is local
    /// `first + k`.
    pub(super) fn as_parameters(&self, first: u32) -> TypeParameterSerializers<'a> {
        TypeParameterSerializers {
            storage: TypeParameterSerializerStorage::Parameters { first },
            identities: self.identities,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.identities.len()
    }

    /// Where the serializer for the type parameter `identity` is held.
    pub(super) fn serializer(&self, identity: &str) -> Option<TypeParameterSerializer> {
        let ordinal = self
            .identities
            .iter()
            .position(|candidate| *candidate == identity)? as u32;
        Some(match self.storage {
            TypeParameterSerializerStorage::SerializerFields(serializer_class) => {
                TypeParameterSerializer::SerializerField {
                    serializer_class,
                    field: 1 + ordinal,
                }
            }
            TypeParameterSerializerStorage::Parameters { first } => {
                TypeParameterSerializer::Parameter(first + ordinal)
            }
        })
    }

    /// Every serializer, in type-parameter order.
    pub(super) fn reads(&self, ir: &mut IrFile) -> Vec<crate::ir::ExprId> {
        self.identities
            .iter()
            .map(|identity| {
                self.serializer(identity)
                    .expect("each identity has a serializer")
                    .read(ir)
            })
            .collect()
    }

    pub(super) fn install_member_body(&self, ir: &mut IrFile, function: u32) {
        let elements = self.reads(ir);
        let array = ir.add_expr(IrExpr::Vararg {
            array_type: Ty::obj_args("kotlin/Array", &[class_ty(KSERIALIZER_FQ)]),
            spreads: vec![false; elements.len()],
            elements,
        });
        let returned = ir.add_expr(IrExpr::Return(Some(array)));
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![returned],
            value: None,
        });
        super::complete_generated_body(ir, function, body);
    }
}

/// The semantic identities of the type parameters served by a generated serializer.
///
/// A generic class must have a signature. Continuing without it would leave its serializer fields
/// unaddressable and hide a broken frontend-to-IR contract behind source labels.
pub(super) fn identities(ir: &IrFile, serialized_class: ClassId) -> Vec<&'static str> {
    let class = &ir.classes[serialized_class as usize];
    if class.type_params.is_empty() {
        return Vec::new();
    }
    let signature = ir
        .class_signature_name(class.fq_name_id())
        .expect("a generic class records its generic signature");
    assert_eq!(
        signature.type_params.len(),
        class.type_params.len(),
        "a generic class's signature declares each of its type parameters"
    );
    signature
        .type_params
        .iter()
        .map(|parameter| crate::types::intern(&parameter.semantic_name))
        .collect()
}

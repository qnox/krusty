//! Type-parameter serializer fields owned by a generated generic `$serializer`.

use super::{class_ty, KSERIALIZER_FQ};
use crate::ir::{ClassId, IrExpr, IrFile};
use crate::types::Ty;

/// The serializers a generic `$serializer` is constructed with, one per type parameter of the
/// class it serves. An element whose type is, or contains, one of those parameters reads it off the
/// `$serializer` (`this.typeSerial<k>`), so only code running on that instance can use it.
#[derive(Clone, Copy)]
pub(super) struct TypeParameterSerializers<'a> {
    serializer_class: ClassId,
    /// The semantic identity of each type parameter, in declaration order. Parameter `k`'s
    /// serializer is field `1 + k`; field 0 is the descriptor.
    identities: &'a [&'static str],
}

impl<'a> TypeParameterSerializers<'a> {
    /// No type parameter has a serializer: a non-generic class, or code that is not on the
    /// `$serializer` instance.
    pub(super) const NONE: TypeParameterSerializers<'static> = TypeParameterSerializers {
        serializer_class: 0,
        identities: &[],
    };

    pub(super) fn new(
        serializer_class: ClassId,
        identities: &'a [&'static str],
    ) -> TypeParameterSerializers<'a> {
        TypeParameterSerializers {
            serializer_class,
            identities,
        }
    }

    /// The `$serializer` field holding the serializer for the type parameter `identity`.
    pub(super) fn field(&self, identity: &str) -> Option<u32> {
        self.identities
            .iter()
            .position(|candidate| *candidate == identity)
            .map(|ordinal| 1 + ordinal as u32)
    }

    pub(super) fn serializer_class(&self) -> ClassId {
        self.serializer_class
    }

    pub(super) fn install_member_body(&self, ir: &mut IrFile, function: u32) {
        let elements = (1..=self.identities.len() as u32)
            .map(|field| {
                let receiver = ir.add_expr(IrExpr::GetValue(0));
                ir.add_expr(IrExpr::GetField {
                    receiver,
                    class: self.serializer_class,
                    index: field,
                })
            })
            .collect::<Vec<_>>();
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
        ir.functions[function as usize].body = Some(body);
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

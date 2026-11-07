//! Provider-owned serialization of the normalized generic-member test declaration.

use super::{KlibBodyClassContract, KlibBodyClassParameters};
use crate::libraries::{
    KlibDeclarationSignature, KlibDependencyCallable, KotlinModality, TypeKind,
};
use crate::metadata::id_signature::{decode_public_id_signature, KlibPublicIdSignature};
use crate::metadata::protobuf::Pb;
use crate::types::{type_name, Ty, TypeVariance};

pub(crate) fn generic_member_declaration() -> (KlibBodyClassContract, KlibDependencyCallable) {
    let owner = type_name("sample/Owner");
    let class_formal = Ty::ty_param("sample.Owner.T", Ty::nullable(Ty::obj("kotlin/Any")));
    let mut common = Pb::new();
    common.field_varint(1, 0);
    common.field_varint(2, 1);
    common.field_varint(2, 2);
    common.field_fixed64(6, 71);
    let mut wrapper = Pb::new();
    wrapper.field_message(1, &common);
    let method = decode_public_id_signature(
        wrapper.as_bytes(),
        &["sample".into(), "Owner".into(), "echo".into()],
    )
    .unwrap()
    .unwrap();
    let callable = KlibDependencyCallable {
        module: "sample".into(),
        declaration: KlibDeclarationSignature::Public(method),
    };
    let class = KlibBodyClassContract {
        module: "sample".into(),
        signature: KlibPublicIdSignature::classifier(owner),
        classifier: owner,
        parameters: KlibBodyClassParameters {
            formals: vec![class_formal].into_boxed_slice(),
            bounds: vec![Vec::new()].into_boxed_slice(),
            variances: vec![TypeVariance::Invariant].into_boxed_slice(),
            own_count: 1,
        },
        kind: TypeKind::Class,
        modality: KotlinModality::Open,
        is_value: false,
        is_inner: false,
        is_expect: false,
        primary_constructor: None,
    };
    (class, callable)
}

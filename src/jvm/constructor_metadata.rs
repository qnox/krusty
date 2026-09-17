//! Kotlin-metadata publication for secondary constructors.
//!
//! Common IR decides whether a constructor is a Kotlin declaration and records its semantic
//! visibility. This JVM boundary supplies descriptors and protobuf flag encoding without inferring
//! publication from a synthetic access flag, parameter spelling, or physical constructor shape.

use crate::ir::{AppliedAnnotation, IrClass, IrFile, IrSecondaryCtor};
use crate::jvm::{ir_emit::jvm_tys, names::type_descriptor};
use crate::types::{Ty, Visibility};

pub(super) struct SecondaryConstructorMetadataShape {
    pub(super) params: Vec<(String, Ty)>,
    pub(super) param_defaults: Vec<bool>,
    pub(super) descriptor: String,
    pub(super) signature_name: Option<&'static str>,
    pub(super) vararg_index: Option<usize>,
    pub(super) flags: u64,
    pub(super) annotations: Vec<AppliedAnnotation>,
}

pub(super) fn secondary_constructor_shapes(
    ir: &IrFile,
    class: &IrClass,
) -> Vec<SecondaryConstructorMetadataShape> {
    let mut shapes = class
        .secondary_ctors
        .iter()
        .enumerate()
        .filter_map(|(ordinal, constructor)| {
            secondary_constructor_shape(ir, class, ordinal, constructor)
        })
        .collect::<Vec<_>>();
    shapes.extend(
        ir.jvm_value_class_secondary_ctors
            .get(&class.fq_name_id())
            .into_iter()
            .flatten()
            .map(|constructor| SecondaryConstructorMetadataShape {
                params: constructor.params.clone(),
                param_defaults: constructor.param_defaults.clone(),
                descriptor: constructor.descriptor.clone(),
                signature_name: Some("constructor-impl"),
                vararg_index: constructor.vararg_index,
                flags: secondary_constructor_flags(constructor.metadata_visibility),
                annotations: constructor.annotations.applications().cloned().collect(),
            }),
    );
    shapes
}

fn secondary_constructor_shape(
    ir: &IrFile,
    class: &IrClass,
    ordinal: usize,
    constructor: &IrSecondaryCtor,
) -> Option<SecondaryConstructorMetadataShape> {
    let visibility = constructor.metadata_visibility?;
    let mut params = constructor.named_params.clone();
    let physical_prefix_params = jvm_tys(&constructor.prefix_params);
    let physical_params = jvm_tys(&constructor.params);
    let serialization_constructor = ir.generated_secondary_constructor_by_owner(
        class.fq_name_id(),
        crate::ir::IrSecondaryConstructorRole::SerializationDeserialization,
    );
    if serialization_constructor.and_then(|recorded| usize::try_from(recorded).ok())
        == Some(ordinal)
    {
        assert_eq!(
            params.len(),
            physical_params.len(),
            "serialization constructor metadata exactly matches its physical parameter contract"
        );
        for ((_, semantic), &physical) in params.iter_mut().zip(&physical_params) {
            if physical.is_reference() {
                *semantic = Ty::nullable(*semantic);
            }
        }
    }
    Some(SecondaryConstructorMetadataShape {
        params,
        param_defaults: constructor.defaults.iter().map(Option::is_some).collect(),
        descriptor: format!(
            "({}{}{})V",
            physical_prefix_params
                .iter()
                .map(|&ty| type_descriptor(ty))
                .collect::<String>(),
            physical_params
                .iter()
                .map(|&ty| type_descriptor(ty))
                .collect::<String>(),
            if constructor.vc_params {
                "Lkotlin/jvm/internal/DefaultConstructorMarker;"
            } else {
                ""
            }
        ),
        signature_name: None,
        vararg_index: constructor.vararg_index,
        flags: secondary_constructor_flags(visibility),
        annotations: constructor.annotations.applications().cloned().collect(),
    })
}

fn secondary_constructor_flags(visibility: Visibility) -> u64 {
    const IS_SECONDARY: u64 = 16;
    let visibility = match visibility {
        Visibility::Internal => 0,
        Visibility::Private => 2,
        Visibility::Protected => 4,
        Visibility::Public => 6,
        Visibility::PackagePrivate => {
            unreachable!("package-private is never published in Kotlin metadata")
        }
    };
    IS_SECONDARY | visibility
}

#[cfg(test)]
mod tests {
    use super::{secondary_constructor_flags, secondary_constructor_shape};
    use crate::ir::{
        CtorDelegateTarget, DeclarationAnnotations, IrFile, IrSecondaryConstructorRole,
        IrSecondaryCtor,
    };
    use crate::plugins::synthetic_class;
    use crate::types::{type_name, Ty, Visibility};

    fn constructor(metadata_visibility: Option<Visibility>) -> IrSecondaryCtor {
        IrSecondaryCtor {
            annotations: DeclarationAnnotations::default(),
            source_order: u32::MAX,
            prefix_params: vec![Ty::Boolean],
            params: vec![Ty::Int, Ty::obj("kotlin/String")],
            named_params: vec![
                ("seen0".to_string(), Ty::Int),
                ("value".to_string(), Ty::obj("kotlin/String")),
            ],
            metadata_visibility,
            generated_debug: crate::ir::IrGeneratedDeclarationDebug::None,
            vararg_index: Some(1),
            defaults: vec![Some(0), None],
            delegate_prelude: Vec::new(),
            delegate_args: Vec::new(),
            default_parameters: Vec::new(),
            body: None,
            delegate: CtorDelegateTarget::Super {
                owner: type_name("kotlin/Any"),
                target_params: Vec::new(),
                default_masks: Vec::new(),
            },
            synthetic: true,
            vc_params: false,
        }
    }

    #[test]
    fn publication_is_explicit_not_inferred_from_names_or_synthetic_shape() {
        let ir = IrFile::default();
        let class = synthetic_class("sample/Owner");
        let mut unpublished = constructor(None);
        unpublished.synthetic = false;
        assert!(
            secondary_constructor_shape(&ir, &class, 0, &unpublished).is_none(),
            "metadata payload and a non-synthetic classfile method do not publish a constructor"
        );

        let mut empty_published = constructor(Some(Visibility::Internal));
        empty_published.named_params.clear();
        assert!(
            secondary_constructor_shape(&ir, &class, 0, &empty_published).is_some(),
            "an explicit zero-parameter publication does not need a spelling sentinel"
        );

        let source = constructor(Some(Visibility::Internal));
        let published = secondary_constructor_shape(&ir, &class, 0, &source)
            .expect("explicitly published constructor");
        assert_eq!(published.params, source.named_params);
        assert_eq!(published.param_defaults, [true, false]);
        assert_eq!(published.descriptor, "(ZILjava/lang/String;)V");
        assert_eq!(published.signature_name, None);
        assert_eq!(published.vararg_index, Some(1));
        assert_eq!(published.flags, 16);
        assert!(published.annotations.is_empty());
    }

    #[test]
    fn serialization_projection_uses_exact_role_and_physical_jvm_parameters() {
        let mut ir = IrFile::default();
        let class_id = ir.add_class(synthetic_class("sample/Owner"));
        let mut generated = constructor(Some(Visibility::Internal));
        generated.prefix_params.clear();
        generated.params = vec![
            Ty::Int,
            Ty::obj("kotlin/String"),
            Ty::Unit,
            Ty::ty_param("T", Ty::obj("kotlin/Any")),
            Ty::Long,
        ];
        generated.named_params = vec![
            ("primitive".to_string(), Ty::Int),
            ("reference".to_string(), Ty::obj("kotlin/String")),
            ("unit".to_string(), Ty::Unit),
            (
                "generic".to_string(),
                Ty::ty_param("T", Ty::obj("kotlin/Any")),
            ),
            ("value".to_string(), Ty::obj("sample/Value")),
        ];
        ir.classes[class_id as usize]
            .secondary_ctors
            .push(generated);
        ir.record_generated_secondary_constructor(
            class_id,
            IrSecondaryConstructorRole::SerializationDeserialization,
            0,
        );

        let shape = secondary_constructor_shape(
            &ir,
            &ir.classes[class_id as usize],
            0,
            &ir.classes[class_id as usize].secondary_ctors[0],
        )
        .expect("published serialization constructor");
        assert_eq!(
            shape.params,
            vec![
                ("primitive".to_string(), Ty::Int),
                (
                    "reference".to_string(),
                    Ty::nullable(Ty::obj("kotlin/String")),
                ),
                ("unit".to_string(), Ty::nullable(Ty::Unit)),
                (
                    "generic".to_string(),
                    Ty::nullable(Ty::ty_param("T", Ty::obj("kotlin/Any"))),
                ),
                ("value".to_string(), Ty::obj("sample/Value")),
            ]
        );
        assert_eq!(
            shape.descriptor,
            "(ILjava/lang/String;Lkotlin/Unit;Ljava/lang/Object;J)V"
        );

        let unrelated = constructor(Some(Visibility::Internal));
        let unrelated_shape =
            secondary_constructor_shape(&ir, &ir.classes[class_id as usize], 1, &unrelated)
                .expect("ordinary published constructor");
        assert_eq!(unrelated_shape.params, unrelated.named_params);
    }

    #[test]
    fn secondary_constructor_flags_encode_exact_semantic_visibility() {
        assert_eq!(secondary_constructor_flags(Visibility::Internal), 16);
        assert_eq!(secondary_constructor_flags(Visibility::Private), 18);
        assert_eq!(secondary_constructor_flags(Visibility::Protected), 20);
        assert_eq!(secondary_constructor_flags(Visibility::Public), 22);
    }
}

//! Frontend publication of serialization's generated nested classifier.

use crate::plugins::FrontendClassContext;
use crate::types::{type_name, GeneratedClassifierFact, GeneratedClassifierKind, Visibility};

use super::{SERIALIZABLE_FQ, SERIALIZER_OBJECT_NAME};

/// Whether `@Serializable` is among `annotations` WITHOUT a custom serializer: the plugin, not a
/// user `KSerializer`, then serializes the class. A custom serializer is `@Serializable`'s explicit
/// class-literal argument (`with = S::class`).
pub(super) fn serializable_by_plugin(
    annotations: &[crate::types::TypeName],
    annotation_class_arguments: &[(u32, crate::types::TypeName)],
) -> bool {
    let Some(serializable_ordinal) = annotations
        .iter()
        .position(|annotation| *annotation == type_name(SERIALIZABLE_FQ))
    else {
        return false;
    };
    !annotation_class_arguments
        .iter()
        .any(|(ordinal, _)| *ordinal as usize == serializable_ordinal)
}

pub(super) fn generated_serializer_classifier_fact(
    ctx: &FrontendClassContext<'_>,
) -> Option<GeneratedClassifierFact> {
    // An enum, a sealed base and an object build their serializer at run time (`EnumSerializer`,
    // `SealedClassSerializer`, `ObjectSerializer`); only an ordinary class gets a `$serializer`.
    if !serializable_by_plugin(ctx.annotations, ctx.annotation_class_arguments)
        || matches!(
            ctx.kind,
            crate::libraries::TypeKind::Enum | crate::libraries::TypeKind::Object
        )
        || ctx.is_sealed
    {
        return None;
    }
    Some(GeneratedClassifierFact {
        classifier: ctx.classifier.nested_child(SERIALIZER_OBJECT_NAME),
        lexical_owner: ctx.classifier,
        purpose: crate::types::GeneratedClassifierPurpose::SerializationSerializer,
        source_name: SERIALIZER_OBJECT_NAME.into(),
        visibility: Visibility::Public,
        kind: GeneratedClassifierKind::Class,
        is_abstract: false,
        is_final: true,
        captures_outer: false,
        compiler_generated: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{FrontendCallable, FrontendCallableOwner, IrPlugin};
    use crate::types::Ty;

    use super::super::{SerializationPlugin, KSERIALIZER_FQ};

    #[test]
    fn contributes_serializer_as_a_companion_member() {
        let annotation = [type_name(SERIALIZABLE_FQ)];
        let classifier = type_name("demo/Box");
        let mut members = Vec::new();
        SerializationPlugin::default().generate_frontend_declarations(
            &FrontendClassContext {
                classifier,
                companion: None,
                kind: crate::libraries::TypeKind::Class,
                is_sealed: false,
                type_parameters: &crate::types::TypeParameters::new(
                    vec!["T".to_string()],
                    vec![Ty::nullable(Ty::obj("kotlin/Any"))],
                    vec![crate::types::TypeVariance::Invariant],
                ),
                annotations: &annotation,
                annotation_class_arguments: &[],
            },
            &mut members,
        );

        assert_eq!(
            members,
            vec![FrontendCallable {
                owner: FrontendCallableOwner::Companion,
                name: "serializer".to_string(),
                params: vec![Ty::obj_args(
                    KSERIALIZER_FQ,
                    &[Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")))],
                )],
                param_names: vec!["typeSerial0".to_string()],
                ret: Ty::obj_args_name(
                    type_name(KSERIALIZER_FQ),
                    &[Ty::obj_args_name(
                        classifier,
                        &[Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")))],
                    )],
                ),
                generic_sig: Some(crate::libraries::GenericSig {
                    formals: vec!["T".to_string()],
                    formal_bounds: vec![vec![Ty::nullable(Ty::obj("kotlin/Any"))]],
                    receiver: None,
                    params: vec![Ty::obj_args(
                        KSERIALIZER_FQ,
                        &[Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")))],
                    )],
                    ret: Ty::obj_args_name(
                        type_name(KSERIALIZER_FQ),
                        &[Ty::obj_args_name(
                            classifier,
                            &[Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")))],
                        )],
                    ),
                    return_policy: crate::libraries::GenericReturnPolicy::Exact,
                }),
                plugin_expression: Some(crate::libraries::PluginExpressionDeclaration {
                    plugin: "serialization",
                    operation: "serializer",
                }),
            }]
        );
    }

    #[test]
    fn contributes_object_serializer_as_an_object_member() {
        let annotation = [type_name(SERIALIZABLE_FQ)];
        let mut members = Vec::new();
        SerializationPlugin::default().generate_frontend_declarations(
            &FrontendClassContext {
                classifier: type_name("demo/Singleton"),
                companion: None,
                kind: crate::libraries::TypeKind::Object,
                is_sealed: false,
                type_parameters: &crate::types::TypeParameters::default(),
                annotations: &annotation,
                annotation_class_arguments: &[],
            },
            &mut members,
        );

        assert_eq!(members.len(), 1);
        assert_eq!(members[0].owner, FrontendCallableOwner::Classifier);
    }

    #[test]
    fn publishes_the_exact_generated_serializer_classifier_fact() {
        let annotation = [type_name(SERIALIZABLE_FQ)];
        let classifier = type_name("demo/Box");
        let parameters = crate::types::TypeParameters::default();
        let context = FrontendClassContext {
            classifier,
            companion: None,
            kind: crate::libraries::TypeKind::Class,
            is_sealed: false,
            type_parameters: &parameters,
            annotations: &annotation,
            annotation_class_arguments: &[],
        };

        assert_eq!(
            generated_serializer_classifier_fact(&context),
            Some(GeneratedClassifierFact {
                classifier: classifier.nested_child(SERIALIZER_OBJECT_NAME),
                lexical_owner: classifier,
                purpose: crate::types::GeneratedClassifierPurpose::SerializationSerializer,
                source_name: SERIALIZER_OBJECT_NAME.into(),
                visibility: Visibility::Public,
                kind: GeneratedClassifierKind::Class,
                is_abstract: false,
                is_final: true,
                captures_outer: false,
                compiler_generated: true,
            })
        );
    }

    #[test]
    fn rejects_non_generating_serializable_forms() {
        let annotation = [type_name(SERIALIZABLE_FQ)];
        let custom = [(0, type_name("demo/OwnSerializer"))];
        let parameters = crate::types::TypeParameters::default();
        for (kind, is_sealed, arguments) in [
            (crate::libraries::TypeKind::Class, false, custom.as_slice()),
            (crate::libraries::TypeKind::Enum, false, &[][..]),
            (crate::libraries::TypeKind::Class, true, &[][..]),
        ] {
            let context = FrontendClassContext {
                classifier: type_name("demo/NotGenerated"),
                companion: None,
                kind,
                is_sealed,
                type_parameters: &parameters,
                annotations: &annotation,
                annotation_class_arguments: arguments,
            };
            assert_eq!(
                generated_serializer_classifier_fact(&context),
                None,
                "{kind:?}, sealed={is_sealed}, arguments={arguments:?}"
            );
        }
    }
}

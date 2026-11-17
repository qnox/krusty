//! `@file:UseSerializers(S::class, ...)`: serializers a file applies to every property type they
//! serve. The annotation targets files only.
//!
//! A serializer serves the type its `KSerializer<T>` supertype names. The annotation makes every occurrence of `T` or `T?` inside a serializable
//! property's declared type (the type itself, or a type argument at any depth) use that serializer,
//! as if the occurrence had been written `@Serializable(with = S::class) T`. This module rewrites
//! the property's checked declared spelling that way, so the element-serializer derivation, which
//! already honours a type-use `@Serializable(with = ...)`, is the one place that selects and
//! constructs it. A serializer the type itself names, and an explicit `@Contextual` property, keep
//! precedence.

use crate::ir::{AnnoValue, IrFile, Spelled, TypeUseAnnotation};
use crate::types::{
    type_name, AnnotationSemanticFacts, AnnotationValue, ResolvedAnnotation, Ty, TypeName,
};

use super::{KSERIALIZER_FQ, SERIALIZABLE_FQ};

const USE_SERIALIZERS_FQ: &str = "kotlinx/serialization/UseSerializers";

/// One serializer the file applies, with the type it serves.
#[derive(Clone, Copy)]
pub(super) struct UsedSerializer {
    serializer: TypeName,
    served: Ty,
}

/// The serializers the file names through `@UseSerializers`, in the order it lists them. A
/// serializer this compilation does not declare, or one that has no `KSerializer<T>` supertype,
/// serves nothing here.
pub(super) fn used_serializers(ir: &IrFile) -> Vec<UsedSerializer> {
    let use_serializers = type_name(USE_SERIALIZERS_FQ);
    ir.file_annotations
        .applications()
        .filter(|application| application.internal == use_serializers)
        .flat_map(|application| &application.values)
        .flat_map(|(_, value)| serializer_classes(value))
        .filter_map(|serializer| {
            let declaration = ir
                .classes
                .iter()
                .find(|class| class.fq_name_id() == serializer)?;
            let kserializer = type_name(KSERIALIZER_FQ);
            let served = declaration
                .supertypes
                .iter()
                .find(|supertype| supertype.kotlin_class_internal() == Some(kserializer))?
                .type_args()
                .first()
                .copied()?;
            Some(UsedSerializer { serializer, served })
        })
        .collect()
}

fn serializer_classes(value: &AnnoValue) -> Vec<TypeName> {
    match value {
        AnnoValue::Class(ty) => ty.kotlin_class_internal().into_iter().collect(),
        AnnoValue::Array(values) => values.iter().flat_map(serializer_classes).collect(),
        _ => Vec::new(),
    }
}

/// Record `@Serializable(with = S::class)` on every node of `spelled` whose type `ty` an applied
/// serializer serves and whose own type-use annotations name no serializer. `contextual` withholds
/// the root: an explicitly `@Contextual` property is serialized contextually.
pub(super) fn apply(spelled: &mut Spelled, ty: Ty, used: &[UsedSerializer], contextual: bool) {
    apply_node(spelled, ty, used, contextual);
}

fn apply_node(spelled: &mut Spelled, ty: Ty, used: &[UsedSerializer], withheld: bool) {
    let arguments = ty.non_null().type_args();
    if spelled.args.len() < arguments.len() {
        spelled.args.resize_with(arguments.len(), Spelled::default);
    }
    for (node, argument) in spelled.args.iter_mut().zip(arguments) {
        let argument = match argument.kind() {
            crate::types::TyKind::OutProjection(inner)
            | crate::types::TyKind::InProjection(inner) => inner,
            _ => *argument,
        };
        apply_node(node, argument, used, false);
    }
    if withheld || names_serializer(spelled) {
        return;
    }
    if let Some(applied) = used
        .iter()
        .find(|used| used.served.non_null() == ty.non_null())
    {
        spelled
            .annotations
            .push(TypeUseAnnotation::Checked(ResolvedAnnotation {
                annotation: type_name(SERIALIZABLE_FQ),
                arguments: vec![(
                    "with".to_string(),
                    AnnotationValue::Class(Ty::obj_name(applied.serializer)),
                )],
                facts: AnnotationSemanticFacts::default(),
                written: None,
            }));
    }
}

fn names_serializer(spelled: &Spelled) -> bool {
    super::type_argument_serializers::named_serializer(spelled).is_some()
}

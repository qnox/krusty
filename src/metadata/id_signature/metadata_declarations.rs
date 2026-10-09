//! Public `IdSignature`s of the declarations a KLIB's metadata describes.
//!
//! Metadata and serialized IR are two views of one KLIB. Each metadata declaration is mangled
//! exactly as the IR serializer mangled the same declaration, so a provider joins the two by
//! identity alone.

use std::borrow::Cow;

use super::mangling::{
    callable_signature, class_signature, property_signature, CallableShape, ClassScope,
    DeclarationContainer, ManglingError, PropertyShape, SignatureType, TypeParameterShape,
    TypeView,
};
use super::KlibPublicIdSignature;
use crate::metadata::semantic::{
    KotlinConstructor, KotlinFunction, KotlinMember, KotlinProperty, KotlinType,
    KotlinTypeParameter,
};

impl SignatureType for KotlinType {
    fn view(&self) -> Result<TypeView<'_, Self>, ManglingError> {
        Ok(match self {
            KotlinType::Class {
                internal,
                args,
                nullable,
                shape,
            } if shape.suspend => {
                // Metadata spells `suspend (A) -> R` as `Function2<A, Continuation<R>, Any?>`; IR
                // spells it `SuspendFunction1<A, R>`.
                let malformed = || {
                    ManglingError::new(format!(
                        "suspend function type {internal} has no continuation"
                    ))
                };
                let [parameters @ .., continuation, _] = args.as_slice() else {
                    return Err(malformed());
                };
                let KotlinType::Class {
                    args: continuation_args,
                    ..
                } = continuation
                else {
                    return Err(malformed());
                };
                let [result] = continuation_args.as_slice() else {
                    return Err(malformed());
                };
                TypeView::Class {
                    fq_name: Cow::Owned(format!(
                        "kotlin.coroutines.SuspendFunction{}",
                        parameters.len()
                    )),
                    arguments: parameters.iter().chain([result]).collect(),
                    nullable: *nullable,
                }
            }
            KotlinType::Class {
                internal,
                args,
                nullable,
                ..
            } => TypeView::Class {
                fq_name: Cow::Owned(internal.replace('/', ".")),
                arguments: args.iter().collect(),
                nullable: *nullable,
            },
            KotlinType::Param { name, nullable } => TypeView::Parameter {
                name,
                nullable: *nullable,
            },
            KotlinType::InProjection(inner) => TypeView::In(inner),
            KotlinType::OutProjection(inner) => TypeView::Out(inner),
            KotlinType::Star => TypeView::Star,
        })
    }
}

/// Where a metadata declaration sits: its package and the classes around it, outermost first,
/// each with its declared type parameters.
#[derive(Clone, Copy)]
pub struct MetadataContainer<'a> {
    pub package: &'a [String],
    pub classes: &'a [(&'a str, &'a [KotlinTypeParameter])],
}

impl MetadataContainer<'_> {
    fn scopes(&self) -> Vec<ClassScope<'_, KotlinType>> {
        self.classes
            .iter()
            .map(|(name, parameters)| ClassScope {
                name,
                type_parameters: type_parameters(parameters),
            })
            .collect()
    }
}

fn type_parameters(parameters: &[KotlinTypeParameter]) -> Vec<TypeParameterShape<'_, KotlinType>> {
    parameters
        .iter()
        .map(|parameter| TypeParameterShape {
            name: &parameter.name,
            bounds: parameter.bounds.iter().collect(),
        })
        .collect()
}

fn with_container<R>(
    container: MetadataContainer<'_>,
    build: impl FnOnce(DeclarationContainer<'_, KotlinType>) -> R,
) -> R {
    let classes = container.scopes();
    build(DeclarationContainer {
        package: container.package,
        classes: &classes,
    })
}

/// The identity of a class; `container` holds the classes around it, not the class itself.
pub fn metadata_class_signature(
    container: MetadataContainer<'_>,
    name: &str,
) -> KlibPublicIdSignature {
    with_container(container, |container| class_signature(container, name))
}

/// A top-level function's identity.
pub fn package_function_signature(
    container: MetadataContainer<'_>,
    function: &KotlinFunction,
) -> Result<KlibPublicIdSignature, ManglingError> {
    let contexts = function
        .params
        .get(..function.context_count)
        .ok_or_else(|| {
            ManglingError::new(format!(
                "function {} has {} context parameters but {} parameters",
                function.name,
                function.context_count,
                function.params.len()
            ))
        })?;
    let shape = CallableShape {
        name: &function.name,
        contexts: contexts.iter().collect(),
        receiver: function.receiver.as_ref(),
        params: function.params[function.context_count..].iter().collect(),
        vararg: function.vararg.map(|index| index - function.context_count),
        type_parameters: type_parameters(&function.formals),
    };
    with_container(container, |container| callable_signature(container, &shape))
}

/// A top-level property's identity.
pub fn package_property_signature(
    container: MetadataContainer<'_>,
    property: &KotlinProperty,
) -> Result<KlibPublicIdSignature, ManglingError> {
    let shape = PropertyShape {
        name: &property.name,
        contexts: property.context_params.iter().collect(),
        receiver: property.receiver.as_ref(),
        type_parameters: type_parameters(&property.formals),
    };
    with_container(container, |container| property_signature(container, &shape))
}

/// A class member's identity; `container` ends with the member's class.
pub fn member_signature(
    container: MetadataContainer<'_>,
    member: &KotlinMember,
) -> Result<KlibPublicIdSignature, ManglingError> {
    with_container(container, |container| {
        if member.is_property {
            property_signature(
                container,
                &PropertyShape {
                    name: &member.name,
                    contexts: member.context_params.iter().collect(),
                    receiver: member.receiver.as_ref(),
                    type_parameters: type_parameters(&member.formals),
                },
            )
        } else {
            callable_signature(
                container,
                &CallableShape {
                    name: &member.name,
                    contexts: member.context_params.iter().collect(),
                    receiver: member.receiver.as_ref(),
                    params: member.params.iter().collect(),
                    vararg: member.vararg,
                    type_parameters: type_parameters(&member.formals),
                },
            )
        }
    })
}

/// A constructor's identity; `container` ends with the constructed class.
pub fn constructor_signature(
    container: MetadataContainer<'_>,
    constructor: &KotlinConstructor,
) -> Result<KlibPublicIdSignature, ManglingError> {
    let shape = CallableShape {
        name: "<init>",
        contexts: Vec::new(),
        receiver: None,
        params: constructor.params.iter().collect(),
        vararg: constructor.vararg,
        type_parameters: Vec::new(),
    };
    with_container(container, |container| callable_signature(container, &shape))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::semantic::KotlinFunctionTypeShape;
    use crate::types::{ReturnValueStatus, TypeVariance, Visibility};

    // Every expected member id below is the one the Kotlin/Native 2.4.20 stdlib KLIB serializes for
    // the named declaration.

    fn class(internal: &str, args: Vec<KotlinType>) -> KotlinType {
        KotlinType::Class {
            internal: internal.to_owned(),
            args,
            nullable: false,
            shape: KotlinFunctionTypeShape::default(),
        }
    }

    fn nullable(ty: KotlinType) -> KotlinType {
        match ty {
            KotlinType::Class {
                internal,
                args,
                shape,
                ..
            } => KotlinType::Class {
                internal,
                args,
                nullable: true,
                shape,
            },
            KotlinType::Param { name, .. } => KotlinType::Param {
                name,
                nullable: true,
            },
            other => other,
        }
    }

    fn param(name: &str) -> KotlinType {
        KotlinType::Param {
            name: name.to_owned(),
            nullable: false,
        }
    }

    fn type_parameter(name: &str, bounds: Vec<KotlinType>) -> KotlinTypeParameter {
        KotlinTypeParameter {
            name: name.to_owned(),
            bounds,
            variance: TypeVariance::Invariant,
            only_input: false,
            reified: false,
        }
    }

    fn function(
        name: &str,
        receiver: Option<KotlinType>,
        params: Vec<KotlinType>,
        formals: Vec<KotlinTypeParameter>,
    ) -> KotlinFunction {
        KotlinFunction {
            name: name.to_owned(),
            receiver,
            param_names: vec![String::new(); params.len()],
            param_defaults: vec![false; params.len()],
            params,
            ret: class("kotlin/Unit", Vec::new()),
            formals,
            vararg: None,
            visibility: Visibility::Public,
            is_inline: false,
            has_reified_type_params: false,
            is_suspend: false,
            is_operator: false,
            is_infix: false,
            context_count: 0,
            annotations: Vec::new(),
        }
    }

    fn member(name: &str, params: Vec<KotlinType>, is_property: bool) -> KotlinMember {
        KotlinMember {
            name: name.to_owned(),
            receiver: None,
            context_params: Vec::new(),
            visibility: Visibility::Public,
            param_names: vec![String::new(); params.len()],
            param_defaults: vec![false; params.len()],
            params,
            ret: class("kotlin/Unit", Vec::new()),
            is_property,
            is_operator: false,
            is_infix: false,
            is_abstract: false,
            return_value_status: ReturnValueStatus::default(),
            formals: Vec::new(),
            ret_nullable: false,
            constant: None,
            vararg: None,
            annotations: Vec::new(),
        }
    }

    fn package(segments: &[&str]) -> Vec<String> {
        segments
            .iter()
            .map(|segment| (*segment).to_owned())
            .collect()
    }

    fn top_level(package: &[String]) -> MetadataContainer<'_> {
        MetadataContainer {
            package,
            classes: &[],
        }
    }

    fn member_id(signature: &KlibPublicIdSignature) -> i64 {
        signature.member_id().expect("a callable has a member id") as i64
    }

    #[test]
    fn a_function_without_a_result_or_type_parameters() {
        let kotlin_io = package(&["kotlin", "io"]);
        let println = function(
            "println",
            None,
            vec![nullable(class("kotlin/Any", Vec::new()))],
            Vec::new(),
        );
        let signature = package_function_signature(top_level(&kotlin_io), &println).unwrap();
        assert_eq!(signature.package().segments(), ["kotlin", "io"]);
        assert_eq!(signature.declaration().segments(), ["println"]);
        assert_eq!(member_id(&signature), -3_363_048_611_743_956_379);
    }

    #[test]
    fn an_extension_function() {
        let kotlin_ranges = package(&["kotlin", "ranges"]);
        let int = class("kotlin/Int", Vec::new());
        let coerce = function("coerceAtLeast", Some(int.clone()), vec![int], Vec::new());
        let signature = package_function_signature(top_level(&kotlin_ranges), &coerce).unwrap();
        assert_eq!(member_id(&signature), 3_998_717_805_095_061_419);
    }

    #[test]
    fn type_parameters_are_referenced_by_container_and_index() {
        let kotlin_comparisons = package(&["kotlin", "comparisons"]);
        let max_of = function(
            "maxOf",
            None,
            vec![param("T"), param("T")],
            vec![type_parameter(
                "T",
                vec![class("kotlin/Comparable", vec![param("T")])],
            )],
        );
        let signature =
            package_function_signature(top_level(&kotlin_comparisons), &max_of).unwrap();
        assert_eq!(member_id(&signature), -5_030_133_889_946_118_250);
    }

    #[test]
    fn a_star_projection_and_an_unbounded_type_parameter() {
        let kotlin_sequences = package(&["kotlin", "sequences"]);
        let filter = function(
            "filterIsInstance",
            Some(class("kotlin/sequences/Sequence", vec![KotlinType::Star])),
            Vec::new(),
            vec![type_parameter("R", Vec::new())],
        );
        let signature = package_function_signature(top_level(&kotlin_sequences), &filter).unwrap();
        assert_eq!(member_id(&signature), 298_316_304_477_115_805);
    }

    #[test]
    fn a_suspend_function_type_is_its_suspend_function_class() {
        let kotlin_coroutines = package(&["kotlin", "coroutines"]);
        let continuation = class("kotlin/coroutines/Continuation", vec![param("T")]);
        let block = KotlinType::Class {
            internal: "kotlin/Function1".to_owned(),
            args: vec![
                continuation.clone(),
                nullable(class("kotlin/Any", Vec::new())),
            ],
            nullable: false,
            shape: KotlinFunctionTypeShape {
                suspend: true,
                ..KotlinFunctionTypeShape::default()
            },
        };
        let create = function(
            "createCoroutine",
            Some(block),
            vec![continuation],
            vec![type_parameter("T", Vec::new())],
        );
        let signature = package_function_signature(top_level(&kotlin_coroutines), &create).unwrap();
        assert_eq!(member_id(&signature), -5_059_621_468_224_635_510);
    }

    #[test]
    fn context_parameters_come_first() {
        let kotlin = package(&["kotlin"]);
        let mut context_of = function(
            "contextOf",
            None,
            vec![param("A")],
            vec![type_parameter("A", Vec::new())],
        );
        context_of.context_count = 1;
        let signature = package_function_signature(top_level(&kotlin), &context_of).unwrap();
        assert_eq!(member_id(&signature), -12_594_581_380_294_987);
    }

    #[test]
    fn an_extension_property_with_a_type_parameter() {
        let kotlin_collections = package(&["kotlin", "collections"]);
        let last_index = KotlinProperty {
            name: "lastIndex".to_owned(),
            receiver: Some(class("kotlin/collections/List", vec![param("T")])),
            context_params: Vec::new(),
            ty: class("kotlin/Int", Vec::new()),
            formals: vec![type_parameter("T", Vec::new())],
            visibility: Visibility::Public,
            is_var: false,
            context_count: 0,
            constant: None,
        };
        let signature =
            package_property_signature(top_level(&kotlin_collections), &last_index).unwrap();
        assert_eq!(member_id(&signature), -7_238_914_123_027_933_299);
    }

    #[test]
    fn class_members_see_the_class_type_parameters_as_the_next_container() {
        let kotlin = package(&["kotlin"]);
        let parameters = [
            type_parameter("A", Vec::new()),
            type_parameter("B", Vec::new()),
        ];
        let classes = [("Pair", &parameters[..])];
        let pair = MetadataContainer {
            package: &kotlin,
            classes: &classes,
        };
        let class_identity = metadata_class_signature(top_level(&kotlin), "Pair");
        assert_eq!(class_identity.declaration().segments(), ["Pair"]);
        assert_eq!(class_identity.member_id(), None);

        let constructor = constructor_signature(
            pair,
            &KotlinConstructor {
                params: vec![param("A"), param("B")],
                param_names: vec![String::new(); 2],
                param_defaults: vec![false; 2],
                vararg: None,
                visibility: Visibility::Public,
            },
        )
        .unwrap();
        assert_eq!(constructor.declaration().segments(), ["Pair", "<init>"]);
        assert_eq!(member_id(&constructor), 3_086_114_026_882_374_588);

        let first = member_signature(pair, &member("first", Vec::new(), true)).unwrap();
        assert_eq!(first.declaration().segments(), ["Pair", "first"]);
        assert_eq!(member_id(&first), 1_497_393_077_339_299_626);
    }

    #[test]
    fn a_member_function_references_its_class_type_parameter() {
        let kotlin_collections = package(&["kotlin", "collections"]);
        let parameters = [type_parameter("E", Vec::new())];
        let classes = [("ArrayList", &parameters[..])];
        let array_list = MetadataContainer {
            package: &kotlin_collections,
            classes: &classes,
        };
        let add = member_signature(array_list, &member("add", vec![param("E")], false)).unwrap();
        assert_eq!(add.declaration().segments(), ["ArrayList", "add"]);
        assert_eq!(member_id(&add), 4_996_012_557_724_047_373);
    }

    #[test]
    fn an_extension_property_without_type_parameters() {
        let kotlin_text = package(&["kotlin", "text"]);
        let last_index = KotlinProperty {
            name: "lastIndex".to_owned(),
            receiver: Some(class("kotlin/CharSequence", Vec::new())),
            context_params: Vec::new(),
            ty: class("kotlin/Int", Vec::new()),
            formals: Vec::new(),
            visibility: Visibility::Public,
            is_var: false,
            context_count: 0,
            constant: None,
        };
        let signature = package_property_signature(top_level(&kotlin_text), &last_index).unwrap();
        assert_eq!(member_id(&signature), 1_266_685_057_648_611_082);
    }

    #[test]
    fn an_unknown_type_parameter_is_an_error() {
        let kotlin = package(&["kotlin"]);
        let broken = function("f", None, vec![param("T")], Vec::new());
        assert_eq!(
            package_function_signature(top_level(&kotlin), &broken)
                .unwrap_err()
                .to_string(),
            "type parameter T is not in scope"
        );
    }
}

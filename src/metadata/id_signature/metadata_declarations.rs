//! Public `IdSignature`s of the declarations a KLIB's metadata describes.
//!
//! Metadata and serialized IR are two views of one KLIB. Each metadata declaration is mangled
//! exactly as the IR serializer mangled the same declaration, so a provider joins the two by
//! identity alone. Type parameters are located by their metadata declaration id, and `expect` comes
//! from the declaration's metadata flags.

use std::borrow::Cow;

use super::mangling::{
    accessor_signature, callable_signature, class_signature, enum_entry_signature,
    property_signature, Accessor, CallableShape, ClassScope, DeclarationContainer, ManglingError,
    Placement, PropertyShape, SignatureType, TypeParameterShape, TypeView,
};
use super::{KlibAccessorIdSignature, KlibPublicIdSignature};
use crate::metadata::semantic::{
    KotlinClass, KotlinConstructor, KotlinFunction, KotlinMember, KotlinProperty, KotlinType,
    KotlinTypeParameter, KotlinTypeParameterId,
};

impl SignatureType for KotlinType {
    type ParameterId = KotlinTypeParameterId;

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
            KotlinType::Param { id, nullable, .. } => TypeView::Parameter {
                id: *id,
                nullable: *nullable,
            },
            KotlinType::InProjection(inner) => TypeView::In(inner),
            KotlinType::OutProjection(inner) => TypeView::Out(inner),
            KotlinType::Star => TypeView::Star,
        })
    }
}

/// A class around a metadata declaration, or the class a class signature names.
#[derive(Clone, Copy)]
pub struct MetadataClass<'a> {
    /// The simple name.
    pub name: &'a str,
    pub type_params: &'a [KotlinTypeParameter],
    pub expect: bool,
}

impl<'a> MetadataClass<'a> {
    pub fn of(name: &'a str, class: &'a KotlinClass) -> Self {
        Self {
            name,
            type_params: &class.type_params,
            expect: class.is_expect,
        }
    }

    fn scope(&self) -> ClassScope<'a, KotlinType> {
        ClassScope {
            name: self.name,
            type_parameters: type_parameters(self.type_params),
            expect: self.expect,
        }
    }
}

/// Where a metadata declaration sits: its package and the classes around it, outermost first.
#[derive(Clone, Copy)]
pub struct MetadataContainer<'a> {
    pub package: &'a [String],
    pub classes: &'a [MetadataClass<'a>],
    /// The declaration's KLIB is a C-interop library (its manifest's `interop=true`).
    pub native_interop_library: bool,
}

fn type_parameters(parameters: &[KotlinTypeParameter]) -> Vec<TypeParameterShape<'_, KotlinType>> {
    parameters
        .iter()
        .map(|parameter| TypeParameterShape {
            id: parameter.id,
            bounds: parameter.bounds.iter().collect(),
        })
        .collect()
}

fn with_container<R>(
    container: MetadataContainer<'_>,
    build: impl FnOnce(DeclarationContainer<'_, KotlinType>) -> R,
) -> R {
    let classes: Vec<_> = container.classes.iter().map(MetadataClass::scope).collect();
    build(DeclarationContainer {
        package: container.package,
        classes: &classes,
        native_interop_library: container.native_interop_library,
    })
}

/// The identity of a class; `container` holds the classes around it, not the class itself.
pub fn metadata_class_signature(
    container: MetadataContainer<'_>,
    class: MetadataClass<'_>,
) -> KlibPublicIdSignature {
    let class = class.scope();
    with_container(container, |container| class_signature(container, &class))
}

/// The identity of an enum entry; `container` ends with its enum class.
pub fn metadata_enum_entry_signature(
    container: MetadataContainer<'_>,
    name: &str,
) -> KlibPublicIdSignature {
    with_container(container, |container| enum_entry_signature(container, name))
}

/// How a metadata declaration is reached and the extension receiver its signature writes.
/// Metadata marks both a `companion { … }` block member and a companion extension `isStatic`;
/// only the companion extension records a receiver, which is the class it extends.
fn placement<'a>(
    name: &str,
    is_static: bool,
    receiver: Option<&'a KotlinType>,
) -> Result<(Placement<'a>, Option<&'a KotlinType>), ManglingError> {
    match (is_static, receiver) {
        (false, receiver) => Ok((Placement::Ordinary, receiver)),
        (true, None) => Ok((Placement::Static, None)),
        (
            true,
            Some(KotlinType::Class {
                internal,
                args,
                nullable: false,
                ..
            }),
        ) if args.is_empty() => Ok((Placement::CompanionExtension { class_id: internal }, None)),
        (true, Some(_)) => Err(ManglingError::new(format!(
            "companion extension {name} extends a type that is not a class"
        ))),
    }
}

/// A top-level function's identity.
pub fn package_function_signature(
    container: MetadataContainer<'_>,
    function: &KotlinFunction,
) -> Result<KlibPublicIdSignature, ManglingError> {
    let (contexts, params) = function
        .params
        .split_at_checked(function.context_count)
        .ok_or_else(|| {
            ManglingError::new(format!(
                "function {} has {} context parameters but {} parameters",
                function.name,
                function.context_count,
                function.params.len()
            ))
        })?;
    let vararg = function
        .vararg
        .map(|index| {
            index.checked_sub(function.context_count).ok_or_else(|| {
                ManglingError::new(format!(
                    "function {} marks context parameter {index} as vararg",
                    function.name
                ))
            })
        })
        .transpose()?;
    let (placement, receiver) = placement(
        &function.name,
        function.is_static,
        function.receiver.as_ref(),
    )?;
    let shape = CallableShape {
        name: &function.name,
        contexts: contexts.iter().collect(),
        receiver,
        params: params.iter().collect(),
        vararg,
        type_parameters: type_parameters(&function.formals),
        expect: function.is_expect,
        placement,
    };
    with_container(container, |container| callable_signature(container, &shape))
}

fn package_property_shape(
    property: &KotlinProperty,
) -> Result<PropertyShape<'_, KotlinType>, ManglingError> {
    let (placement, receiver) = placement(
        &property.name,
        property.is_static,
        property.receiver.as_ref(),
    )?;
    Ok(PropertyShape {
        name: &property.name,
        contexts: property.context_params.iter().collect(),
        receiver,
        type_parameters: type_parameters(&property.formals),
        expect: property.is_expect,
        placement,
    })
}

/// A top-level property's identity.
pub fn package_property_signature(
    container: MetadataContainer<'_>,
    property: &KotlinProperty,
) -> Result<KlibPublicIdSignature, ManglingError> {
    let shape = package_property_shape(property)?;
    with_container(container, |container| property_signature(container, &shape))
}

/// Which accessor of a metadata property.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataAccessor {
    Getter,
    Setter,
}

fn metadata_accessor<'a>(
    accessor: MetadataAccessor,
    name: &str,
    is_var: bool,
    ty: &'a KotlinType,
) -> Result<Accessor<'a, KotlinType>, ManglingError> {
    match accessor {
        MetadataAccessor::Getter => Ok(Accessor::Getter),
        MetadataAccessor::Setter if is_var => Ok(Accessor::Setter { value: ty }),
        MetadataAccessor::Setter => Err(ManglingError::new(format!(
            "property {name} is a val and has no setter"
        ))),
    }
}

/// A top-level property's getter or setter identity.
pub fn package_property_accessor_signature(
    container: MetadataContainer<'_>,
    property: &KotlinProperty,
    accessor: MetadataAccessor,
) -> Result<KlibAccessorIdSignature, ManglingError> {
    let accessor = metadata_accessor(accessor, &property.name, property.is_var, &property.ty)?;
    let shape = package_property_shape(property)?;
    with_container(container, |container| {
        accessor_signature(container, &shape, accessor)
    })
}

fn member_property_shape(
    member: &KotlinMember,
) -> Result<PropertyShape<'_, KotlinType>, ManglingError> {
    if !member.is_property {
        return Err(ManglingError::new(format!(
            "member {} is a function and has no accessors",
            member.name
        )));
    }
    let (placement, receiver) =
        placement(&member.name, member.is_static, member.receiver.as_ref())?;
    Ok(PropertyShape {
        name: &member.name,
        contexts: member.context_params.iter().collect(),
        receiver,
        type_parameters: type_parameters(&member.formals),
        expect: member.is_expect,
        placement,
    })
}

/// A member property's getter or setter identity; `container` ends with the property's class.
pub fn member_property_accessor_signature(
    container: MetadataContainer<'_>,
    member: &KotlinMember,
    accessor: MetadataAccessor,
) -> Result<KlibAccessorIdSignature, ManglingError> {
    let shape = member_property_shape(member)?;
    let accessor = metadata_accessor(accessor, &member.name, member.is_var, &member.ret)?;
    with_container(container, |container| {
        accessor_signature(container, &shape, accessor)
    })
}

/// A class member's identity; `container` ends with the member's class.
pub fn member_signature(
    container: MetadataContainer<'_>,
    member: &KotlinMember,
) -> Result<KlibPublicIdSignature, ManglingError> {
    if member.is_property {
        let shape = member_property_shape(member)?;
        return with_container(container, |container| property_signature(container, &shape));
    }
    let (placement, receiver) =
        placement(&member.name, member.is_static, member.receiver.as_ref())?;
    let shape = CallableShape {
        name: &member.name,
        contexts: member.context_params.iter().collect(),
        receiver,
        params: member.params.iter().collect(),
        vararg: member.vararg,
        type_parameters: type_parameters(&member.formals),
        expect: member.is_expect,
        placement,
    };
    with_container(container, |container| callable_signature(container, &shape))
}

/// A constructor's identity; `container` ends with the constructed class. Metadata records no
/// `expect` on a constructor: it is expected exactly when its class is, which the container
/// carries.
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
        expect: false,
        placement: Placement::Ordinary,
    };
    with_container(container, |container| callable_signature(container, &shape))
}

/// The members the compiler gives every enum class, which its metadata leaves implicit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnumClassMemberSignatures {
    /// `values(): Array<E>`.
    pub values: KlibPublicIdSignature,
    /// `valueOf(value: String): E`.
    pub value_of: KlibPublicIdSignature,
    /// `entries: EnumEntries<E>`, present when the enum was compiled for Kotlin 1.9 or later.
    pub entries: Option<KlibPublicIdSignature>,
    pub entries_getter: Option<KlibAccessorIdSignature>,
}

/// The implicit members of an enum class; `container` ends with the enum class. They carry the
/// enum class's flags.
pub fn enum_class_member_signatures(
    container: MetadataContainer<'_>,
    has_enum_entries: bool,
) -> Result<EnumClassMemberSignatures, ManglingError> {
    let string = KotlinType::class("kotlin/String");
    let function = |name, params| CallableShape {
        name,
        contexts: Vec::new(),
        receiver: None,
        params,
        vararg: None,
        type_parameters: Vec::new(),
        expect: false,
        placement: Placement::Static,
    };
    let entries = PropertyShape {
        name: "entries",
        contexts: Vec::new(),
        receiver: None,
        type_parameters: Vec::new(),
        expect: false,
        placement: Placement::Static,
    };
    with_container(container, |container| {
        Ok(EnumClassMemberSignatures {
            values: callable_signature(container, &function("values", Vec::new()))?,
            value_of: callable_signature(container, &function("valueOf", vec![&string]))?,
            entries: has_enum_entries
                .then(|| property_signature(container, &entries))
                .transpose()?,
            entries_getter: has_enum_entries
                .then(|| accessor_signature(container, &entries, Accessor::Getter))
                .transpose()?,
        })
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod fixture_tests;

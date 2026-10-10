//! Exact public `IdSignature`s of the classes a KLIB provider publishes, and of their members.

use std::collections::{HashMap, HashSet};

use super::declaration_signatures::{
    sign_function, sign_property, type_parameter_identities, unsignable, KlibLibraryError,
    SignedFunction, SignedProperty,
};
use crate::fir::ResolvedParameterIdentity;
use crate::libraries::{
    classifier_shape, constructor_parameter_identities, declared_type_alias, AliasExpansion,
    ClassTypeParameters, LibraryType, TypeKind,
};
use crate::metadata::id_signature::{
    constructor_signature, enum_class_member_signatures, metadata_class_signature,
    EnumClassMemberSignatures, KlibPublicIdSignature, MetadataClass, MetadataContainer,
};
use crate::metadata::semantic::{KotlinClass, KotlinConstructor, KotlinModality};
use crate::types::{type_name, TypeName, Visibility};

/// A constructor joined with the identity its library serialized it under and its validated
/// parameter identities.
pub(super) struct SignedConstructor {
    pub(super) declaration: KotlinConstructor,
    pub(super) signature: KlibPublicIdSignature,
    pub(super) parameters: Box<[ResolvedParameterIdentity]>,
}

/// A class joined with its identity, its validated shape, and its signed members.
pub(super) struct SignedClassifier {
    /// The class's shape without its callables.
    pub(super) shape: LibraryType,
    pub(super) modality: KotlinModality,
    pub(super) type_parameters: ClassTypeParameters,
    pub(super) constructors: Vec<SignedConstructor>,
    /// Instance members and member extensions.
    pub(super) functions: Vec<SignedFunction>,
    pub(super) properties: Vec<SignedProperty>,
    /// Type aliases declared in this classifier's namespace.
    pub(super) type_aliases: Vec<AliasExpansion>,
    /// `companion { … }` block members, which metadata marks static.
    pub(super) associated_functions: Vec<SignedFunction>,
    pub(super) associated_properties: Vec<SignedProperty>,
    /// The members every enum class declares implicitly; `None` for any other class.
    pub(super) enum_members: Option<EnumClassMemberSignatures>,
}

/// Sign every class of one package fragment whose package has exactly these segments.
///
/// A private class is visible only inside its own file, and so is everything nested in it. A
/// private member or constructor is visible only inside its class, although its library's IR
/// still signs it publicly. None of them is published to a dependent. A class or
/// member whose exact signature `declared` already holds is a duplicate of one already published
/// and is skipped.
pub(super) fn sign_package_classes(
    package: &[String],
    mut classes: HashMap<String, KotlinClass>,
    declared: &mut HashSet<KlibPublicIdSignature>,
) -> Result<Vec<(TypeName, SignedClassifier)>, KlibLibraryError> {
    let prefix = if package.is_empty() {
        String::new()
    } else {
        format!("{}/", package.join("/"))
    };
    let mut names = classes.keys().cloned().collect::<Vec<_>>();
    // An enclosing class sorts before every class nested in it.
    names.sort();
    let mut scopes: HashMap<String, ClassTypeParameters> = HashMap::new();
    let mut published = Vec::new();
    for name in names {
        let local = name
            .strip_prefix(&prefix)
            .ok_or_else(|| unsignable(package, &name, "class lies outside its package"))?
            .to_owned();
        let segments = local.split('.').collect::<Vec<_>>();
        let enclosing = segments[..segments.len() - 1]
            .iter()
            .enumerate()
            .map(|(depth, _)| format!("{prefix}{}", segments[..=depth].join(".")))
            .collect::<Vec<_>>();
        let chain_keys = enclosing
            .iter()
            .chain(std::iter::once(&name))
            .collect::<Vec<_>>();
        // The classes around each declaration, outermost first, as its signatures name them.
        let mut containers = Vec::with_capacity(chain_keys.len());
        for (key, simple) in chain_keys.iter().zip(&segments) {
            let class = classes.get(key.as_str()).ok_or_else(|| {
                unsignable(
                    package,
                    &local,
                    format!("enclosing class {key} is absent from its fragment"),
                )
            })?;
            if class.visibility == Visibility::Private {
                break;
            }
            containers.push((*simple, class.type_params.clone(), class.is_expect));
        }
        if containers.len() != chain_keys.len() {
            continue;
        }
        let chain = containers
            .iter()
            .map(|(name, type_params, expect)| MetadataClass {
                name,
                type_params,
                expect: *expect,
            })
            .collect::<Vec<_>>();
        let class = &classes[&name];
        let captured = match enclosing.last() {
            Some(owner) if class.is_inner => Some(scopes.get(owner).ok_or_else(|| {
                unsignable(package, &local, "inner class has no enclosing class scope")
            })?),
            _ => None,
        };

        let (own, outer) = chain.split_last().expect("a class has a name");
        let around = MetadataContainer {
            package,
            classes: outer,
            native_interop_library: false,
        };
        let signature = metadata_class_signature(around, *own);
        let identities = type_parameter_identities(
            &signature,
            &class.type_params,
            captured.map(ClassTypeParameters::identities),
        );
        let type_parameters = ClassTypeParameters::of(class, identities, captured);
        scopes.insert(name.clone(), type_parameters.clone());
        if !declared.insert(signature) {
            continue;
        }
        let inside = MetadataContainer {
            classes: &chain,
            ..around
        };
        let identity = type_name(&name);
        let outer_instance = enclosing
            .last()
            .filter(|_| class.is_inner)
            .map(|owner| type_name(owner));
        let shape = classifier_shape(identity, &name, class, &type_parameters, outer_instance)
            .map_err(|error| unsignable(package, &local, error))?;
        let enum_members = (class.kind == TypeKind::Enum)
            .then(|| enum_class_member_signatures(inside, class.has_enum_entries))
            .transpose()
            .map_err(|error| unsignable(package, &local, error))?;
        let constructor_signatures = class
            .constructors
            .iter()
            .map(|constructor| {
                constructor_signature(inside, constructor)
                    .map_err(|error| unsignable(package, &format!("{local}.<init>"), error))
            })
            .collect::<Result<Vec<_>, _>>()?;

        // The class itself stays in place: a class nested in it still reads it as its container.
        let class = classes.get_mut(&name).expect("the class was just read");
        let mut constructors = Vec::new();
        for (declaration, signature) in std::mem::take(&mut class.constructors)
            .into_iter()
            .zip(constructor_signatures)
        {
            if declaration.visibility == Visibility::Private || !declared.insert(signature.clone())
            {
                continue;
            }
            let parameters = constructor_parameter_identities(&declaration)
                .map_err(|error| unsignable(package, &format!("{local}.<init>"), error))?;
            constructors.push(SignedConstructor {
                declaration,
                signature,
                parameters,
            });
        }
        let mut functions = Vec::new();
        let mut associated_functions = Vec::new();
        for declaration in std::mem::take(&mut class.functions) {
            if declaration.visibility == Visibility::Private {
                continue;
            }
            let path = format!("{local}.{}", declaration.name);
            let signed = sign_function(
                inside,
                &path,
                declaration,
                Some(type_parameters.identities()),
            )?;
            if !declared.insert(signed.signature.clone()) {
                continue;
            }
            if signed.declaration.is_static {
                associated_functions.push(signed);
            } else {
                functions.push(signed);
            }
        }
        let mut properties = Vec::new();
        let mut associated_properties = Vec::new();
        for declaration in std::mem::take(&mut class.properties) {
            if declaration.visibility == Visibility::Private {
                continue;
            }
            let path = format!("{local}.{}", declaration.name);
            let signed = sign_property(
                inside,
                &path,
                declaration,
                Some(type_parameters.identities()),
            )?;
            if !declared.insert(signed.signature.clone()) {
                continue;
            }
            if signed.declaration.is_static {
                associated_properties.push(signed);
            } else {
                properties.push(signed);
            }
        }
        let type_aliases = std::mem::take(&mut class.type_aliases)
            .into_iter()
            .filter(|alias| alias.visibility != Visibility::Private)
            .map(|alias| {
                declared_type_alias(identity, &alias, type_parameters.enclosing())
                    .map_err(|error| unsignable(package, &format!("{local}.{}", alias.name), error))
            })
            .collect::<Result<Vec<_>, _>>()?;
        published.push((
            identity,
            SignedClassifier {
                shape,
                modality: class.modality,
                type_parameters,
                constructors,
                functions,
                properties,
                type_aliases,
                associated_functions,
                associated_properties,
                enum_members,
            },
        ));
    }
    Ok(published)
}

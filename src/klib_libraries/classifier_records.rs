//! Published classes normalized into the common classifier model.
//!
//! A class record carries its own declarations only: its constructors, the members and member
//! extensions it declares, and its `companion { … }` block members. Inherited members are core's
//! hierarchy walk over the direct supertypes the record lists.

use super::builtin_realizations;
use super::classifier_signatures::SignedClassifier;
use super::external_identities::ExternalIdentities;
use super::inventory::PackageInventory;
use super::lookup::{published_function, published_property};
use crate::fir::ResolvedParameterIdentity;
use crate::libraries::{
    constructor_parameter_list, declared_constructor, enum_entries_getter, enum_value_of,
    enum_values, member_record, settle_no_arg_construction, CallablePlacement, Callables, FnKind,
    FunctionInfo, FunctionSet, KlibDeclarationSignature, LibraryType, PropertyInfo, PropertySet,
};
use crate::types::{SemanticCallableOwner, TypeName};

/// The complete record of the published class `identity`.
pub(super) fn classifier_record(
    inventory: &PackageInventory,
    identities: &ExternalIdentities,
    identity: TypeName,
) -> Option<LibraryType> {
    let signed = inventory.classifier(identity)?;
    let mut shape = signed.shape.clone();
    for constructor in &signed.constructors {
        let mut member = declared_constructor(
            identity,
            &shape,
            &signed.type_parameters,
            &constructor.declaration,
            &constructor.parameters,
        );
        identities.assign_constructor(
            &constructor.signature,
            &constructor.parameters,
            identity,
            &mut member,
        );
        shape
            .named_parameter_lists
            .push(constructor_parameter_list(&member));
        shape.constructors.push(member);
    }
    settle_no_arg_construction(&mut shape, signed.modality);

    let member = CallablePlacement::Member {
        owner: identity,
        owner_is_interface: shape.is_interface(),
    };
    let enclosing = signed.type_parameters.enclosing();
    let mut functions: Vec<(String, FunctionInfo)> = Vec::new();
    for function in &signed.functions {
        let mut published = published_function(identities, function, member, enclosing);
        builtin_realizations::realize_member(identity, &mut published);
        shape.members.push(member_record(&published));
        functions.push((function.declaration.name.clone(), published));
    }
    let properties = signed
        .properties
        .iter()
        .map(|property| {
            (
                property.declaration.name.clone(),
                published_property(identities, property, member, enclosing),
            )
        })
        .collect::<Vec<_>>();
    let names = functions
        .iter()
        .map(|(name, _)| name)
        .chain(properties.iter().map(|(name, _)| name))
        .cloned()
        .collect::<Vec<_>>();
    for name in names {
        if shape.declared_callables.contains_key(&name) {
            continue;
        }
        let named_functions = functions
            .iter()
            .filter(|(declared, _)| *declared == name)
            .map(|(_, function)| function.clone())
            .collect();
        let named_properties = properties
            .iter()
            .filter(|(declared, _)| *declared == name)
            .map(|(_, property)| property.clone())
            .collect();
        shape.insert_declared_callables(
            name,
            Callables::from_parts(
                FunctionSet {
                    overloads: named_functions,
                },
                PropertySet {
                    overloads: named_properties,
                },
            ),
        );
    }

    crate::libraries::add_core_builtin_declarations(&mut shape, identity);

    for function in &signed.associated_functions {
        let published = published_function(
            identities,
            function,
            block_member_placement(identity),
            enclosing,
        );
        shape.companion.push(member_record(&published));
    }
    for (implicit, member) in enum_class_members(identities, identity, signed) {
        match implicit {
            EnumClassMember::Function => shape.companion.push(member),
            EnumClassMember::EntriesGetter => shape.enum_entries_accessor = Some(member),
        }
    }
    Some(shape)
}

/// The placement of a `companion { … }` block member of `classifier`: named through it, declared
/// by it, and privately accessible within its body.
fn block_member_placement(classifier: TypeName) -> CallablePlacement {
    CallablePlacement::Associated {
        classifier,
        declaration_owner: SemanticCallableOwner::Classifier(classifier),
        access_owner: Some(classifier),
    }
}

/// The callables named `name` that are named through `classifier` with no value operand: its
/// `companion { … }` block members, the companion extensions declared for it, and an enum class's
/// implicit `values` and `valueOf`.
pub(super) fn associated_callables(
    inventory: &PackageInventory,
    identities: &ExternalIdentities,
    classifier: TypeName,
    name: &str,
) -> (Vec<FunctionInfo>, Vec<PropertyInfo>) {
    let mut functions = Vec::new();
    let mut properties = Vec::new();
    if let Some(signed) = inventory.classifier(classifier) {
        let enclosing = signed.type_parameters.enclosing();
        let placement = block_member_placement(classifier);
        functions.extend(
            signed
                .associated_functions
                .iter()
                .filter(|function| function.declaration.name == name)
                .map(|function| published_function(identities, function, placement, enclosing)),
        );
        properties.extend(
            signed
                .associated_properties
                .iter()
                .filter(|property| property.declaration.name == name)
                .map(|property| published_property(identities, property, placement, enclosing)),
        );
        functions.extend(
            enum_class_members(identities, classifier, signed)
                .into_iter()
                .filter(|(implicit, member)| {
                    *implicit == EnumClassMember::Function && member.name == name
                })
                .map(|(_, member)| {
                    let mut function =
                        FunctionInfo::classifier_member(FnKind::TopLevel, classifier, member);
                    function.callable.declaration_owner =
                        Some(SemanticCallableOwner::Classifier(classifier));
                    function
                }),
        );
    }
    for extension in inventory.companion_functions(classifier, name) {
        let placement = companion_extension_placement(classifier, extension.package);
        functions.push(published_function(
            identities,
            &extension.signed,
            placement,
            &Default::default(),
        ));
    }
    for extension in inventory.companion_properties(classifier, name) {
        let placement = companion_extension_placement(classifier, extension.package);
        properties.push(published_property(
            identities,
            &extension.signed,
            placement,
            &Default::default(),
        ));
    }
    (functions, properties)
}

/// The placement of a companion extension of `classifier` that `package` declares: named through
/// the classifier, with no private access tied to its body.
fn companion_extension_placement(classifier: TypeName, package: TypeName) -> CallablePlacement {
    CallablePlacement::Associated {
        classifier,
        declaration_owner: SemanticCallableOwner::Package(package),
        access_owner: None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EnumClassMember {
    /// `values()` or `valueOf(value)`, named through the enum class.
    Function,
    /// The getter of the synthetic `entries` property.
    EntriesGetter,
}

/// The members an enum class declares implicitly, each with the identity of the declaration its
/// library serialized: `values`, `valueOf`, and, for an enum compiled with `entries`, its getter.
fn enum_class_members(
    identities: &ExternalIdentities,
    owner: TypeName,
    signed: &SignedClassifier,
) -> Vec<(EnumClassMember, crate::libraries::LibraryMember)> {
    let Some(signatures) = &signed.enum_members else {
        return Vec::new();
    };
    let mut members = Vec::new();
    let mut values = enum_values(owner);
    identities.assign_classifier_member(
        KlibDeclarationSignature::Public(signatures.values.clone()),
        &[],
        owner,
        &mut values,
    );
    members.push((EnumClassMember::Function, values));
    let mut value_of = enum_value_of(owner);
    identities.assign_classifier_member(
        KlibDeclarationSignature::Public(signatures.value_of.clone()),
        &[ResolvedParameterIdentity::Source("value".into())],
        owner,
        &mut value_of,
    );
    members.push((EnumClassMember::Function, value_of));
    if let Some(getter_signature) = &signatures.entries_getter {
        let mut getter = enum_entries_getter(owner, getter_signature.name());
        identities.assign_classifier_member(
            KlibDeclarationSignature::Accessor(getter_signature.clone()),
            &[],
            owner,
            &mut getter,
        );
        members.push((EnumClassMember::EntriesGetter, getter));
    }
    members
}

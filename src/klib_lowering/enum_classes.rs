//! An authenticated dependency enum class, which a support unit materializes so that the shared
//! enum realization builds its constants as it builds a source enum's.
//!
//! The serialized class is the producer's declaration, so it is the authority: the class must be
//! an enum class extending `kotlin.Enum` of itself, its one constructor must only delegate to
//! `kotlin.Enum`'s and run its instance initializer, and each entry must call that constructor
//! with no arguments and declare no body. The compiler-generated members (`values`, `valueOf` and
//! `entries`) are recognized by their synthetic bodies; the common realization provides them.
//! Members the class declares for itself, without overriding another declaration, are linked as
//! members of the materialized class when a body calls them. A stored property, an override, a
//! constructor parameter or an entry body is a capability this does not model yet and declines
//! the read.

use super::decline::KlibBodyDeclineReason;
use super::klib_types::{classifier_declaration, classifier_name, semantic_type};
use crate::klib_libraries::{KlibCalleeOrigin, KlibDeclarationBodies, KlibEnumEntryClass};
use crate::libraries::{KlibDeclarationSignature, KlibDependencyCallable};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrClass, KlibIrExprKind, KlibIrFunction,
    KlibIrMember, KlibIrStatement, KlibIrSyntheticBody,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};
use crate::types::{ClassifierDeclarationName, Ty, TypeName};

/// What a support unit materializes for one dependency enum class.
pub(super) struct EnumClass {
    /// The enum's constructor, the identity its materialization is keyed by.
    pub(super) constructor: KlibDependencyCallable,
    pub(super) classifier: TypeName,
    pub(super) declaration_name: ClassifierDeclarationName,
    /// Its declared supertype, `kotlin.Enum` of the enum itself.
    pub(super) supertype: Ty,
    /// Its constants, in declaration order.
    pub(super) entries: Vec<String>,
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

fn unsupported(what: &'static str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::UnsupportedDeclaration(what)
}

/// The enum class declaring `entry`, which a body of `body_module` reads, and that entry's name.
pub(super) fn authenticate_entry(
    bodies: &KlibDeclarationBodies,
    body_module: &str,
    entry: &KlibPublicIdSignature,
) -> Result<(EnumClass, String), KlibBodyDeclineReason> {
    let declared = bodies
        .enum_entry_class(body_module, entry)
        .map_err(mismatch)?
        .ok_or_else(|| mismatch("an enum entry has no serialized declaring class"))?;
    let class = authenticate(bodies, &declared)?;
    let name = declared
        .class
        .members
        .iter()
        .find_map(|member| match member {
            KlibIrMember::EnumEntry(declared)
                if declared.base.symbol.signature == KlibIrSignature::Public(entry.clone()) =>
            {
                Some(declared.name.clone())
            }
            _ => None,
        })
        .expect("the declaring class was found by this entry");
    Ok((class, name))
}

/// Whether a support unit materializes the enum class `class` a member of it is called on: every
/// instance is one of its constants, which the unit builds when the class's read authenticates.
pub(crate) fn materializes(
    bodies: &KlibDeclarationBodies,
    class: &crate::libraries::KlibBodyClassContract,
) -> bool {
    let Ok(Some((arena, declared))) = bodies.class_header(&class.module, &class.signature) else {
        return false;
    };
    authenticate(
        bodies,
        &KlibEnumEntryClass {
            module: &class.module,
            arena,
            class: declared,
        },
    )
    .is_ok()
}

/// One of the two operations an enum class declares on itself: `values()` and `valueOf(name)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EnumStaticMember {
    Values,
    ValueOf,
}

/// The enum class whose compiler-generated `values()` or `valueOf` is exactly `identity`, and which
/// of the two it is. `None` for any other callable.
pub(super) fn authenticate_static_member(
    bodies: &KlibDeclarationBodies,
    identity: &KlibDependencyCallable,
) -> Result<Option<(EnumClass, EnumStaticMember)>, KlibBodyDeclineReason> {
    let Some((declared, body)) = bodies.enum_static_member(identity) else {
        return Ok(None);
    };
    let member = match body {
        KlibIrSyntheticBody::EnumValues => EnumStaticMember::Values,
        KlibIrSyntheticBody::EnumValueOf => EnumStaticMember::ValueOf,
        KlibIrSyntheticBody::EnumEntries => {
            return Err(mismatch(
                "an enum's `entries` getter is called as a function",
            ))
        }
    };
    Ok(Some((authenticate(bodies, &declared)?, member)))
}

/// The enum class `declared`, which a body reads a constant, a generated operation or a member of.
pub(super) fn authenticate(
    bodies: &KlibDeclarationBodies,
    declared: &KlibEnumEntryClass<'_>,
) -> Result<EnumClass, KlibBodyDeclineReason> {
    let (arena, class) = (declared.arena, declared.class);
    let KlibIrSignature::Public(signature) = &class.base.symbol.signature else {
        return Err(unsupported("a file-private enum class"));
    };
    if class.base.symbol.kind != KlibIrSymbolKind::Class
        || crate::metadata::semantic::class_kind(class.base.flags)
            != crate::libraries::TypeKind::Enum
        || class.value_class_representation.is_some()
    {
        return Err(mismatch("an enum entry's class is not an enum class"));
    }
    if signature.member_id().is_some() {
        return Err(mismatch("an enum class signature names a member"));
    }
    if !class.type_parameters.is_empty() {
        return Err(mismatch("an enum class declares type parameters"));
    }
    let classifier = classifier_name(signature)?;
    let parameters = super::klib_types::KlibTypeParameters::default();
    let self_type = Ty::obj_name(classifier);
    let receiver = class
        .this_receiver
        .as_ref()
        .ok_or_else(|| mismatch("an enum class has no serialized instance receiver"))?;
    if semantic_type(arena, &parameters, receiver.ty)? != self_type {
        return Err(mismatch(
            "an enum instance receiver has another declared type",
        ));
    }
    let supertype = Ty::obj_args_name(crate::types::wk::kotlin_enum(), &[self_type]);
    if class
        .supertypes
        .iter()
        .map(|ty| semantic_type(arena, &parameters, *ty))
        .collect::<Result<Vec<_>, _>>()?
        != [supertype]
    {
        return Err(unsupported("an enum class implementing an interface"));
    }

    let mut constructors = Vec::new();
    let mut entries = Vec::new();
    for member in &class.members {
        match member {
            KlibIrMember::Function(function) => {
                let function = arena.function(*function);
                if function.constructor {
                    constructors.push(function);
                } else if !matches!(
                    (function.name.as_str(), function.enum_special_member()),
                    ("values", Some(KlibIrSyntheticBody::EnumValues))
                        | ("valueOf", Some(KlibIrSyntheticBody::EnumValueOf))
                ) {
                    own_member(function)?;
                }
            }
            KlibIrMember::Property(property) => {
                let getter = property.getter.map(|getter| arena.function(getter));
                let generated_entries = property.name == "entries"
                    && property.setter.is_none()
                    && getter.is_some_and(|getter| {
                        getter.enum_special_member() == Some(KlibIrSyntheticBody::EnumEntries)
                    });
                if !generated_entries {
                    if property.backing_field.is_some() {
                        return Err(unsupported("an enum class with stored member properties"));
                    }
                    for accessor in [property.getter, property.setter].into_iter().flatten() {
                        own_member(arena.function(accessor))?;
                    }
                }
            }
            KlibIrMember::EnumEntry(entry) => entries.push(entry),
            _ => return Err(unsupported("an enum class with nested declarations")),
        }
    }
    let [constructor] = constructors.as_slice() else {
        return Err(unsupported("an enum class with several constructors"));
    };
    authenticate_constructor(
        bodies,
        declared.module,
        arena,
        class,
        constructor,
        self_type,
    )?;
    let names = entries
        .iter()
        .map(|entry| {
            if entry.class.is_some() {
                return Err(unsupported("an enum entry with a body"));
            }
            let initializer = entry
                .initializer
                .ok_or_else(|| mismatch("an enum entry has no serialized initializer"))?;
            let KlibIrExprKind::EnumConstructorCall(access) = &arena.expr(initializer).kind else {
                return Err(mismatch(
                    "an enum entry does not call its enum's constructor",
                ));
            };
            if access.symbol != constructor.base.symbol
                || !access.type_arguments.is_empty()
                || !no_operands(&access.arguments)
            {
                return Err(mismatch(
                    "an enum entry's constructor call disagrees with its enum's constructor",
                ));
            }
            Ok(entry.name.clone())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let KlibIrSignature::Public(constructor_signature) = &constructor.base.symbol.signature else {
        return Err(mismatch("an enum constructor has no public signature"));
    };
    Ok(EnumClass {
        constructor: KlibDependencyCallable {
            module: declared.module.to_owned(),
            declaration: KlibDeclarationSignature::Public(constructor_signature.clone()),
        },
        classifier,
        declaration_name: classifier_declaration(signature)?,
        supertype,
        entries: names,
    })
}

/// A member the enum class declares for itself: a call of it links its body as a member of the
/// materialized class, which every constant is an instance of. A member overriding another
/// declaration is reached through dispatch, which the materialized class does not install, and
/// an abstract one needs entry bodies, which the read already declines.
fn own_member(function: &KlibIrFunction) -> Result<(), KlibBodyDeclineReason> {
    if !function.overridden.is_empty() {
        return Err(unsupported("an enum class overriding an inherited member"));
    }
    if function.body.is_none() {
        return Err(unsupported("an enum class with abstract members"));
    }
    Ok(())
}

/// The constructor only delegates to `kotlin.Enum`'s and then runs the instance initializer, so
/// constructing a constant does nothing beyond the `kotlin.Enum` storage the target owns.
fn authenticate_constructor(
    bodies: &KlibDeclarationBodies,
    module: &str,
    arena: &KlibIrArena,
    class: &KlibIrClass,
    constructor: &KlibIrFunction,
    self_type: Ty,
) -> Result<(), KlibBodyDeclineReason> {
    if !constructor.regular_parameters.is_empty()
        || !constructor.context_parameters.is_empty()
        || constructor.extension_receiver.is_some()
        || !constructor.type_parameters.is_empty()
    {
        return Err(unsupported("an enum constructor with parameters"));
    }
    let Some(KlibIrBody::Block(statements)) = &constructor.body else {
        return Err(mismatch("an enum constructor has no serialized body"));
    };
    let [KlibIrStatement::Expression(delegate), KlibIrStatement::Expression(initialize)] =
        statements.as_slice()
    else {
        return Err(unsupported("a nontrivial enum constructor body"));
    };
    let KlibIrExprKind::EnumConstructorCall(access) = &arena.expr(*delegate).kind else {
        return Err(mismatch(
            "an enum constructor does not delegate to kotlin.Enum",
        ));
    };
    let KlibIrSignature::Public(enum_constructor) = &access.symbol.signature else {
        return Err(mismatch(
            "an enum constructor delegates to a file-private constructor",
        ));
    };
    let enum_constructor = match bodies.linked_origin(
        module,
        &KlibDeclarationSignature::Public(enum_constructor.clone()),
    ) {
        KlibCalleeOrigin::Exact(identity) => identity,
        KlibCalleeOrigin::Ambiguous => {
            return Err(mismatch("ambiguous kotlin.Enum constructor producer"))
        }
        KlibCalleeOrigin::Missing => {
            return Err(mismatch(
                "an enum constructor delegates to an unknown constructor",
            ))
        }
    };
    let enum_class = KlibPublicIdSignature::classifier(crate::types::wk::kotlin_enum());
    let delegated = bodies
        .class_member_header(
            &enum_constructor,
            &enum_class,
            Some(crate::libraries::KlibAccessorRole::Getter),
        )
        .map_err(mismatch)?
        .ok_or_else(|| mismatch("an enum constructor delegates past kotlin.Enum"))?;
    // `kotlin.Enum`'s `name` and `ordinal` are absent operands: the target supplies each
    // constant's, as it does for a source enum.
    let implicit_operands = matches!(&access.arguments, KlibIrArguments::Flat(values)
        if values.len() == delegated.function.regular_parameters.len()
            && values.iter().all(Option::is_none));
    let [Some(applied)] = access.type_arguments.as_slice() else {
        return Err(mismatch(
            "an enum constructor applies kotlin.Enum to no single type",
        ));
    };
    if access.symbol.kind != KlibIrSymbolKind::Constructor
        || !delegated.function.constructor
        || !implicit_operands
        || semantic_type(
            arena,
            &super::klib_types::KlibTypeParameters::default(),
            *applied,
        )? != self_type
        || !matches!(&arena.expr(*initialize).kind,
            KlibIrExprKind::InstanceInitializerCall(initialized) if *initialized == class.base.symbol)
    {
        return Err(mismatch(
            "an enum constructor does not delegate to kotlin.Enum before initialization",
        ));
    }
    Ok(())
}

fn no_operands(arguments: &KlibIrArguments) -> bool {
    match arguments {
        KlibIrArguments::Flat(values) => values.is_empty(),
        KlibIrArguments::Split {
            dispatch_receiver,
            extension_receiver,
            values,
        } => dispatch_receiver.is_none() && extension_receiver.is_none() && values.is_empty(),
    }
}

//! An authenticated dependency enum class, which a support unit materializes so that the shared
//! enum realization builds its constants as it builds a source enum's.
//!
//! The serialized class is the producer's declaration, so it is the authority: the class must be
//! an enum class extending `kotlin.Enum` of itself, its one constructor must only delegate to
//! `kotlin.Enum`'s and run its instance initializer, and each entry must call that constructor
//! with no arguments and declare no body. The compiler-generated members (`values`, `valueOf` and
//! `entries`) are recognized by their synthetic bodies; the common realization provides them.
//! Any other member, constructor parameter or entry body is a capability this does not model yet
//! and declines the read.

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

fn authenticate(
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
                    (function.name.as_str(), special_member_body(function)),
                    ("values", Some(KlibIrSyntheticBody::EnumValues))
                        | ("valueOf", Some(KlibIrSyntheticBody::EnumValueOf))
                ) {
                    return Err(unsupported("an enum class with member functions"));
                }
            }
            KlibIrMember::Property(property) => {
                let getter = property.getter.map(|getter| arena.function(getter));
                if property.backing_field.is_some()
                    || property.setter.is_some()
                    || property.name != "entries"
                    || !getter.is_some_and(|getter| {
                        special_member_body(getter) == Some(KlibIrSyntheticBody::EnumEntries)
                    })
                {
                    return Err(unsupported("an enum class with member properties"));
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

/// The synthetic body of a compiler-generated enum member, which the common realization provides.
fn special_member_body(function: &KlibIrFunction) -> Option<KlibIrSyntheticBody> {
    match &function.body {
        Some(KlibIrBody::Synthetic(body))
            if function.base.origin == "ENUM_CLASS_SPECIAL_MEMBER" =>
        {
            Some(*body)
        }
        _ => None,
    }
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
    let enum_constructor = match bodies.callee_origin(
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
            crate::libraries::KlibAccessorRole::Getter,
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

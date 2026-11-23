//! The entries of an admitted dependency enum class and its delegation to `kotlin.Enum`.
//!
//! An enum class is initialized as any admitted class is (`class_initialization`): its private
//! primary constructor stores its own state, after delegating to `kotlin.Enum`'s constructor,
//! whose `name` and `ordinal` the target supplies for each constant, as it does for a source enum.
//! What this module authenticates is particular to enums: that delegation, and each entry, a call
//! of the enum's constructor whose arguments the shared enum realization evaluates in declaration
//! order. An entry with a body of its own is a capability this does not model yet.

use super::class_headers::ClassMemberHeader;
use super::class_initialization::SuperclassCall;
use super::decline::KlibBodyDeclineReason;
use super::klib_types::semantic_type;
use crate::klib_libraries::{KlibCalleeOrigin, KlibDeclarationBodies};
use crate::libraries::KlibDeclarationSignature;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArguments, KlibIrExprId, KlibIrExprKind, KlibIrMember, KlibIrMemberAccess,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};
use crate::types::{Ty, TypeName};

/// One constant of an admitted enum class, in declaration order: its name, and the arguments its
/// declaration passes to the enum's constructor, in that constructor's parameter order.
pub(super) struct EnumEntryInitialization {
    pub(super) name: String,
    pub(super) arguments: Vec<KlibIrExprId>,
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

/// The enum class declaring `entry`, which a body of `body_module` reads, and that entry's name.
pub(super) fn authenticate_entry(
    bodies: &KlibDeclarationBodies,
    body_module: &str,
    entry: &KlibPublicIdSignature,
) -> Result<(TypeName, String), KlibBodyDeclineReason> {
    let class = bodies
        .enum_entry_class(body_module, entry)
        .map_err(mismatch)?
        .ok_or_else(|| mismatch("an enum entry has no serialized declaring class"))?;
    let KlibIrSignature::Public(signature) = &class.base.symbol.signature else {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a file-private enum class",
        ));
    };
    if class.base.symbol.kind != KlibIrSymbolKind::Class
        || crate::metadata::semantic::class_kind(class.base.flags)
            != crate::libraries::TypeKind::Enum
        || signature.member_id().is_some()
    {
        return Err(mismatch("an enum entry's class is not an enum class"));
    }
    let name = class
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
    Ok((super::klib_types::classifier_name(signature)?, name))
}

/// The entries of the enum class `header` initializes, in declaration order. Each must call the
/// constructor `header` authenticates with a value for every parameter, and declare no body.
pub(super) fn entries(
    header: &ClassMemberHeader<'_, '_>,
) -> Result<Vec<EnumEntryInitialization>, KlibBodyDeclineReason> {
    let parameters = header.function.regular_parameters.len();
    let mut entries = Vec::new();
    for member in &header.class.members {
        let KlibIrMember::EnumEntry(entry) = member else {
            continue;
        };
        if entry.class.is_some() {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "an enum entry with a body",
            ));
        }
        let initializer = entry
            .initializer
            .ok_or_else(|| mismatch("an enum entry has no serialized initializer"))?;
        let KlibIrExprKind::EnumConstructorCall(access) = &header.arena.expr(initializer).kind
        else {
            return Err(mismatch(
                "an enum entry does not call its enum's constructor",
            ));
        };
        if access.symbol != header.function.base.symbol || !access.type_arguments.is_empty() {
            return Err(mismatch(
                "an enum entry's constructor call disagrees with its enum's constructor",
            ));
        }
        let arguments = value_operands(access)?
            .iter()
            .copied()
            .collect::<Option<Vec<_>>>()
            .filter(|arguments| arguments.len() == parameters)
            .ok_or_else(|| {
                mismatch("an enum entry does not pass a value for each constructor parameter")
            })?;
        entries.push(EnumEntryInitialization {
            name: entry.name.clone(),
            arguments,
        });
    }
    Ok(entries)
}

/// The delegation `delegate` of the enum constructor `header` to `kotlin.Enum`'s constructor,
/// which the target lays out: `kotlin.Enum` applied to the enum itself, with its `name` and
/// `ordinal` left to the target, as a source enum's are. Answers the call and the superclass type.
pub(super) fn kotlin_enum_delegation(
    header: &ClassMemberHeader<'_, '_>,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
    delegate: KlibIrExprId,
) -> Result<(SuperclassCall, Ty), KlibBodyDeclineReason> {
    let KlibIrExprKind::EnumConstructorCall(access) = &header.arena.expr(delegate).kind else {
        return Err(mismatch(
            "an enum constructor does not delegate to kotlin.Enum",
        ));
    };
    let KlibIrSignature::Public(enum_constructor) = &access.symbol.signature else {
        return Err(mismatch(
            "an enum constructor delegates to a file-private constructor",
        ));
    };
    let identity = match bodies.linked_origin(
        &header.contract.module,
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
    let kotlin_enum = crate::types::wk::kotlin_enum();
    let delegated = bodies
        .class_member_header(
            &identity,
            &KlibPublicIdSignature::classifier(kotlin_enum),
            Some(crate::libraries::KlibAccessorRole::Getter),
        )
        .map_err(mismatch)?
        .ok_or_else(|| mismatch("an enum constructor delegates past kotlin.Enum"))?;
    let self_type = header.contract.self_type();
    let operands = value_operands(access)?;
    let implicit_operands = operands.len() == delegated.function.regular_parameters.len()
        && operands.iter().all(Option::is_none);
    let [Some(applied)] = access.type_arguments.as_slice() else {
        return Err(mismatch(
            "an enum constructor applies kotlin.Enum to no single type",
        ));
    };
    if access.symbol.kind != KlibIrSymbolKind::Constructor
        || !delegated.function.constructor
        || !implicit_operands
        || semantic_type(header.arena, &header.types, *applied)? != self_type
    {
        return Err(mismatch(
            "an enum constructor does not delegate to kotlin.Enum of itself",
        ));
    }
    if !callees.target_represents(kotlin_enum) {
        return Err(mismatch("the target does not lay out kotlin.Enum"));
    }
    Ok((
        SuperclassCall {
            constructor: identity,
            arguments: Vec::new(),
            owner: kotlin_enum,
            params: Vec::new(),
            materialized: false,
        },
        Ty::obj_args_name(kotlin_enum, &[self_type]),
    ))
}

/// The value operands of a constructor call, which passes no receiver.
fn value_operands(
    access: &KlibIrMemberAccess,
) -> Result<&[Option<KlibIrExprId>], KlibBodyDeclineReason> {
    match &access.arguments {
        KlibIrArguments::Flat(values)
        | KlibIrArguments::Split {
            dispatch_receiver: None,
            extension_receiver: None,
            values,
        } => Ok(values),
        KlibIrArguments::Split { .. } => {
            Err(mismatch("an enum constructor call passes a receiver"))
        }
    }
}

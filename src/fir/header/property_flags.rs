//! Parser-owned property facts published by the stable header inventory.

use crate::ast::{File, PropDecl};

use super::{DeclarationFlags, InferredSignatureKind};

pub(super) fn from_property(
    file: &File,
    property: &PropDecl,
    inferred_signature: Option<InferredSignatureKind>,
) -> DeclarationFlags {
    DeclarationFlags::default()
        .with(DeclarationFlags::EXTERNAL, property.is_external)
        .with(DeclarationFlags::EXPECT, property.is_expect)
        .with(DeclarationFlags::CONST, property.is_const)
        .with(DeclarationFlags::OPEN, property.is_open)
        .with(DeclarationFlags::FINAL_MODIFIER, property.is_final)
        .with(DeclarationFlags::OVERRIDE, property.is_override)
        .with(DeclarationFlags::ABSTRACT, property.is_abstract)
        .with(DeclarationFlags::MUTABLE, property.is_var)
        .with(
            DeclarationFlags::HAS_VISIBILITY_MODIFIER,
            property.has_visibility_modifier,
        )
        .with(DeclarationFlags::LATEINIT, property.is_lateinit)
        .with(DeclarationFlags::ACTUAL, property.is_actual)
        .with(DeclarationFlags::DELEGATED, property.delegate.is_some())
        .with(
            DeclarationFlags::INFERRED_PROPERTY_TYPE,
            inferred_signature
                .is_some_and(|kind| kind != InferredSignatureKind::BackingFieldInitializer),
        )
        .with(
            DeclarationFlags::EXPLICIT_BACKING_FIELD,
            property.explicit_backing_field.is_some(),
        )
        .with(DeclarationFlags::CUSTOM_GETTER, property.getter.is_some())
        .with(
            DeclarationFlags::GETTER_READS_BACKING_FIELD,
            property.getter_reads_field,
        )
        .with(DeclarationFlags::CUSTOM_SETTER, property.setter.is_some())
        .with(
            DeclarationFlags::SETTER_HAS_BODY,
            property
                .setter
                .as_ref()
                .is_some_and(|setter| setter.body.is_some()),
        )
        .with(DeclarationFlags::HAS_INITIALIZER, property.init.is_some())
        .with(
            DeclarationFlags::SCRIPT_RESULT,
            property.init.is_some() && property.init == file.script_result,
        )
        .with(
            DeclarationFlags::LITERAL_INITIALIZER,
            file.literal_initializer(property),
        )
        .with(DeclarationFlags::COMPANION, property.is_companion_extension)
        .with(
            DeclarationFlags::COMPANION_BLOCK_MEMBER,
            property.is_companion_block_member,
        )
}

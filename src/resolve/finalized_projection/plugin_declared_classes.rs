//! Compiler plugins' records over the module's finalized class declarations.
//!
//! Pass 2 lowers one file at a time, so what a plugin decides about a class that another file
//! needs (the serial elements a subclass in that file inherits) is decided once, before Pass 2, by
//! the plugin itself. This module only describes each source class from its checked declarations
//! in the shape a plugin reads ([`FrontendDeclaredClass`]) and publishes the plugin's record by the
//! class's stable identity; it makes none of the plugin's decisions.

use super::SymbolTable;
use crate::fir::{DeclarationFlags, DeclarationId, DeclarationKind, ResolvedModuleIndex};
use crate::plugins::{FrontendDeclaredClass, FrontendDeclaredProperty};
use crate::spelling::Spelled;

/// Publish the serial elements the enabled plugins commit to for each source class.
pub(super) fn publish_plugin_class_records(index: &mut ResolvedModuleIndex, table: &SymbolTable) {
    if table.native_plugins.is_empty() {
        return;
    }
    let host = table.native_plugins.host("main");
    let mut records = Vec::new();
    for raw in 0..index.declaration_count() {
        let declaration = DeclarationId::from_raw(
            u32::try_from(raw).expect("too many stable declarations for a packed id"),
        );
        if index.is_body_local_declaration(declaration) {
            continue;
        }
        let Some(class) = declared_class(index, declaration) else {
            continue;
        };
        let class = FrontendDeclaredClass {
            kind: class.kind,
            is_value: class.is_value,
            applied_annotations: index.declaration_applied_annotations(declaration),
            superclass: class.superclass,
            properties: &class.properties,
        };
        if let Some(record) = host.declared_serializable_elements(&class) {
            records.push((declaration, record));
        }
    }
    for (declaration, record) in records {
        index.publish_serializable_class_elements(declaration, record);
    }
}

struct DeclaredClass {
    kind: crate::libraries::TypeKind,
    is_value: bool,
    superclass: Option<crate::types::Ty>,
    properties: Vec<FrontendDeclaredProperty>,
}

fn declared_class(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
) -> Option<DeclaredClass> {
    let classifier = index.classifier_header(declaration)?;
    let flags = index.declaration_header(declaration)?.flags;
    let kind = if flags.has(DeclarationFlags::ANNOTATION_CLASS) {
        crate::libraries::TypeKind::Annotation
    } else if flags.has(DeclarationFlags::ENUM) {
        crate::libraries::TypeKind::Enum
    } else if flags.has(DeclarationFlags::SINGLETON) {
        crate::libraries::TypeKind::Object
    } else if flags.has(DeclarationFlags::INTERFACE) {
        crate::libraries::TypeKind::Interface
    } else {
        crate::libraries::TypeKind::Class
    };
    let any = crate::types::type_name("kotlin/Any");
    let superclass = classifier
        .superclass
        .map(|superclass| superclass.get())
        .filter(|superclass| superclass.kotlin_class_internal() != Some(any));
    let owned = index.owned_declarations(declaration);
    let primary = owned.iter().copied().find(|&child| {
        index.declaration_anchor(child).is_some_and(|anchor| {
            anchor.kind == DeclarationKind::Constructor && anchor.sibling == 0
        })
    });
    let primary_defaults = primary
        .and_then(|constructor| {
            let callable = index.callable_for_declaration(constructor)?.id;
            let defaults = index.callable_default_bitmap(callable)?;
            Some(
                defaults
                    .into_iter()
                    .enumerate()
                    .filter_map(|(ordinal, has_default)| {
                        let name = index.callable_parameter_name(callable, ordinal as u32)?;
                        Some((Box::<str>::from(name), has_default))
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap_or_default();
    let mut properties = Vec::new();
    for &child in owned {
        let Some(header) = index.declaration_header(child) else {
            continue;
        };
        if header.kind != DeclarationKind::Property {
            continue;
        }
        let name: Box<str> = index.declaration_name(child)?.into();
        let constructor_property = header.flags.has(DeclarationFlags::PROPERTY_PARAMETER);
        let has_default = if constructor_property {
            primary_defaults
                .iter()
                .find(|(parameter, _)| *parameter == name)?
                .1
        } else {
            header.flags.has(DeclarationFlags::HAS_INITIALIZER)
        };
        properties.push(FrontendDeclaredProperty {
            ty: index.signature(child)?.result.get(),
            annotations: index
                .declaration_applied_annotations(child)
                .iter()
                .map(|annotation| annotation.annotation)
                .collect(),
            has_backing_field: has_backing_field(header.flags),
            has_default,
            type_annotated: type_carries_annotations(index, child),
            name,
        });
    }
    Some(DeclaredClass {
        kind,
        is_value: flags.has(DeclarationFlags::VALUE) || flags.has(DeclarationFlags::VALUE_KEYWORD),
        superclass,
        properties,
    })
}

/// A stored property: one that keeps a backing field.
fn has_backing_field(flags: DeclarationFlags) -> bool {
    if flags.has(DeclarationFlags::PROPERTY_PARAMETER) {
        return true;
    }
    if [
        DeclarationFlags::ABSTRACT,
        DeclarationFlags::EXTERNAL,
        DeclarationFlags::DELEGATED,
    ]
    .into_iter()
    .any(|flag| flags.has(flag))
    {
        return false;
    }
    !flags.has(DeclarationFlags::CUSTOM_GETTER)
        || [
            DeclarationFlags::GETTER_READS_BACKING_FIELD,
            DeclarationFlags::HAS_INITIALIZER,
            DeclarationFlags::EXPLICIT_BACKING_FIELD,
        ]
        .into_iter()
        .any(|flag| flags.has(flag))
}

/// Whether `property`'s written type carries a type-use annotation anywhere, or its spelling is
/// not settled yet. An inferred type has no spelling and carries none.
fn type_carries_annotations(index: &ResolvedModuleIndex, property: DeclarationId) -> bool {
    if index.declaration_spellings_deferred(property) {
        return true;
    }
    index
        .declaration_spellings(property)
        .is_some_and(|spellings| spelled_carries_annotations(&spellings.ret))
}

fn spelled_carries_annotations(spelled: &Spelled) -> bool {
    !spelled.annotations.is_empty()
        || !spelled.expansion_annotations.is_empty()
        || spelled.args.iter().any(spelled_carries_annotations)
        || spelled
            .alias_args
            .iter()
            .any(|(_, argument)| spelled_carries_annotations(argument))
}

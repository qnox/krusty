//! Serializers a property's declared type names for itself and for its type arguments.
//!
//! `val items: List<@Serializable(with = S::class) Item>` serializes each element with `S`, whatever
//! `Item`'s own serializer is (it may have none). The frontend records every type-use annotation on
//! the property's checked declared spelling ([`Spelled`]), the same tree `@Metadata` encodes; this
//! module reads the checked `@Serializable(with = …)` applications off that tree, and the
//! element-serializer derivation walks it in step with the checked type's arguments
//! ([`Spelled::arg`]).
//!
//! Below a typealias application the tree already describes the EXPANDED type: [`Spelled::args`]
//! places each use-site argument's spelling at the expansion position the alias substitutes it
//! into (`typealias Named<V> = Map<String, V>` puts `Named<@A Item>`'s `@A` on the map's value),
//! and [`Spelled::expansion_annotations`] carries the annotations the alias's right-hand side wrote
//! on its root (`typealias Coded = @Serializable(with = S::class) Item`).

use super::SERIALIZABLE_FQ;
use crate::ir::Spelled;
use crate::ir::{ClassId, IrFile};
use crate::plugins::PluginContext;
use crate::types::{type_name, AnnotationValue, TypeName};

/// The checked declared spelling of property `property` of `class`, detached from `ir` so the
/// derivation can keep reading it while it emits into `ir`. A property that spelled no alias and
/// recorded no type-use annotation has the empty spelling.
pub(super) fn declared_type_spelling(
    ctx: &PluginContext,
    ir: &IrFile,
    class: ClassId,
    property: &str,
) -> Spelled {
    ctx.property_declared_type(ir, class, property).clone()
}

/// The serializer class a checked `@Serializable(with = …)` on the spelled type itself names: the
/// occurrence's own annotation, else the one an alias's right-hand side wrote on its root.
pub(super) fn named_serializer(spelled: &Spelled) -> Option<TypeName> {
    let serializable = type_name(SERIALIZABLE_FQ);
    spelled
        .annotations
        .iter()
        .chain(&spelled.expansion_annotations)
        .map(|annotation| annotation.checked())
        .filter(|annotation| annotation.annotation == serializable)
        .flat_map(|annotation| &annotation.arguments)
        .find_map(|(name, value)| match value {
            AnnotationValue::Class(serializer) if name == "with" => {
                serializer.kotlin_class_internal()
            }
            _ => None,
        })
}

/// Every serializer class a checked `@Serializable(with = …)` names anywhere in `spelled`,
/// including below a typealias application.
pub(in crate::plugins) fn named_serializers(spelled: &Spelled, out: &mut Vec<TypeName>) {
    out.extend(named_serializer(spelled));
    for argument in &spelled.args {
        named_serializers(argument, out);
    }
}

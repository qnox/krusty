//! kotlinc's frontend rule for a `@kotlinx.serialization.Transient` property: it must have an
//! initializer.
//!
//! A transient property is not a serial element (`serial_elements`), so the deserialization
//! constructor has no argument for it and runs its initializer instead. The plugin's FIR class
//! checker (`TRANSIENT_MISSING_INITIALIZER`) therefore rejects a transient property that stores a
//! value but has neither an initializer (a constructor property's default, a body property's
//! initializer) nor `lateinit`. It reports the whole declaration, modifiers and annotations
//! included.
//!
//! The rule belongs to the classes whose serializer the plugin builds from their properties:
//! `@Serializable` without a custom serializer, and neither an object nor an enum. An abstract or a
//! sealed class is checked too.

use crate::libraries::TypeKind;
use crate::plugins::{FrontendClassCheckContext, FrontendPluginDiagnostic};
use crate::types::type_name;

use super::generated_classifier::serializable_by_plugin;
use super::{PluginRelease, TRANSIENT_FQ};

/// The plugin's wording for `TRANSIENT_MISSING_INITIALIZER`, which follows the plugin release, not the
/// target Kotlin version: the 2.4.20 plugin ends the sentence with a full stop. Without a known
/// release it is the newest wording.
fn transient_missing_initializer(release: Option<PluginRelease>) -> &'static str {
    if release.is_none_or(|release| release >= PluginRelease::V2_4_20) {
        "this property is marked as @Transient and therefore must have an initializing expression."
    } else {
        "this property is marked as @Transient and therefore must have an initializing expression"
    }
}

pub(super) fn missing_initializers<'a>(
    ctx: &'a FrontendClassCheckContext<'a>,
    release: Option<PluginRelease>,
) -> impl Iterator<Item = FrontendPluginDiagnostic> + 'a {
    let message = transient_missing_initializer(release);
    let checked = matches!(ctx.kind, TypeKind::Class)
        && serializable_by_plugin(ctx.annotations, ctx.annotation_class_arguments);
    let transient = type_name(TRANSIENT_FQ);
    ctx.properties
        .iter()
        .filter(move |property| {
            checked
                && property.annotations.contains(&transient)
                && property.has_backing_field
                && !property.has_initializer
                && !property.is_lateinit
        })
        .map(|property| FrontendPluginDiagnostic {
            span: property.declaration_span,
            message,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;
    use crate::plugins::FrontendPropertyFacts;

    use super::super::SERIALIZABLE_FQ;

    fn property(
        annotations: &[&str],
        has_initializer: bool,
        is_lateinit: bool,
    ) -> FrontendPropertyFacts {
        FrontendPropertyFacts {
            annotations: annotations.iter().map(|name| type_name(name)).collect(),
            has_backing_field: true,
            has_initializer,
            is_lateinit,
            declaration_span: Span::new(10, 20),
        }
    }

    fn reported(
        kind: TypeKind,
        class_arguments: &[(u32, crate::types::TypeName)],
        properties: &[FrontendPropertyFacts],
    ) -> Vec<FrontendPluginDiagnostic> {
        let annotations = [type_name(SERIALIZABLE_FQ)];
        missing_initializers(
            &FrontendClassCheckContext {
                kind,
                annotations: &annotations,
                annotation_class_arguments: class_arguments,
                properties,
            },
            None,
        )
        .collect()
    }

    #[test]
    fn only_a_transient_property_without_a_value_is_rejected() {
        let properties = [
            property(&[TRANSIENT_FQ], false, false),
            property(&[TRANSIENT_FQ], true, false),
            property(&[TRANSIENT_FQ], false, true),
            property(&["demo/Transient"], false, false),
        ];
        assert_eq!(
            reported(TypeKind::Class, &[], &properties),
            vec![FrontendPluginDiagnostic {
                span: Span::new(10, 20),
                message: transient_missing_initializer(None),
            }]
        );
    }

    #[test]
    fn the_wording_follows_the_plugin_release() {
        let earlier =
            "this property is marked as @Transient and therefore must have an initializing expression";
        let full_stop = format!("{earlier}.");
        assert_eq!(
            transient_missing_initializer(PluginRelease::parse("2.4.10-release-377")),
            earlier
        );
        assert_eq!(
            transient_missing_initializer(PluginRelease::parse("2.4.20")),
            full_stop
        );
        assert_eq!(transient_missing_initializer(None), full_stop);
    }

    #[test]
    fn a_class_the_plugin_does_not_build_from_its_properties_is_not_checked() {
        let properties = [property(&[TRANSIENT_FQ], false, false)];
        assert!(reported(TypeKind::Object, &[], &properties).is_empty());
        assert!(reported(TypeKind::Enum, &[], &properties).is_empty());
        assert!(reported(
            TypeKind::Class,
            &[(0, type_name("demo/CustomSerializer"))],
            &properties
        )
        .is_empty());
    }
}

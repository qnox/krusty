//! JVM storage of an already-resolved Kotlin singleton classifier.
//!
//! Providers normalize declaration ownership into backend classifier facts. This operation maps
//! those facts to one physical JVM field; callers do not retry module, classpath, and spelling
//! representations independently.

use crate::backend::BackendClassifierSource;
use crate::libraries::TypeKind;
use crate::types::{type_name, TypeName};

pub(super) fn of(
    classifiers: &dyn BackendClassifierSource,
    classifier: TypeName,
) -> Option<(TypeName, String)> {
    if let Some(owner) = super::jvm_class_map::intrinsic_companion_jvm_class(classifier) {
        return Some((type_name(&owner), "INSTANCE".to_string()));
    }

    let declaration = classifiers.classifier(classifier)?;
    if declaration.kind != TypeKind::Object {
        return None;
    }
    if let Some(holder) = classifier.nested_owner() {
        if let Some(holder_declaration) = classifiers.classifier(holder) {
            if let Some((field, companion)) = holder_declaration.companion.as_ref() {
                if *companion == classifier {
                    return Some((holder, field.to_string()));
                }
            }
        }
    }
    Some((classifier, "INSTANCE".to_string()))
}

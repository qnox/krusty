//! Which JVM field a singleton value loads while one class is being emitted.
//!
//! The published field of an interface companion is the interface's `Companion` field. That field
//! is assigned only after the companion `<clinit>` returns, so the companion and a class declared
//! in its initializer load `$$INSTANCE`, which that `<clinit>` stores first.

use crate::ir::IrFile;
use crate::types::TypeName;

/// The field a resolved singleton publishes for callers outside its own class.
pub(super) struct PublishedSingleton {
    pub(super) owner: TypeName,
    pub(super) field: String,
}

pub(super) fn published_singleton(
    ir: &IrFile,
    classifier: TypeName,
    dependency: Option<(TypeName, String)>,
) -> Option<PublishedSingleton> {
    if let Some(declaration) = ir.referenced_module_classifiers.get(&classifier).copied() {
        if !declaration.singleton {
            return None;
        }
        return Some(if let Some(owner) = declaration.companion_owner {
            PublishedSingleton {
                owner,
                field: classifier.nested_segment_ref().to_owned(),
            }
        } else {
            PublishedSingleton {
                owner: classifier,
                field: "INSTANCE".to_string(),
            }
        });
    }
    if let Some(class) = ir
        .classes
        .iter()
        .find(|class| class.fq_name == classifier && class.is_singleton())
    {
        return Some(if class.is_companion {
            PublishedSingleton {
                owner: classifier.nested_owner()?,
                field: classifier.nested_segment_ref().to_owned(),
            }
        } else {
            PublishedSingleton {
                owner: classifier,
                field: "INSTANCE".to_string(),
            }
        });
    }
    dependency.map(|(owner, field)| PublishedSingleton { owner, field })
}

/// The interface companion whose `$$INSTANCE` this emission should load for a self-read.
///
/// That is the class being emitted when it is the companion, and the companion that encloses a
/// class declared in its initializer. An anonymous object there is its own class, but it still
/// runs before the interface's `Companion` field is assigned.
pub(super) fn self_companion(ir: &IrFile, emitted_owner: &str) -> Option<TypeName> {
    let class = ir
        .classes
        .iter()
        .find(|class| class.fq_name_matches(emitted_owner))?;
    if super::companion_of_interface(ir, class) {
        return Some(class.fq_name);
    }
    let crate::ir::IrEnclosure::ClassInitializer(owner) = class.enclosure? else {
        return None;
    };
    let companion = ir.classes.get(owner as usize)?;
    super::companion_of_interface(ir, companion).then_some(companion.fq_name)
}

/// The field actually loaded for `singleton`.
///
/// A self-read of the companion being emitted, or of the companion whose initializer encloses the
/// class being emitted, uses `$$INSTANCE`. Every other read uses the published field.
pub(super) fn instance_load(
    ir: &IrFile,
    singleton: TypeName,
    emitted_owner: &str,
    published: PublishedSingleton,
) -> (TypeName, String) {
    loaded_instance(self_companion(ir, emitted_owner), singleton, published)
}

pub(super) fn loaded_instance(
    self_companion: Option<TypeName>,
    singleton: TypeName,
    published: PublishedSingleton,
) -> (TypeName, String) {
    if self_companion.is_some_and(|companion| companion == singleton) {
        (singleton, "$$INSTANCE".to_string())
    } else {
        (published.owner, published.field)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    fn published_interface_field() -> PublishedSingleton {
        PublishedSingleton {
            owner: type_name("Test"),
            field: "Companion".to_string(),
        }
    }

    #[test]
    fn an_interface_companion_loads_its_own_instance_field() {
        let companion = type_name("Test$Companion");
        let (owner, field) =
            loaded_instance(Some(companion), companion, published_interface_field());
        assert_eq!(owner, companion);
        assert_eq!(field, "$$INSTANCE");
    }

    #[test]
    fn an_external_caller_loads_the_published_companion_field() {
        let companion = type_name("Test$Companion");
        let interface = type_name("Test");
        let (owner, field) = loaded_instance(None, companion, published_interface_field());
        assert_eq!(owner, interface);
        assert_eq!(field, "Companion");
    }

    #[test]
    fn another_singleton_inside_the_companion_keeps_its_published_field() {
        let other = type_name("Other");
        let (owner, field) = loaded_instance(
            Some(type_name("Test$Companion")),
            other,
            PublishedSingleton {
                owner: other,
                field: "INSTANCE".to_string(),
            },
        );
        assert_eq!(owner, other);
        assert_eq!(field, "INSTANCE");
    }
}

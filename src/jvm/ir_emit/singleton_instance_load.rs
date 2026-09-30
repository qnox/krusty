//! Which JVM field a singleton value loads while one class is being emitted.
//!
//! The published field of an interface companion is the interface's `Companion` field. That field
//! is assigned only after the companion `<clinit>` returns, so code of the companion itself loads
//! `$$INSTANCE`, which that `<clinit>` stores first. The emitted class's role is decided once per
//! emitter; each read only applies it.

use crate::ir::IrFile;
use crate::types::TypeName;

/// Whether the class whose bytecode this emitter is writing is an interface companion.
pub(super) fn emitted_class_is_interface_companion(ir: &IrFile, owner: &str) -> bool {
    ir.classes
        .iter()
        .any(|class| class.fq_name_matches(owner) && super::companion_of_interface(ir, class))
}

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

/// The field actually loaded for `singleton`.
///
/// `interface_companion_self` is the role of the class being emitted. A self-read of that
/// companion uses `$$INSTANCE`; every other read uses the published field.
pub(super) fn instance_load(
    interface_companion_self: bool,
    singleton: TypeName,
    emitted_owner: &str,
    published: PublishedSingleton,
) -> (TypeName, String) {
    if interface_companion_self && singleton.matches(emitted_owner) {
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
        let (owner, field) = instance_load(
            true,
            companion,
            &companion.render(),
            published_interface_field(),
        );
        assert_eq!(owner, companion);
        assert_eq!(field, "$$INSTANCE");
    }

    #[test]
    fn an_external_caller_loads_the_published_companion_field() {
        let companion = type_name("Test$Companion");
        let interface = type_name("Test");
        let (owner, field) = instance_load(false, companion, "MainKt", published_interface_field());
        assert_eq!(owner, interface);
        assert_eq!(field, "Companion");
    }

    #[test]
    fn another_singleton_inside_the_companion_keeps_its_published_field() {
        let other = type_name("Other");
        let (owner, field) = instance_load(
            true,
            other,
            &type_name("Test$Companion").render(),
            PublishedSingleton {
                owner: other,
                field: "INSTANCE".to_string(),
            },
        );
        assert_eq!(owner, other);
        assert_eq!(field, "INSTANCE");
    }
}

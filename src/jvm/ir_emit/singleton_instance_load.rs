//! Which JVM field a singleton value loads while one class is being emitted.
//!
//! The published field of an interface companion is the interface's `Companion` field. That field
//! is assigned only after the companion `<clinit>` returns, so the companion and a class declared
//! in its initializer load `$$INSTANCE`, which that `<clinit>` stores first.

use crate::ir::{IrEnclosure, IrFile};
use crate::jvm::private_static_access::StaticOwner;
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

/// The interface companion whose `$$INSTANCE` code of `owner` loads for a self-read.
///
/// That is `owner` when it is the companion, and the companion that encloses a class declared in
/// its initializer. An anonymous object there is its own class, but it still runs before the
/// interface's `Companion` field is assigned. The emitter caches this once from [`StaticOwner`].
pub(super) fn self_companion(ir: &IrFile, owner: Option<StaticOwner>) -> Option<TypeName> {
    let StaticOwner::Class(name) = owner? else {
        return None;
    };
    let class_id = ir.class_id_by_name(name)?;
    let class = ir.classes.get(class_id as usize)?;
    if super::companion_of_interface(ir, class) {
        return Some(class.fq_name);
    }
    let IrEnclosure::ClassInitializer(enclosing) = class.enclosure? else {
        return None;
    };
    let companion = ir.classes.get(enclosing as usize)?;
    super::companion_of_interface(ir, companion).then_some(companion.fq_name)
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
    fn an_initializer_object_uses_the_enclosing_interface_companion() {
        use crate::ir::IrClass;
        let mut ir = IrFile::default();
        let interface_name = type_name("Test");
        let companion_name = type_name("Test$Companion");
        let mut interface = IrClass::synthetic(interface_name);
        interface.is_interface = true;
        interface.companion_class = Some(companion_name);
        ir.add_class(interface);
        let mut companion = IrClass::synthetic(companion_name);
        companion.is_companion = true;
        let companion_id = ir.add_class(companion);
        let anonymous_name = type_name("Test$Companion$anonObject$1");
        let mut anonymous = IrClass::synthetic(anonymous_name);
        anonymous.enclosure = Some(IrEnclosure::ClassInitializer(companion_id));
        ir.add_class(anonymous);
        let mut holder = IrClass::synthetic(type_name("Holder"));
        holder.companion_class = Some(type_name("Holder$Companion"));
        ir.add_class(holder);
        let class_companion = type_name("Holder$Companion");
        let mut class_companion_class = IrClass::synthetic(class_companion);
        class_companion_class.is_companion = true;
        ir.add_class(class_companion_class);

        assert_eq!(
            self_companion(&ir, Some(StaticOwner::Class(companion_name))),
            Some(companion_name)
        );
        assert_eq!(
            self_companion(&ir, Some(StaticOwner::Class(anonymous_name))),
            Some(companion_name)
        );
        assert_eq!(self_companion(&ir, Some(StaticOwner::Facade)), None);
        assert_eq!(
            self_companion(&ir, Some(StaticOwner::Class(interface_name))),
            None
        );
        assert_eq!(
            self_companion(&ir, Some(StaticOwner::Class(class_companion))),
            None
        );
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

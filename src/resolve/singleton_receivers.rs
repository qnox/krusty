use crate::symbol_resolver::{ClassifierCompanionInstance, SymbolResolver};
use crate::types::{Ty, TypeName};

use super::SingletonValue;

/// Complete singleton value denoted by a classifier identity. A class with a companion denotes
/// that nested singleton; an object denotes itself. This records the Kotlin value receiver;
/// target storage (including a JVM static field) is chosen only after common lowering.
pub(super) fn classifier_singleton_value(
    resolver: &SymbolResolver<'_>,
    internal: TypeName,
) -> Option<SingletonValue> {
    let classifier = resolver.classifier(internal)?;
    if classifier.is_object() {
        return Some(SingletonValue {
            classifier: internal,
        });
    }
    let (_, companion) = classifier.companion_object.clone()?;
    Some(SingletonValue {
        classifier: companion,
    })
}

/// Preserve the exact runtime identity of a singleton selected from the implicit receiver tower.
/// A receiver of class `C` remains a scoped `C` instance even when `C` has a companion value.
pub(super) fn implicit_singleton_value(
    resolver: &SymbolResolver<'_>,
    ty: Ty,
    current: bool,
    static_singleton_this: Option<&SingletonValue>,
    static_companion_this: Option<&ClassifierCompanionInstance>,
) -> Option<SingletonValue> {
    // The nearest receiver normally has a real `this` slot. Constructor headers and delegation
    // arguments instead install an enclosing object or target companion before the dispatch
    // instance exists. Return that retained identity directly so lowering never reads the
    // half-built instance or repeats classifier lookup.
    if current {
        return static_this_singleton(ty, static_singleton_this, static_companion_this);
    }
    let classifier = ty.non_null().obj_internal()?;
    classifier_singleton_value(resolver, classifier)
        .filter(|singleton| singleton.classifier == classifier)
}

/// The retained singleton denoted by a constructor-header `this` that has no runtime slot.
fn static_this_singleton(
    ty: Ty,
    static_singleton_this: Option<&SingletonValue>,
    static_companion_this: Option<&ClassifierCompanionInstance>,
) -> Option<SingletonValue> {
    if let Some(instance) =
        static_singleton_this.filter(|instance| ty == Ty::obj_name(instance.classifier))
    {
        return Some(instance.clone());
    }
    static_companion_this
        .filter(|instance| ty == Ty::obj_name(instance.companion))
        .map(|instance| SingletonValue {
            classifier: instance.companion,
        })
}

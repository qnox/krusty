//! The classifier a bare class literal of a generic classifier denotes.
//!
//! `H::class` for `class H<T>` is `KClass<H<*>>`: kotlinc star-projects every type parameter the
//! classifier declares, its own and those it captures from an enclosing `inner` chain, so
//! `H::class.java` is `Class<H<*>>`. The raw `H` is not a Kotlin type.

use crate::libraries::LibraryType;
use crate::types::Ty;

/// `receiver`, the unbound left-hand side of a class literal, star-projected over the type
/// parameters `classifier` declares when it was written without type arguments. Each star keeps
/// the parameter's declared upper bound, as a written `H<*>` does.
pub(super) fn star_projected_class_literal_receiver(receiver: Ty, classifier: &LibraryType) -> Ty {
    let Some((internal, [])) = receiver.obj_parts() else {
        return receiver;
    };
    // A bare `Array::class` is its own language feature (`BareArrayClassLiteral`).
    if receiver.is_reference_array() {
        return receiver;
    }
    super::star_projected_classifier::star_projected_classifier(internal, classifier)
}

//! The classifier a bare class literal of a generic classifier denotes.
//!
//! `H::class` for `class H<T>` is `KClass<H<*>>`: kotlinc star-projects every type parameter the
//! classifier declares, its own and those it captures from an enclosing `inner` chain, so
//! `H::class.java` is `Class<H<*>>`. The raw `H` is not a Kotlin type.

use crate::libraries::LibraryType;
use crate::types::Ty;

use super::projected_classifier_argument_bindings;

/// `receiver`, the unbound left-hand side of a class literal, star-projected over the type
/// parameters `classifier` declares when it was written without type arguments. Each star keeps
/// the parameter's declared upper bound, as a written `H<*>` does.
pub(super) fn star_projected_class_literal_receiver(receiver: Ty, classifier: &LibraryType) -> Ty {
    let Ty::Obj(internal, []) = receiver else {
        return receiver;
    };
    // A bare `Array::class` is its own language feature (`BareArrayClassLiteral`).
    if receiver.is_reference_array() {
        return receiver;
    }
    let type_params = classifier.type_params();
    if type_params.is_empty() {
        return receiver;
    }
    let bindings =
        projected_classifier_argument_bindings(type_params, &vec![None; type_params.len()]);
    let arguments = (0..type_params.len())
        .map(|index| {
            let upper_bound = classifier
                .type_param_bounds()
                .get(index)
                .and_then(|bounds| bounds.first())
                .map(|&bound| crate::symbol_resolver::ty_subst_keep_unbound(bound, &bindings))
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            Ty::star_projection(upper_bound)
        })
        .collect::<Vec<_>>();
    Ty::obj_args_name(internal, &arguments)
}

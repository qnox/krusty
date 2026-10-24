//! A generic classifier applied to a star projection of every type parameter it declares.
//!
//! Kotlin has no raw types. Where a use site needs an application of a generic classifier
//! without knowing its arguments (`H<*>` for a bare class literal, `KProperty<*>` for a delegated
//! property's reference), each parameter is star-projected and keeps its declared upper bound, as a
//! written `H<*>` does.

use crate::libraries::LibraryType;
use crate::types::{Ty, TypeName};

use super::projected_classifier_argument_bindings;

/// `internal` star-projected over the type parameters `classifier` declares; `internal` itself
/// when it declares none.
pub(super) fn star_projected_classifier(internal: TypeName, classifier: &LibraryType) -> Ty {
    let type_params = classifier.type_params();
    if type_params.is_empty() {
        return Ty::obj_name(internal);
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

//! The denotable type a declaration publishes for an inferred intersection.
//!
//! Checked IR keeps an inferred `A & B` complete; metadata has no intersection type, so each
//! writer approximates it at the serialization boundary with the variances of the checked
//! classifier facts. JVM `@Metadata` and a KLIB `PackageFragment` publish the same declaration
//! types, so both use this one approximation.

use crate::backend::BackendClassifierSource;
use crate::types::Ty;

/// `ty` as the declaration publishes it, or `None` when a classifier it applies has no fact.
pub fn approximate(symbols: &dyn BackendClassifierSource, ty: Ty) -> Option<Ty> {
    crate::types::declaration_approximation(ty, &mut |classifier, index| {
        symbols
            .classifier(classifier)
            .and_then(|declaration| declaration.type_param_variances.get(index).copied())
    })
}

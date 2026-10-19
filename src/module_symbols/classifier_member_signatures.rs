//! Provider-normalized generic signatures for members owned by source classifiers.

use crate::frontend::FrontendClassSig;
use crate::libraries::{GenericReturnPolicy, GenericSig, PropertyInfo};
use crate::types::Ty;

pub(super) fn classifier_member_generic_signature(
    class: &FrontendClassSig,
    existing: Option<GenericSig>,
    receiver: Option<Ty>,
    params: &[Ty],
    ret: Ty,
) -> Option<GenericSig> {
    let formals = class
        .type_params
        .iter()
        .chain(class.captured_type_parameters.type_params.iter())
        .cloned()
        .collect::<Vec<_>>();
    let mentions_classifier_parameter = receiver
        .into_iter()
        .chain(params.iter().copied())
        .chain(std::iter::once(ret))
        .any(|ty| crate::types::ty_mentions_param(ty, &formals));
    let mut signature = existing.or_else(|| {
        mentions_classifier_parameter.then(|| GenericSig {
            // Classifier parameters are bound by the dispatch receiver, not inferred as callable
            // parameters. An empty method-formal list matches metadata's `()TT;` declaration.
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver,
            params: params.to_vec(),
            ret,
            return_policy: GenericReturnPolicy::Exact,
        })
    })?;
    if signature.receiver.is_none() {
        signature.receiver = receiver;
    }
    Some(signature)
}

pub(super) fn normalize_classifier_property_generic_signature(
    class: &FrontendClassSig,
    property: &mut PropertyInfo,
) {
    property.getter.generic_sig = classifier_member_generic_signature(
        class,
        property
            .getter
            .generic_sig
            .take()
            .map(|signature| *signature),
        None,
        &property.getter.params,
        property.getter.ret,
    )
    .map(Box::new);
    if let Some(setter) = &mut property.setter {
        setter.generic_sig = classifier_member_generic_signature(
            class,
            setter.generic_sig.take().map(|signature| *signature),
            None,
            &setter.params,
            setter.ret,
        )
        .map(Box::new);
    }
}

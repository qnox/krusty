//! Value-parameter facts published for one callable identity.
//!
//! Every fact here is what the parameter WROTE. None of it is reconstructed from the resolved type
//! the parameter was given: `vararg` and `noinline` are modifiers, and a parameter that wrote one
//! has the same semantic type as the parameter beside it that did not.

use crate::fir::{
    DeclarationId, HeaderParameter, HeaderParameterRange, LookupNameId, StreamedHeaderModule,
};

/// The declaration behavior one compact header parameter publishes.
pub(super) fn published_flags(
    parameter: &HeaderParameter,
) -> crate::fir::ResolvedValueParameterFlags {
    crate::fir::ResolvedValueParameterFlags::new(
        parameter.flags.is_vararg(),
        parameter.flags.has_default(),
        parameter.flags.is_property(),
        parameter.flags.is_mutable_property(),
    )
    .with_materialized_lambda(parameter.flags.materializes_its_lambda())
    .with_crossinline(parameter.flags.is_crossinline())
    .with_context_kind(parameter.context_kind)
}

/// Exact parameter identities published by a generated property accessor.
///
/// Context identities come from their typed header facts. A source-written setter parameter keeps
/// its spelling; an implicit setter carries only its semantic role and deliberately has no source
/// name for later phases to rediscover.
pub(super) fn property_accessor_parameters(
    headers: &StreamedHeaderModule,
    context_parameters: HeaderParameterRange,
    setter_parameter_name: Option<LookupNameId>,
    is_setter: bool,
) -> Vec<(&str, crate::fir::ResolvedValueParameterFlags)> {
    let mut parameters = headers
        .syntax
        .parameters(context_parameters)
        .iter()
        .map(|parameter| {
            (
                headers
                    .lookup_names
                    .get(parameter.name)
                    .expect("an accessor context parameter must retain its spelling"),
                published_flags(parameter),
            )
        })
        .collect::<Vec<_>>();
    if is_setter {
        let source_name = setter_parameter_name.and_then(|name| headers.lookup_names.get(name));
        parameters.push(match source_name {
            Some(name) => (
                name,
                crate::fir::ResolvedValueParameterFlags::new(false, false, false, false),
            ),
            None => (
                "",
                crate::fir::ResolvedValueParameterFlags::new(false, false, false, false)
                    .with_property_setter_value(true),
            ),
        });
    }
    parameters
}

/// The compact header parameters of one declaration, in source order, or an empty slice when the
/// declaration owns no callable header.
///
/// A publication driven by a resolved `Signature` still reaches for this: a `Signature` carries the
/// parameter's semantic type and nothing about the modifiers written beside it, so reading the
/// header is the only way such a path can publish the same facts as every other.
pub(super) fn header_parameters(
    headers: &StreamedHeaderModule,
    declaration: DeclarationId,
) -> &[HeaderParameter] {
    match headers
        .syntax
        .declaration(declaration)
        .map(|header| header.kind)
    {
        Some(crate::fir::HeaderDeclarationKind::Callable { parameters, .. }) => {
            headers.syntax.parameters(parameters)
        }
        _ => &[],
    }
}

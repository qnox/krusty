//! Value-parameter facts published for one callable identity.
//!
//! Every fact here is what the parameter WROTE. None of it is reconstructed from the resolved type
//! the parameter was given: `vararg` and `noinline` are modifiers, and a parameter that wrote one
//! has the same semantic type as the parameter beside it that did not.

use crate::fir::{DeclarationId, HeaderParameter, StreamedHeaderModule};

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

//! JVM adapter for the target-neutral Kotlin metadata contract decoder.

use super::*;

/// A member's contract can name its class's type parameters (`this@Either is Left<A>`), so they
/// are in scope alongside the function's own.
pub(super) fn decode_function_contract(
    name: &str,
    function: &ParsedFunction,
    (class_type_parameters, class_type_parameter_bounds): (&[(u64, String)], &[Vec<Ty>]),
    (records, d2): (&[Rec], &[String]),
    type_table: Option<&[u8]>,
) -> MetadataResult<Option<std::sync::Arc<crate::contracts::Contract>>> {
    let Some(body) = function.contract_body.as_deref() else {
        return Ok(None);
    };
    crate::trace_compiler!(
        "metadata_contracts",
        "contract function={} value_params={} context_params={} context_receivers={}",
        name,
        function.value_params.len(),
        function.context_params.len(),
        function.context_receiver_bodies.len() + function.context_receiver_type_ids.len(),
    );
    let type_parameters = type_parameter_context(
        class_type_parameters,
        class_type_parameter_bounds,
        &function.type_params,
        records,
        d2,
        type_table,
    )
    .map(|context| context.names)
    .unwrap_or_default();
    decode_contract(body, records, d2, &type_parameters, type_table)
        .map(|contract| contract.map(std::sync::Arc::new))
}

pub(super) fn decode_contract(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
    type_parameters: &HashMap<u64, String>,
    type_table: Option<&[u8]>,
) -> MetadataResult<Option<crate::contracts::Contract>> {
    use crate::metadata::contract_decoder::ContractTypeRef;

    let mut resolve_type = |reference: crate::metadata::contract_decoder::ContractTypeRef<'_>| {
        let resolved = (|| {
            let (body, table_nullable) = match reference {
                ContractTypeRef::Inline(body) => (body, false),
                ContractTypeRef::Table(id) => type_table_entry(type_table?, id as usize)?,
            };
            decode_metadata_type(
                body,
                type_table,
                records,
                d2,
                type_parameters,
                &HashMap::new(),
                table_nullable,
                0,
            )
        })();
        resolved.ok_or_else(|| crate::metadata::decode::PackageFragmentDecodeError {
            offset: 0,
            detail: "cannot resolve contract is-instance type".to_string(),
        })
    };
    crate::metadata::contract_decoder::decode_contract(body, &mut resolve_type)
        .map_err(|_| MetadataDecodeError::InvalidContract)
}

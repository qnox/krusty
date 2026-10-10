//! JVM adapter for the target-neutral Kotlin metadata contract decoder.

use super::*;

pub(super) fn decode_contract(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
    type_parameters: &HashMap<u64, String>,
    type_table: Option<&[u8]>,
) -> MetadataResult<Option<crate::contracts::Contract>> {
    use crate::metadata::contract_decoder::ContractTypeRef;

    let mut resolve_type = |reference| {
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

//! The `@CCall.Direct` annotation of an interop library's function: the C symbol a direct call
//! realizes it with.

use super::SemanticTables;
use super::{annotation_applications, metadata, semantic_error, PackageFragmentDecodeError};

/// The annotation Kotlin/Native's cinterop puts on a function a direct C call realizes.
const CCALL_DIRECT: &str = "kotlinx/cinterop/internal/CCall$Direct";

/// The C function an interop library's `@CCall.Direct(name)` names on this function, if it carries
/// one. Only a function `annotations` already identifies as so annotated has its applications
/// decoded with their arguments.
pub(super) fn direct_c_symbol(
    body: &[u8],
    annotations: &[crate::types::TypeName],
    tables: &SemanticTables<'_>,
) -> Result<Option<String>, PackageFragmentDecodeError> {
    let direct = crate::types::type_name(CCALL_DIRECT);
    if !annotations.contains(&direct) {
        return Ok(None);
    }
    let applications = annotation_applications(
        body,
        &[12, 34, 150, 170, 171],
        tables.strings,
        tables.qnames,
        "function declaration",
    )?;
    let application = applications
        .iter()
        .find(|application| application.identity == direct)
        .ok_or_else(|| semantic_error("`@CCall.Direct` application did not decode"))?;
    match application.argument("name") {
        Some(metadata::AnnotationArgument::String(symbol)) => Ok(Some(symbol.clone())),
        _ => Err(semantic_error(
            "`@CCall.Direct` has no string `name` argument",
        )),
    }
}

//! Rendering for source-level visibility diagnostics after callable selection.
//!
//! Selection supplies one normalized semantic callable. The shared declaration renderer owns its
//! source-shaped function text; this module adds only the selected container and visibility. It
//! never performs name lookup or recovers a declaration from source spelling.

use crate::fir::ResolvedModuleIndex;
use crate::libraries::{CallSig, FunctionInfo, LibraryMember};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

pub(in crate::resolve) fn owner_display(owner: TypeName) -> String {
    owner.render().replace(['/', '$'], ".")
}

/// kotlinc's value-parameter list in an access diagnostic: a defaulted parameter shows `= ...`.
pub(in crate::resolve) fn parameter_display(
    call_sig: &CallSig,
    params: &[Ty],
    start: usize,
) -> String {
    params
        .iter()
        .enumerate()
        .map(|(offset, ty)| {
            let index = start + offset;
            let name = call_sig
                .param_names
                .get(index)
                .filter(|name| !name.is_empty())
                .cloned()
                .unwrap_or_else(|| format!("p{index}"));
            let default = if call_sig.param_defaults.get(index) == Some(&true) {
                " = ..."
            } else {
                ""
            };
            format!("{name}: {}{default}", ty.source_name())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn callable_message(
    resolved_index: Option<&ResolvedModuleIndex>,
    declaration: String,
    owner: TypeName,
    visibility: &str,
) -> String {
    let container = resolved_index
        .and_then(|index| {
            index
                .classifier_declaration(owner)
                .map(|declaration| (index, declaration))
        })
        .and_then(|(index, declaration)| index.declaration_header(declaration))
        .filter(|header| header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS))
        .map_or_else(
            || format!("'{}'", owner_display(owner)),
            |_| "file".to_string(),
        );
    format!("cannot access '{declaration}': it is {visibility} in {container}.")
}

pub(in crate::resolve) fn member_callable_message(
    source: &dyn SymbolSource,
    resolved_index: Option<&ResolvedModuleIndex>,
    member: &LibraryMember,
    owner: TypeName,
    visibility: &str,
) -> String {
    callable_message(
        resolved_index,
        super::super::declaration_rendering::declared_member(source, member),
        owner,
        visibility,
    )
}

pub(in crate::resolve) fn static_callable_message(
    source: &dyn SymbolSource,
    resolved_index: Option<&ResolvedModuleIndex>,
    function: &FunctionInfo,
    owner: TypeName,
    visibility: &str,
) -> String {
    callable_message(
        resolved_index,
        super::super::declaration_rendering::declared_static_function(source, function),
        owner,
        visibility,
    )
}

pub(in crate::resolve) fn function_callable_message(
    source: &dyn SymbolSource,
    resolved_index: Option<&ResolvedModuleIndex>,
    function: &FunctionInfo,
    owner: TypeName,
    visibility: &str,
) -> String {
    callable_message(
        resolved_index,
        super::super::declaration_rendering::access_function(source, function),
        owner,
        visibility,
    )
}

pub(in crate::resolve) fn constructor_message(
    source: &dyn SymbolSource,
    owner: TypeName,
    member: &LibraryMember,
) -> String {
    let visibility = match member.visibility {
        crate::types::Visibility::Private => "private",
        crate::types::Visibility::Protected => "protected",
        crate::types::Visibility::Internal => "internal",
        crate::types::Visibility::PackagePrivate => "package-private",
        crate::types::Visibility::Public => "public",
    };
    format!(
        "cannot access '{}': it is {visibility} in '{}'.",
        super::super::declaration_rendering::declared_constructor(source, member),
        owner_display(owner),
    )
}

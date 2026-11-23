//! The parameter names a Java declaration wrote, as its class file records them.

/// The names a Java class file's `MethodParameters` gives the logical parameters: those after the
/// enclosing-instance prefix, provided every one is named and they are as many as the parameters.
/// Anything less is a declaration whose names are unknown, not a partly known one.
pub(super) fn declared_parameter_names(
    physical: &[Option<String>],
    enclosing_prefix: usize,
    logical_count: usize,
) -> Vec<String> {
    let names = physical
        .iter()
        .skip(enclosing_prefix)
        .cloned()
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default();
    if names.len() == logical_count {
        names
    } else {
        Vec::new()
    }
}

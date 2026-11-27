//! The parameter names a Java declaration wrote, as its class file records them.

use crate::jvm::classreader::MethodSig;
use crate::libraries::LibraryMember;

/// Publish the names `method`'s `MethodParameters` gives `member`'s logical parameters: those after
/// the enclosing-instance prefix, provided every one is named and they are as many as the
/// parameters. Anything less is a declaration whose names are unknown, not a partly known one.
pub(super) fn publish(member: &mut LibraryMember, method: &MethodSig, enclosing_prefix: usize) {
    let names = method
        .parameter_names
        .iter()
        .skip(enclosing_prefix)
        .cloned()
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default();
    member.call_sig.declared_parameter_names = if names.len() == member.params.len() {
        names
    } else {
        Vec::new()
    };
}

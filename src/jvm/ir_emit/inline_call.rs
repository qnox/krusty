//! Small boundary types and adapters shared by JVM inline-call emission paths.

use super::parse_physical_method_desc;
use crate::jvm::classfile::CodeBuilder;
use crate::types::Ty;

#[derive(Clone, Copy)]
pub(super) struct InlineStaticTarget<'a> {
    pub(super) owner: &'a str,
    pub(super) name: &'a str,
    pub(super) descriptor: &'a str,
    pub(super) splice_desc: &'a str,
    /// An `@InlineOnly` callee contributes no debug information to the caller.
    pub(super) inline_only: bool,
    /// Whether the fetched body may belong to another class than `owner` (a bridge to it).
    pub(super) allow_owner_bridge: bool,
}

pub(super) fn parse_descriptor_params(desc: &str) -> Option<Vec<Ty>> {
    parse_physical_method_desc(desc).map(|(params, _)| params)
}

/// Attach already-relocated inline exception-table entries to their caller labels.
pub(super) fn bind_inline_handlers(
    code: &mut CodeBuilder,
    handlers: &[(usize, usize, usize, u16)],
) {
    for &(start, end, handler, catch_type) in handlers {
        let (start_label, end_label, handler_label) =
            (code.new_label(), code.new_label(), code.new_label());
        code.bind_at(start_label, start);
        code.bind_at(end_label, end);
        code.bind_at(handler_label, handler);
        code.add_exception(start_label, end_label, handler_label, catch_type);
    }
}

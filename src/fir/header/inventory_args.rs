//! Argument groups shared by header stub and syntax extraction.

use super::{DeclarationIds, HeaderSyntaxArena, LookupNames, SourceFileId};

/// Source identity and the interners that build one file's declaration stubs.
pub(super) struct StubBuild<'a> {
    pub(super) source: SourceFileId,
    pub(super) ids: &'a mut DeclarationIds,
    pub(super) names: &'a mut LookupNames,
}

pub(super) fn stub_build<'a>(
    source: SourceFileId,
    ids: &'a mut DeclarationIds,
    names: &'a mut LookupNames,
) -> StubBuild<'a> {
    StubBuild { source, ids, names }
}

/// Whether a classifier stub is a companion and whether it is a classifier member.
pub(super) struct ClassStubKind {
    pub(super) companion: bool,
    pub(super) classifier_member: bool,
}

/// Mutable arenas that record one file's header syntax.
pub(super) struct HeaderArenas<'a> {
    pub(super) ids: &'a mut DeclarationIds,
    pub(super) names: &'a mut LookupNames,
    pub(super) headers: &'a mut HeaderSyntaxArena,
}

pub(super) fn header_arenas<'a>(
    ids: &'a mut DeclarationIds,
    names: &'a mut LookupNames,
    headers: &'a mut HeaderSyntaxArena,
) -> HeaderArenas<'a> {
    HeaderArenas {
        ids,
        names,
        headers,
    }
}

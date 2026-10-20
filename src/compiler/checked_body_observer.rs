//! A consumer of Pass 2 that sees each checked body before common lowering takes it, with the
//! module index and the declaration unit it was checked in. Lowering is unaffected: an observer
//! reads checked FIR, never changes it.

use crate::ast::File;
use crate::fir::{
    ActiveSourceDeclarations, BodyOwnerId, FirBody, ResolvedModuleIndex, SourceFileId, SourceMap,
};
use crate::resolve::PassTwoSymbols;

/// The module Pass 2 is about to check: its finalized index and its Kotlin sources, in input order.
pub(crate) struct ObservedModule<'a> {
    pub index: &'a ResolvedModuleIndex,
    pub source_map: &'a SourceMap,
    pub sources: &'a [ObservedSource<'a>],
}

pub(crate) struct ObservedSource<'a> {
    pub id: SourceFileId,
    pub text: &'a str,
}

/// One checked body, with what it was checked against.
pub(crate) struct ObservedBody<'a> {
    pub index: &'a ResolvedModuleIndex,
    pub symbols: &'a PassTwoSymbols,
    pub source_map: &'a SourceMap,
    pub file: &'a File,
    pub declarations: &'a ActiveSourceDeclarations,
    pub source: SourceFileId,
    pub owner: BodyOwnerId,
    pub body: &'a FirBody,
}

pub(crate) trait CheckedBodyObserver {
    fn begin_module(&mut self, module: ObservedModule<'_>);

    fn observe(&mut self, body: ObservedBody<'_>);
}

/// Observes nothing: an ordinary compilation.
pub(crate) struct NoObserver;

impl CheckedBodyObserver for NoObserver {
    fn begin_module(&mut self, _module: ObservedModule<'_>) {}

    fn observe(&mut self, _body: ObservedBody<'_>) {}
}

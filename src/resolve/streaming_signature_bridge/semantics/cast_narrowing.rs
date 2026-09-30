//! Successful-cast narrowing and least upper bound for signature evaluation.
//!
//! Both need the file's module view. A miss is `Err(())`; the production semantics maps that onto
//! its diagnostic id.

use crate::fir::ResolvedTy;
use crate::resolve::SymbolTable;

pub(super) fn least_upper_bound(
    table: &SymbolTable,
    scope: crate::fir::SignatureScope,
    origin: crate::fir::OriginId,
    operands: &[ResolvedTy],
) -> Result<ResolvedTy, ()> {
    let Some(first) = operands.first().copied() else {
        return Err(());
    };
    let _ = origin;
    // Least upper bound is type algebra; only its lookup context is caller-specific. Signature
    // evaluation supplies the module view of the file whose signature is being solved, so no
    // checker is constructed here. A branch whose type is still undetermined has no common
    // supertype with anything and declines rather than naming a placeholder.
    let module = crate::module_symbols::ModuleSymbols::for_file(table, scope.source.raw());
    let source = crate::symbol_source::CompositeSource::new(vec![
        &module as &dyn crate::symbol_source::SymbolSource,
        &*table.libraries as &dyn crate::symbol_source::SymbolSource,
    ]);
    let oracle = crate::symbol_resolver::SourceOracle(&source);
    let joined = operands
        .iter()
        .skip(1)
        .try_fold(first.get(), |left, right| {
            crate::resolve::semantic_common_supertype(&source, &oracle, left, right.get())
        })
        .ok_or(())?;
    if joined.mentions_error() || joined.mentions_pending() {
        return Err(());
    }
    ResolvedTy::new(joined).map_err(|_| ())
}

pub(super) fn narrow_successful_cast(
    table: &SymbolTable,
    scope: crate::fir::SignatureScope,
    original: ResolvedTy,
    target: ResolvedTy,
) -> Result<ResolvedTy, ()> {
    let module = crate::module_symbols::ModuleSymbols::for_file(table, scope.source.raw());
    let source = crate::symbol_source::CompositeSource::new(vec![
        &module as &dyn crate::symbol_source::SymbolSource,
        &*table.libraries as &dyn crate::symbol_source::SymbolSource,
    ]);
    let oracle = crate::symbol_resolver::SourceOracle(&source);
    let chosen = if crate::assignable::is_subtype(
        &crate::assignable::TyCtx::new(),
        &oracle,
        original.get(),
        target.get(),
    ) {
        original.get()
    } else {
        target.get()
    };
    ResolvedTy::new(chosen).map_err(|_| ())
}

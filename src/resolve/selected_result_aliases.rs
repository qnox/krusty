//! The typealias application each selected call or read's result is spelled through.
//!
//! A selection of a declaration whose result is written through a typealias (`fun names():
//! Names`, `val foo: Foo<T>?`) records that alias applied as the selection bound it. kotlinc's
//! result carries the abbreviation, so an inferred declaration result reached through it records
//! the alias.

use super::selected_result_publication::CompactSelectedResultPublication;
use super::SymbolTable;
use crate::fir::{OriginId, ResolvedTypeAbbreviation};
use crate::fx_hash::HashMap;
use crate::libraries::DeclaredResultAlias;
use crate::types::Ty;
use std::cell::RefCell;

/// What a selection applied its declaration to: the receiver it was selected on and its argument
/// types in source order.
#[derive(Clone, Copy, Default)]
pub(in crate::resolve) struct SelectionInputs<'a> {
    pub(in crate::resolve) receiver: Option<Ty>,
    pub(in crate::resolve) arguments: &'a [Ty],
}

impl<'a> SelectionInputs<'a> {
    pub(in crate::resolve) fn new(receiver: Option<Ty>, arguments: &'a [Ty]) -> Self {
        Self {
            receiver,
            arguments,
        }
    }
}

#[derive(Default)]
pub(in crate::resolve) struct SelectedResultAliases {
    aliases: RefCell<HashMap<OriginId, DeclaredResultAlias>>,
}

impl SelectedResultAliases {
    /// The selection at `origin` chose a declaration whose result is spelled through `alias`; a
    /// selection of a declaration without one forgets the former alias.
    pub(in crate::resolve) fn record(&self, origin: OriginId, alias: Option<DeclaredResultAlias>) {
        let mut aliases = self.aliases.borrow_mut();
        match alias {
            Some(alias) => aliases.insert(origin, alias),
            None => aliases.remove(&origin),
        };
    }

    /// Record the alias `publication` selected at `origin`, with its declaration's type
    /// parameters bound through the selection's `inputs`.
    pub(in crate::resolve) fn record_applied(
        &self,
        origin: OriginId,
        publication: &CompactSelectedResultPublication,
        inputs: SelectionInputs<'_>,
        table: &SymbolTable,
    ) {
        self.record(
            origin,
            publication.applied_result_alias(
                inputs.receiver,
                inputs.arguments,
                |owner| table.declaring_self(owner),
                |actual, owner| table.applied_supertype(actual, owner),
            ),
        );
    }

    pub(in crate::resolve) fn get(&self, origin: OriginId) -> Option<DeclaredResultAlias> {
        self.aliases.borrow().get(&origin).copied()
    }

    /// The abbreviation the result at `origin` carries: its recorded alias application.
    pub(in crate::resolve) fn abbreviation(
        &self,
        origin: OriginId,
    ) -> Option<ResolvedTypeAbbreviation> {
        let alias = self.get(origin)?;
        Some(ResolvedTypeAbbreviation::root(
            alias.alias(),
            alias.arguments(),
            alias.marked_nullable(),
        ))
    }
}

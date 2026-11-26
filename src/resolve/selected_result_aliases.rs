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

/// The alias shape a constructor call inferred its abbreviation arguments through: the alias
/// formals and its expansion. The published constructor type binds those formals again, so an
/// inferred argument carries the same flexibility in the abbreviation as in the expanded type.
#[derive(Clone)]
pub(in crate::resolve) struct InferredAbbreviation {
    pub(in crate::resolve) formals: Box<[String]>,
    pub(in crate::resolve) expansion: Ty,
}

/// The abbreviation of a call's published result `published`: arguments ordinary selection
/// inferred for the alias are those the published expansion binds, and a formal it leaves unbound
/// keeps the argument ordinary selection inferred for it.
pub(in crate::resolve) fn rebind_to_published(
    abbreviation: ResolvedTypeAbbreviation,
    inferred: Option<&InferredAbbreviation>,
    published: Ty,
) -> ResolvedTypeAbbreviation {
    let Some(inferred) = inferred else {
        return abbreviation;
    };
    let mut bindings = crate::symbol_resolver::GSigBinds::default();
    crate::symbol_resolver::unify_ty(inferred.expansion, published, &mut bindings);
    let arguments = inferred
        .formals
        .iter()
        .zip(abbreviation.arguments.iter())
        .map(|(formal, ordinary)| bindings.get(formal.as_str()).copied().unwrap_or(*ordinary))
        .collect();
    ResolvedTypeAbbreviation {
        arguments,
        ..abbreviation
    }
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

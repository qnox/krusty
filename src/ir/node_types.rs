//! The checked types every expression node carries, kotlinc's `IrExpression.type`.
//!
//! A type belongs to the node, not to a table a pass must remember: it lives in a column parallel
//! to [`IrFile::exprs`], so a copy of the node takes it with the node's other facts
//! ([`InheritedFacts`]).

use super::expression_copy::InheritedFacts;
use super::{ExprId, IrFile};
use crate::types::Ty;

/// The types of one expression node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IrNodeTypes {
    /// The expression's logical (source) type as the checker inferred it, recorded verbatim by
    /// lowering and never erased. The value-class pass reads it to recover the representation of a
    /// value whose node alone is ambiguous: a library call returns a physical `Object`, but its
    /// logical type may be a value class (`runCatching {…}: Result`). Target-neutral rewrites that
    /// keep a value in a new temporary read [`Self::physical`] first, when a backend has already
    /// changed the representation, and this type otherwise.
    pub logical: Option<Ty>,
    /// The physical type before a semantic read coercion.
    pub physical: Option<Ty>,
}

/// The types of every expression node, by [`ExprId`].
#[derive(Default)]
pub(crate) struct IrNodeTypeColumn {
    nodes: Vec<IrNodeTypes>,
}

impl IrNodeTypeColumn {
    fn get(&self, expression: ExprId) -> IrNodeTypes {
        self.nodes
            .get(expression as usize)
            .copied()
            .unwrap_or_default()
    }

    fn get_mut(&mut self, expression: ExprId) -> &mut IrNodeTypes {
        let index = expression as usize;
        if self.nodes.len() <= index {
            self.nodes.resize(index + 1, IrNodeTypes::default());
        }
        &mut self.nodes[index]
    }

    fn existing_mut(&mut self, expression: ExprId) -> Option<&mut IrNodeTypes> {
        self.nodes.get_mut(expression as usize)
    }

    fn all(&self) -> impl Iterator<Item = (ExprId, IrNodeTypes)> + '_ {
        self.nodes
            .iter()
            .enumerate()
            .map(|(expression, types)| (expression as ExprId, *types))
    }
}

impl IrNodeTypeColumn {
    /// The checked logical type of `expression`.
    pub(crate) fn logical(&self, expression: ExprId) -> Option<Ty> {
        self.get(expression).logical
    }

    /// The physical type `expression` has before a semantic read coercion.
    pub(crate) fn physical(&self, expression: ExprId) -> Option<Ty> {
        self.get(expression).physical
    }
}

impl InheritedFacts for IrNodeTypeColumn {
    fn inherit(&mut self, source: ExprId, target: ExprId) {
        let IrNodeTypes { logical, physical } = self.get(source);
        let copy = self.get_mut(target);
        if logical.is_some() {
            copy.logical = logical;
        }
        if physical.is_some() {
            copy.physical = physical;
        }
    }
}

impl IrFile {
    /// Give `target` the types `source` has.
    pub(crate) fn copy_types(&mut self, source: ExprId, target: ExprId) {
        self.types.inherit(source, target);
    }

    /// The node types, for a view that borrows them beside other parts of the file.
    pub(crate) fn node_types(&self) -> &IrNodeTypeColumn {
        &self.types
    }

    /// The checked logical type of `expression`.
    pub fn logical_type(&self, expression: ExprId) -> Option<Ty> {
        self.types.get(expression).logical
    }

    /// The physical type `expression` has before a semantic read coercion.
    pub fn physical_type(&self, expression: ExprId) -> Option<Ty> {
        self.types.get(expression).physical
    }

    pub fn set_logical_type(&mut self, expression: ExprId, ty: Ty) {
        self.types.get_mut(expression).logical = Some(ty);
    }

    pub fn set_physical_type(&mut self, expression: ExprId, ty: Ty) {
        self.types.get_mut(expression).physical = Some(ty);
    }

    /// Give `expression` the logical type `ty` unless it already has one.
    pub fn set_logical_type_if_absent(&mut self, expression: ExprId, ty: Ty) {
        self.types.get_mut(expression).logical.get_or_insert(ty);
    }

    /// Give `expression` the physical type `ty` unless it already has one.
    pub fn set_physical_type_if_absent(&mut self, expression: ExprId, ty: Ty) {
        self.types.get_mut(expression).physical.get_or_insert(ty);
    }

    /// Remove `expression`'s logical type, returning it.
    pub fn take_logical_type(&mut self, expression: ExprId) -> Option<Ty> {
        self.types.existing_mut(expression)?.logical.take()
    }

    /// Remove `expression`'s physical type, returning it.
    pub fn take_physical_type(&mut self, expression: ExprId) -> Option<Ty> {
        self.types.existing_mut(expression)?.physical.take()
    }

    /// `expression`'s logical type, to rewrite in place.
    pub(crate) fn logical_type_mut(&mut self, expression: ExprId) -> Option<&mut Ty> {
        self.types.existing_mut(expression)?.logical.as_mut()
    }

    /// `expression`'s physical type, to rewrite in place.
    pub(crate) fn physical_type_mut(&mut self, expression: ExprId) -> Option<&mut Ty> {
        self.types.existing_mut(expression)?.physical.as_mut()
    }

    /// Every expression with a logical type, in arena order.
    pub(crate) fn logical_types(&self) -> impl Iterator<Item = (ExprId, Ty)> + '_ {
        self.types
            .all()
            .filter_map(|(expression, types)| Some((expression, types.logical?)))
    }

    /// Every expression with a physical type, in arena order.
    pub(crate) fn physical_types(&self) -> impl Iterator<Item = (ExprId, Ty)> + '_ {
        self.types
            .all()
            .filter_map(|(expression, types)| Some((expression, types.physical?)))
    }

    /// Rewrite every recorded logical and physical type in place.
    pub(crate) fn rewrite_types(&mut self, mut rewrite: impl FnMut(Ty) -> Ty) {
        for types in &mut self.types.nodes {
            for ty in [&mut types.logical, &mut types.physical]
                .into_iter()
                .flatten()
            {
                *ty = rewrite(*ty);
            }
        }
    }

    /// Forget the types of every expression from `first` on, as when the arena is truncated there.
    pub(crate) fn truncate_types(&mut self, first: ExprId) {
        self.types.nodes.truncate(first as usize);
    }
}

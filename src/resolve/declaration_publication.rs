//! Checked type-publication facts for declarations whose type is inferred from a body value.
//!
//! The checker owns the value's selected Java enhancement and the provider-normalized upper face
//! of every flexible type occurrence. It joins those facts to a stable declaration identity here.
//! The compiler metadata handoff consumes that explicit contract; it never reopens an AST body or
//! asks a backend to reconstruct provider semantics.

use super::{ty_mentions_param, Checker, Signature, TypeInfo};
use crate::ast::ExprId;
use crate::fir::DeclarationId;
use crate::fx_hash::HashMap;
use crate::spelling::Spelled;
use crate::types::Ty;

/// A literal `null` is an expression-only sentinel. Once inference publishes it as a declaration
/// type, Kotlin's semantic type is the nullable bottom type `Nothing?`.
pub(super) fn inferred_declaration_ty(ty: Ty) -> Ty {
    if ty == Ty::Null {
        Ty::nullable(Ty::Nothing)
    } else {
        ty
    }
}

impl Signature {
    /// Publish one body-inferred result without replacing a declaration-backed symbolic result by
    /// the erased bound a later body pass may observe.
    pub(super) fn set_inferred_return(&mut self, ret: Ty) -> bool {
        let ret = inferred_declaration_ty(ret);
        let inferred_generic_ret = self.generic_sig.as_ref().map(|generic| {
            let declaration_bindings = generic
                .formals
                .iter()
                .enumerate()
                .map(|(index, formal)| {
                    (
                        crate::types::type_parameter_source_name(formal).to_string(),
                        Ty::ty_param(
                            formal,
                            generic
                                .formal_bounds
                                .get(index)
                                .and_then(|bounds| bounds.first())
                                .copied()
                                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
                        ),
                    )
                })
                .collect::<HashMap<_, _>>();
            let ret = crate::symbol_resolver::ty_subst_keep_unbound(ret, &declaration_bindings);
            if ty_mentions_param(generic.ret, &generic.formals)
                && !ty_mentions_param(ret, &generic.formals)
            {
                generic.ret
            } else {
                ret
            }
        });
        let changed = self.ret != ret
            || self
                .generic_sig
                .as_ref()
                .zip(inferred_generic_ret)
                .is_some_and(|(generic, inferred)| generic.ret != inferred);
        self.ret = ret;
        if let Some((generic, inferred)) = self.generic_sig.as_mut().zip(inferred_generic_ret) {
            generic.ret = inferred;
        }
        changed
    }
}

/// Metadata-only result-type facts keyed by the declaration that owns them.
#[derive(Default)]
pub(super) struct InferredDeclarationPublications {
    results: HashMap<DeclarationId, Spelled>,
}

impl Checker<'_> {
    /// Freeze the checked value facts of one inferred declaration before its active source unit is
    /// discarded. `ty` is the already-finalized semantic declaration type.
    pub(super) fn record_inferred_declaration_publication(
        &mut self,
        declaration: DeclarationId,
        value: ExprId,
        ty: Ty,
    ) {
        if ty == Ty::Error || ty.mentions_pending() {
            return;
        }
        let mut result = Spelled::default();
        result.record_flexible_upper_bounds(ty, &|lower| {
            crate::assignable::platform_shape_upper_bound(
                crate::assignable::TypeOracle::platform_flexible_upper_bound(self, lower),
            )
        });
        result.mark_enhanced(&self.value_enhancement(value).declared());
        if result.is_none() {
            return;
        }
        self.inferred_declaration_publications
            .results
            .insert(declaration, result);
    }

    /// A property inferred from an enhanced Java value also commits that unchecked value to its
    /// published declaration type, so it needs the ordinary declaration-boundary guard.
    pub(super) fn record_inferred_property_publication(
        &mut self,
        declaration: DeclarationId,
        initializer: ExprId,
        ty: Ty,
    ) {
        self.record_inferred_declaration_publication(declaration, initializer, ty);
        self.narrow_enhanced_value(ty, initializer);
    }
}

impl TypeInfo {
    /// Metadata facts for the inferred result owned by `declaration`, if that result needs any.
    pub(crate) fn inferred_declaration_publication(
        &self,
        declaration: DeclarationId,
    ) -> Option<&Spelled> {
        self.inferred_declaration_publications
            .results
            .get(&declaration)
    }
}

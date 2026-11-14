//! JVM-only realization facts for checked Kotlin property operations.
//!
//! Common IR retains a semantic property operation and its stable operation identity.  The JVM
//! preparation passes attach either the exact active-source declaration selected by FIR or a
//! physical access supplied by module/classpath metadata.  Keeping both in one backend-owned table
//! prevents emission from recovering a declaration by `(owner, name)` and prevents JVM fields or
//! accessor descriptors from leaking into FIR/common IR.

use krusty::fx_hash::HashMap;

use krusty::fir::PropertyId;
use krusty::ir::ExprId;
use krusty::jvm::inline::PropertyAccess;
use krusty::types::{TypeName, Visibility};

/// Whether a checked Kotlin property declaration admits the JVM's `@JvmField` realization.
///
/// Declaration storage and cross-file access share this predicate so both sides consume exactly
/// the same semantic restrictions. The caller still chooses the physical owner/staticness; this
/// operation contains no class-file identity or source-spelling logic.
pub(crate) fn jvm_field_eligible(
    annotations: &[TypeName],
    visibility: Visibility,
    flags: krusty::fir::DeclarationFlags,
    has_extension_receiver: bool,
    has_context_parameters: bool,
) -> bool {
    annotations
        .iter()
        .any(|annotation| annotation.matches("kotlin/jvm/JvmField"))
        && matches!(
            visibility,
            Visibility::Public | Visibility::Internal | Visibility::Protected
        )
        && !flags.has(krusty::fir::DeclarationFlags::CUSTOM_GETTER)
        && !flags.has(krusty::fir::DeclarationFlags::CUSTOM_SETTER)
        && !flags.has(krusty::fir::DeclarationFlags::DELEGATED)
        && !flags.has(krusty::fir::DeclarationFlags::LATEINIT)
        && !flags.has(krusty::fir::DeclarationFlags::CONST)
        && !flags.has(krusty::fir::DeclarationFlags::OPEN)
        && !flags.has(krusty::fir::DeclarationFlags::ABSTRACT)
        && !has_extension_receiver
        && !has_context_parameters
}

#[derive(Clone, Debug)]
pub(crate) enum PropertyRealization {
    Local(PropertyId),
    Physical(PropertyAccess),
}

#[derive(Default)]
pub(crate) struct PropertyRealizations {
    entries: HashMap<ExprId, PropertyRealization>,
    /// The current-module property whose getter a realized `super` call invokes. The realized
    /// call keeps only its physical spelling, so its property identity is carried here.
    super_getters: HashMap<ExprId, PropertyId>,
}

impl PropertyRealizations {
    pub(crate) fn record_local(&mut self, expression: ExprId, target: PropertyId) {
        self.entries
            .insert(expression, PropertyRealization::Local(target));
    }

    pub(crate) fn record_physical(&mut self, expression: ExprId, access: PropertyAccess) {
        self.entries
            .insert(expression, PropertyRealization::Physical(access));
    }

    pub(crate) fn get(&self, expression: ExprId) -> Option<&PropertyRealization> {
        self.entries.get(&expression)
    }

    pub(crate) fn record_super_getter(&mut self, call: ExprId, property: PropertyId) {
        self.super_getters.insert(call, property);
    }

    /// The realized `super` getter calls, each with the property whose getter it invokes.
    pub(crate) fn super_getters(&self) -> impl Iterator<Item = (ExprId, PropertyId)> + '_ {
        self.super_getters
            .iter()
            .map(|(&call, &property)| (call, property))
    }
}

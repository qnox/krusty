//! Stable override decisions published while the declaration providers are live.
//!
//! An override edge is a Kotlin semantic fact. It records the exact declarations and the raw and
//! applied language types; it does not record a JVM descriptor, erased storage type, accessor
//! spelling, or bridge method. A backend may use the edge to decide whether its representation
//! requires a bridge, but it must not repeat property lookup or infer an override from a name.

use super::{CallableId, ExternalCallableId, PropertyId, ResolvedTy};
use crate::types::TypeName;

/// Source identity or semantic compiler-generated role of a callable parameter. Target backends
/// format generated roles for their ABI; common phases never invent a platform spelling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedParameterIdentity {
    Source(Box<str>),
    /// The declaration/provider publishes no source name for this parameter. The ordinal is stable
    /// semantic position only; targets must not turn it into a fabricated `pN` spelling.
    Unnamed {
        ordinal: u32,
    },
    ContextValue {
        ordinal: u32,
        source_name: Box<str>,
    },
    AnonymousContextParameter {
        ordinal: u32,
    },
    LegacyContextReceiver {
        ordinal: u32,
    },
    ExtensionReceiver,
    PropertySetterValue,
    SuspendCompletion,
}

/// Build the complete semantic identity list for a declaration parameter layout.
///
/// `names` and the context counts describe the logical Kotlin parameters; `parameter_count`
/// describes the physical list consumed by the target-facing surface and may additionally contain
/// the extension receiver. Missing provider names stay missing. In particular, this never turns a
/// stable ordinal into a fabricated `pN` spelling.
pub(crate) fn declaration_parameter_identities(
    names: &[String],
    parameter_count: usize,
    context_count: usize,
    extension_receiver: bool,
) -> Box<[ResolvedParameterIdentity]> {
    let receiver = extension_receiver.then(|| {
        assert!(
            context_count < parameter_count,
            "an extension declaration must retain its physical receiver parameter"
        );
        context_count
    });
    (0..parameter_count)
        .map(|physical| {
            if receiver == Some(physical) {
                return ResolvedParameterIdentity::ExtensionReceiver;
            }
            let logical = physical
                .checked_sub(usize::from(
                    receiver.is_some_and(|receiver| physical > receiver),
                ))
                .expect("an extension receiver has one physical slot");
            let ordinal = u32::try_from(logical).expect("parameter ordinal fits u32");
            if logical < context_count {
                match names.get(logical).map(String::as_str).unwrap_or_default() {
                    "" => ResolvedParameterIdentity::LegacyContextReceiver { ordinal },
                    "_" | "<unused var>" => {
                        ResolvedParameterIdentity::AnonymousContextParameter { ordinal }
                    }
                    name => ResolvedParameterIdentity::ContextValue {
                        ordinal,
                        source_name: name.into(),
                    },
                }
            } else if let Some(name) = names
                .get(logical)
                .filter(|name| !name.is_empty() && name.as_str() != "_")
            {
                ResolvedParameterIdentity::Source(name.as_str().into())
            } else {
                ResolvedParameterIdentity::Unnamed { ordinal }
            }
        })
        .collect()
}

/// Stable identity of the overridden property declaration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ResolvedPropertyOverrideTarget {
    Module(PropertyId),
    External(ExternalCallableId),
}

/// One exact property override selected in Pass 1.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPropertyOverride {
    /// The source property that implements this edge.
    pub implementation: ResolvedPropertyOverrideTarget,
    pub implementation_owner: TypeName,
    /// Exact declaration being overridden, independent of its source spelling.
    pub overridden: ResolvedPropertyOverrideTarget,
    pub overridden_owner: TypeName,
    pub overridden_is_interface: bool,
    pub name: Box<str>,
    /// Declaration-side type before applying the implementing class's supertype arguments. A target
    /// backend erases this type according to its own representation rules.
    pub declared_type: ResolvedTy,
    /// The same property as viewed through the implementing class's applied supertype.
    pub applied_type: ResolvedTy,
    /// Declaration-side type of the implementation before applying the inheriting class's type
    /// arguments. This is the type a representation backend erases for the target accessor.
    pub implementation_type: ResolvedTy,
    pub overridden_mutable: bool,
    pub implementation_mutable: bool,
    /// Whether a Kotlin superclass declaration among the implementation's other overridden
    /// properties itself overrides `overridden`. A target realization of `overridden` (such as a
    /// JVM renamed-builtin bridge) may therefore already be owned by that superclass.
    pub has_kotlin_superclass_override: bool,
    pub depth: u32,
}

/// Stable identity of the overridden function declaration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ResolvedFunctionOverrideTarget {
    Module(CallableId),
    External(ExternalCallableId),
}

/// One exact function override selected in Pass 1. All types are Kotlin semantic types; the
/// declaration-side shape remains unapplied so each backend can perform its own erasure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedFunctionOverride {
    pub implementation: ResolvedFunctionOverrideTarget,
    pub implementation_owner: TypeName,
    pub overridden: ResolvedFunctionOverrideTarget,
    pub overridden_owner: TypeName,
    /// The overridden owner is an interface. This is a semantic classifier fact, retained so a
    /// representation backend does not re-query the declaration provider merely to distinguish
    /// superclass and interface dispatch obligations.
    pub overridden_is_interface: bool,
    /// Semantic declaration name. Physical spellings remain behind external callable identities and
    /// are selected only by a target backend.
    pub name: Box<str>,
    pub declared_parameters: Box<[ResolvedTy]>,
    pub declared_result: ResolvedTy,
    pub applied_parameters: Box<[ResolvedTy]>,
    pub applied_result: ResolvedTy,
    /// Declaration-side implementation shape before applying the inheriting class's type arguments.
    /// The frontend has already selected the declaration; a backend only erases this shape.
    pub implementation_parameters: Box<[ResolvedTy]>,
    pub implementation_parameter_identities: Box<[ResolvedParameterIdentity]>,
    pub implementation_result: ResolvedTy,
    pub suspend: bool,
    /// Whether a Kotlin superclass declaration among the implementation's other overridden
    /// functions itself overrides `overridden`; see
    /// [`ResolvedPropertyOverride::has_kotlin_superclass_override`].
    pub has_kotlin_superclass_override: bool,
    pub depth: u32,
}

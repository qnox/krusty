//! Argument groups for compact signature-bridge calls.
//!
//! The bridge's line budget only shrinks, so the groups live here. Tuple order is the order
//! named on each type.

use crate::types::Ty;

/// `receiver`, `arguments`, `argument_kinds`, `explicit_type_arguments`, `expected`.
pub(super) struct DemandedSourceInputs<'a>(
    pub(super) Option<Ty>,
    pub(super) &'a [Ty],
    pub(super) Option<&'a [crate::symbol_resolver::CallArgKind]>,
    pub(super) &'a [Ty],
    pub(super) Option<Ty>,
);

/// `root`, `bodies`.
pub(crate) struct CheckedLocalBodies<'a>(
    pub(crate) crate::fir::DeclarationId,
    pub(crate) &'a crate::fx_hash::HashSet<crate::fir::DeclarationId>,
);

/// `inline_owners`, `active_source`, `active_root`, `exact_declarations`.
pub(super) struct LocalSignatureSelection<'a>(
    pub(super) Option<&'a [crate::fir::DeclarationId]>,
    pub(super) Option<&'a crate::fir::ActiveSourceDeclarations>,
    pub(super) Option<ActiveLocalRoot<'a>>,
    pub(super) Option<&'a [crate::fir::DeclarationId]>,
);

pub(super) type ActiveLocalRoot<'a> = (
    &'a crate::fir::ActiveSourceDeclarations,
    crate::fir::DeclarationId,
    &'a crate::fx_hash::HashSet<crate::fir::DeclarationId>,
);

/// `extension`, `dispatch`.
#[derive(Clone, Copy)]
pub(super) struct MemberExtensionReceivers(pub(super) Ty, pub(super) Ty);

/// `arguments`, `type_arguments`, `trailing_lambda`.
pub(super) struct SuperMemberCall<'a>(
    pub(super) &'a [crate::fir::ResolvedSigCallArgument<'a>],
    pub(super) &'a [Ty],
    pub(super) bool,
);

/// `scope`, `site`, `delegate`, `name`, `this_ref`.
pub(super) struct DelegateConventionAmbiguity<'a>(
    pub(super) crate::fir::SignatureScope,
    pub(super) crate::fir::ResolvedSignatureDelegateSite,
    pub(super) Ty,
    pub(super) &'a str,
    pub(super) Ty,
);

/// `receiver`, `spelling`, `arguments`, `local_operator`.
pub(super) struct OperatorConventionCall<'a>(
    pub(super) crate::fir::ResolvedTy,
    pub(super) &'a str,
    pub(super) &'a [crate::fir::ResolvedSigCallArgument<'a>],
    pub(super) Option<crate::fir::ResolvedTy>,
);

/// `operator`, `receiver`, `argument`, `local_operator`.
pub(super) struct InPlaceAssignment(
    pub(super) crate::fir::SigBinaryOperator,
    pub(super) crate::fir::ResolvedTy,
    pub(super) crate::fir::ResolvedTy,
    pub(super) Option<crate::fir::ResolvedTy>,
);

/// `arguments`, `type_arguments`, `trailing_lambda`, `expected`.
pub(super) struct RejectedMemberCall<'a>(
    pub(super) &'a [crate::fir::ResolvedSigCallArgument<'a>],
    pub(super) &'a [Ty],
    pub(super) bool,
    pub(super) Option<Ty>,
);

/// `arguments`, `names`, `type_argument_count`, `trailing_lambda`.
pub(super) struct SoleMappedCandidateQuery<'a>(
    pub(super) &'a [crate::fir::ResolvedSigCallArgument<'a>],
    pub(super) Option<&'a [Option<String>]>,
    pub(super) usize,
    pub(super) bool,
);

/// `arguments`, `parameter_by_argument`.
pub(super) struct RejectedArgumentMapping<'a>(
    pub(super) &'a [crate::fir::ResolvedSigCallArgument<'a>],
    pub(super) &'a [usize],
);

/// `receiver`, `type_arguments`, `expected`, `result`, `member`, `source`,
/// `source_declaration`, `argument_types`, `publication`.
pub(super) struct SelectedMemberPublication<'a>(
    pub(super) crate::fir::ResolvedTy,
    pub(super) &'a [Ty],
    pub(super) Option<crate::fir::ResolvedTy>,
    pub(super) Ty,
    pub(super) Option<crate::libraries::LibraryMember>,
    pub(super) Option<(u32, u32)>,
    pub(super) Option<crate::fir::DeclarationId>,
    pub(super) Vec<Ty>,
    pub(super) crate::resolve::selected_result_publication::CompactSelectedResultPublication,
);

/// `spelling`, `arguments`, `type_arguments`, `trailing_lambda`.
pub(super) struct ClassifierCall<'a, A, T>(
    pub(super) &'a str,
    pub(super) &'a [A],
    pub(super) &'a [T],
    pub(super) bool,
);

/// `names`, `bounds`.
pub(super) struct DeclaredTypeParameters<'a>(
    pub(super) &'a [String],
    pub(super) &'a [(String, crate::fir::HeaderTypeId, crate::ast::TypeRef)],
);

/// `deferred_candidates`, `deferred_headers`, `failed`, `diagnostics`.
pub(super) struct TypeParameterPublication<'a>(
    pub(super) &'a crate::fx_hash::HashSet<crate::fir::DeclarationId>,
    pub(super) &'a mut crate::fx_hash::HashSet<crate::fir::DeclarationId>,
    pub(super) &'a mut Vec<crate::fir::DeclarationId>,
    pub(super) &'a mut crate::diag::DiagSink,
);

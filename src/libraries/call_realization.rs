//! Provider-normalized physical call targets carried with a selected semantic declaration.

use super::{
    physical_parameter_plan, CompilerIntrinsic, InlineKind, LibraryCallable, LibraryConst,
    LibraryMember, MemberRealization, Origin, SingletonDispatch,
};
use crate::types::{SemanticCallableOwner, Ty, TypeName};

/// The serialized-IR identity a KLIB provider publishes with a realization. It is part of this
/// provider-to-backend contract, so backends name it here rather than reaching into metadata.
pub use crate::metadata::id_signature::KlibDeclarationSignature;

/// One callable exported by one exact KLIB module.
///
/// A public IdSignature is unique inside a KLIB, not across an arbitrary dependency graph:
/// unrelated modules may legally declare identical internal signatures. Target linkage therefore
/// joins both the provider-owned module identity and the serialized declaration identity.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct KlibDependencyCallable {
    pub module: String,
    pub declaration: KlibDeclarationSignature,
}

/// A C ABI entry published by an exact Native interop declaration.
///
/// The symbol is physical provider data. It is never lookup input: source resolution selects the
/// declaration's opaque identity first, and a Native backend consumes this target only afterward.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectCFunction {
    symbol: Box<str>,
}

impl DirectCFunction {
    pub(crate) fn new(symbol: &str) -> Self {
        Self {
            symbol: symbol.into(),
        }
    }

    pub fn symbol(&self) -> &str {
        &self.symbol
    }
}

/// A receiver slot retained only by a classifier-associated companion extension's serialized
/// KLIB body.
///
/// Source lookup exposes such a declaration as an associated function with no value receiver, so
/// this is deliberately separate from [`LibraryCallable::params`]. The serialized IR remains
/// authoritative for the slot's physical type; this contract authenticates only its classifier
/// marker and stable parameter identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KlibBodyCompanionReceiver {
    classifier: TypeName,
    identity: crate::fir::ResolvedParameterIdentity,
}

impl KlibBodyCompanionReceiver {
    pub(crate) fn new(classifier: TypeName) -> Self {
        Self {
            classifier,
            identity: crate::fir::ResolvedParameterIdentity::ExtensionReceiver,
        }
    }

    pub(crate) fn classifier(&self) -> TypeName {
        self.classifier
    }

    pub(crate) fn identity(&self) -> &crate::fir::ResolvedParameterIdentity {
        &self.identity
    }
}

/// How a provider realizes one already-selected dependency callable.
///
/// The identity a consumer holds is opaque and provider-assigned; this is what the provider hands
/// back for it. Neither half names a target: [`LibraryCallable`] is the semantic declaration, and
/// the kind says which shape the declaration takes — a top-level function, a member, or a property
/// realized over storage rather than an accessor. A backend reads it; it does not reconstruct it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExternalCallableKind {
    TopLevel,
    Extension,
    Member,
    Constructor,
    /// A provider-normalized Kotlin property getter realized as a read of instance storage.
    InstanceFieldRead,
    /// A provider-normalized Kotlin property setter realized as a write of instance storage.
    InstanceFieldWrite,
    /// A selected dependency property whose realization is a static field read.
    StaticFieldRead,
    /// A selected dependency property whose realization is a static field write.
    StaticFieldWrite,
}

/// The semantic declaration facts needed to lower one selected KLIB body.
///
/// This deliberately excludes every target-realization field carried by
/// [`ExternalCallableRealization`] and the backend handoff: physical owners, descriptors, ABI
/// parameter types and physical names do not participate in common-IR lowering. The exact KLIB
/// module-qualified signature remains the only join key.
#[derive(Clone, Copy)]
pub struct KlibBodyCallable<'a> {
    name: &'a str,
    kind: ExternalCallableKind,
    visibility: crate::types::Visibility,
    semantic_role: Option<crate::types::SemanticCallRole>,
    params: &'a [Ty],
    ret: Ty,
    parameter_identities: &'a [crate::fir::ResolvedParameterIdentity],
    context_count: usize,
    /// Declaration-owned packed parameter position, including contexts and extension receiver.
    vararg_index: Option<usize>,
    parameter_defaults: Option<super::ParamFlags>,
    generic_signature: Option<&'a super::GenericSig>,
    type_parameters: &'a [Ty],
    reified_type_parameter_ordinals: &'a [u32],
    compiler_intrinsic: Option<CompilerIntrinsic>,
    /// The range operator an exact builtin `rangeTo`/`rangeUntil` member constructs.
    range_construction: Option<crate::fir::FirRangeOperation>,
    declaration_modifiers: Option<super::KlibBodyDeclarationModifiers>,
    class_contract: Option<&'a super::KlibBodyClassContract>,
    accessor: Option<&'a super::KlibBodyAccessor>,
    /// Exact singleton dispatch receiver selected for this call edge. Common KLIB body lowering
    /// uses its semantic type to validate and consume the serialized receiver; target storage
    /// remains outside this target-free contract.
    singleton_dispatch: Option<Ty>,
    /// Exact singleton classifier selected for this call edge. Its semantic type can be a mapped
    /// primitive, so later phases cannot reconstruct the classifier from the type.
    singleton_classifier: Option<TypeName>,
    companion_receiver: Option<&'a KlibBodyCompanionReceiver>,
    /// The provider-normalized namespace that declares the callable. Metadata is authoritative
    /// for it: a serialized body must name the same owner.
    declaration_owner: Option<crate::types::SemanticCallableOwner>,
    identity: &'a KlibDependencyCallable,
}

/// The declared shape of a selected callable that its KLIB body is lowered against.
pub(crate) struct KlibBodyCallableShape<'a> {
    pub(crate) name: &'a str,
    pub(crate) kind: ExternalCallableKind,
    pub(crate) visibility: crate::types::Visibility,
    pub(crate) semantic_role: Option<crate::types::SemanticCallRole>,
    pub(crate) params: &'a [Ty],
    pub(crate) ret: Ty,
    pub(crate) parameter_identities: &'a [crate::fir::ResolvedParameterIdentity],
    pub(crate) context_count: usize,
    pub(crate) vararg_index: Option<usize>,
    pub(crate) parameter_defaults: Option<super::ParamFlags>,
    pub(crate) generic_signature: Option<&'a super::GenericSig>,
    pub(crate) type_parameters: &'a [Ty],
    pub(crate) reified_type_parameter_ordinals: &'a [u32],
    pub(crate) compiler_intrinsic: Option<CompilerIntrinsic>,
    pub(crate) range_construction: Option<crate::fir::FirRangeOperation>,
    pub(crate) declaration_modifiers: Option<super::KlibBodyDeclarationModifiers>,
    pub(crate) class_contract: Option<&'a super::KlibBodyClassContract>,
    pub(crate) accessor: Option<&'a super::KlibBodyAccessor>,
}

impl<'a> KlibBodyCallable<'a> {
    pub(crate) fn new(
        shape: KlibBodyCallableShape<'a>,
        singleton: Option<SingletonDispatch>,
        companion_receiver: Option<&'a KlibBodyCompanionReceiver>,
        declaration_owner: Option<SemanticCallableOwner>,
        identity: Option<&'a KlibDependencyCallable>,
    ) -> Option<Self> {
        let KlibBodyCallableShape {
            name,
            kind,
            visibility,
            semantic_role,
            params,
            ret,
            parameter_identities,
            context_count,
            vararg_index,
            parameter_defaults,
            generic_signature,
            type_parameters,
            reified_type_parameter_ordinals,
            compiler_intrinsic,
            range_construction,
            declaration_modifiers,
            class_contract,
            accessor,
        } = shape;
        Some(Self {
            name,
            kind,
            visibility,
            semantic_role,
            params,
            ret,
            parameter_identities,
            context_count,
            vararg_index,
            parameter_defaults,
            generic_signature,
            type_parameters,
            reified_type_parameter_ordinals,
            compiler_intrinsic,
            range_construction,
            declaration_modifiers,
            class_contract,
            accessor,
            singleton_dispatch: singleton.map(SingletonDispatch::ty),
            singleton_classifier: singleton.map(|dispatch| dispatch.classifier),
            companion_receiver,
            declaration_owner,
            identity: identity?,
        })
    }

    pub(crate) fn name(self) -> &'a str {
        self.name
    }

    pub(crate) fn is_top_level(self) -> bool {
        matches!(
            self.kind,
            ExternalCallableKind::TopLevel | ExternalCallableKind::Extension
        )
    }

    pub(crate) fn is_member(self) -> bool {
        self.kind == ExternalCallableKind::Member
    }

    pub(crate) fn kind(self) -> ExternalCallableKind {
        self.kind
    }

    pub(crate) fn visibility(self) -> crate::types::Visibility {
        self.visibility
    }

    pub(crate) fn semantic_role(self) -> Option<crate::types::SemanticCallRole> {
        self.semantic_role
    }

    pub(crate) fn params(self) -> &'a [Ty] {
        self.params
    }

    pub(crate) fn ret(self) -> Ty {
        self.ret
    }

    pub(crate) fn generic_signature(self) -> Option<&'a super::GenericSig> {
        self.generic_signature
    }

    pub(crate) fn type_parameters(self) -> &'a [Ty] {
        self.type_parameters
    }

    pub(crate) fn reified_type_parameter_ordinals(self) -> &'a [u32] {
        self.reified_type_parameter_ordinals
    }

    pub(crate) fn parameter_identities(self) -> &'a [crate::fir::ResolvedParameterIdentity] {
        self.parameter_identities
    }

    pub(crate) fn context_count(self) -> usize {
        self.context_count
    }

    pub(crate) fn vararg_index(self) -> Option<usize> {
        self.vararg_index
    }

    pub(crate) fn parameter_defaults(self) -> Option<super::ParamFlags> {
        self.parameter_defaults
    }

    pub(crate) fn compiler_intrinsic(self) -> Option<CompilerIntrinsic> {
        self.compiler_intrinsic
    }

    pub(crate) fn range_construction(self) -> Option<crate::fir::FirRangeOperation> {
        self.range_construction
    }

    pub(crate) fn singleton_dispatch(self) -> Option<Ty> {
        self.singleton_dispatch
    }

    pub(crate) fn declaration_modifiers(self) -> Option<super::KlibBodyDeclarationModifiers> {
        self.declaration_modifiers
    }

    pub(crate) fn class_contract(self) -> Option<&'a super::KlibBodyClassContract> {
        self.class_contract
    }

    pub(crate) fn accessor(self) -> Option<&'a super::KlibBodyAccessor> {
        self.accessor
    }

    pub(crate) fn singleton_classifier(self) -> Option<TypeName> {
        self.singleton_classifier
    }

    /// Whether two selected edges describe the same dependency declaration before their
    /// call-site singleton dispatch is applied. A KLIB identity names one declaration, while
    /// inherited object selections may legitimately contribute several receiver variants.
    pub(crate) fn same_declaration_contract(self, other: Self) -> bool {
        self.accessor == other.accessor
            && self.name == other.name
            && self.kind == other.kind
            && self.visibility == other.visibility
            && self.semantic_role == other.semantic_role
            && self.params == other.params
            && self.ret == other.ret
            && self.parameter_identities == other.parameter_identities
            && self.context_count == other.context_count
            && self.vararg_index == other.vararg_index
            && self.parameter_defaults == other.parameter_defaults
            && self.generic_signature == other.generic_signature
            && self.type_parameters == other.type_parameters
            && self.reified_type_parameter_ordinals == other.reified_type_parameter_ordinals
            && self.compiler_intrinsic == other.compiler_intrinsic
            && self.range_construction == other.range_construction
            && self.declaration_modifiers == other.declaration_modifiers
            && self.class_contract == other.class_contract
            && self.companion_receiver == other.companion_receiver
            && self.declaration_owner == other.declaration_owner
            && self.identity == other.identity
    }

    pub(crate) fn companion_receiver(self) -> Option<&'a KlibBodyCompanionReceiver> {
        self.companion_receiver
    }

    pub(crate) fn declaration_owner(self) -> Option<crate::types::SemanticCallableOwner> {
        self.declaration_owner
    }

    pub(crate) fn signature(self) -> &'a KlibDeclarationSignature {
        &self.identity.declaration
    }

    pub(crate) fn identity(self) -> &'a KlibDependencyCallable {
        self.identity
    }
}

#[derive(Clone, Debug)]
pub struct ExternalCallableRealization {
    pub callable: LibraryCallable,
    pub kind: ExternalCallableKind,
    /// Exact semantic namespace that declares this callable. Providers publish it independently
    /// of the physical realization kind and owner: a JVM facade, mapped member, storage field, and
    /// KLIB declaration can expose the same source declaration without sharing a physical shape.
    pub declaration_owner: Option<SemanticCallableOwner>,
    /// Provider-published identities parallel to the callable's semantic source parameters. The
    /// physical parameter plan joins them to ABI-only slots such as a suspend continuation.
    /// A backend formats these for debug/metadata surfaces; it never reconstructs them from arity.
    pub parameter_identities: Box<[crate::fir::ResolvedParameterIdentity]>,
    /// Exact serialized-IR identity of the declaration, published by a KLIB provider so a backend
    /// can join this selection to its decoded IR body. Providers without serialized IR publish
    /// `None`; a consumer never reconstructs it from the callable's spelling or shape.
    pub declaration_signature: Option<KlibDeclarationSignature>,
    /// Exact KLIB module plus declaration identity used to join a selected dependency call to
    /// target code emitted earlier in the same program. This is absent for non-KLIB providers.
    pub dependency_callable: Option<KlibDependencyCallable>,
    /// Exact Native C entry authenticated while the provider still owned the interop archive,
    /// qualified annotation application, and declaration shape.
    pub direct_c_function: Option<DirectCFunction>,
    /// KLIB-only serialized receiver contract for a classifier-associated companion extension.
    /// It is separate from source parameters because source lookup names the declaration through
    /// the classifier without a value receiver.
    pub(crate) klib_body_companion_receiver: Option<KlibBodyCompanionReceiver>,
    /// Declaration-owned formal types, frozen before normalized full bounds can deepen their
    /// finite self/forward references. Empty for declarations without KLIB generic body facts.
    pub(crate) klib_body_type_parameters: Box<[Ty]>,
    /// Known declaration implementation modifiers; absent for providers without KLIB body facts.
    pub(crate) klib_body_declaration_modifiers: Option<super::KlibBodyDeclarationModifiers>,
    pub(crate) klib_body_class_contract: Option<super::KlibBodyClassContract>,
    pub(crate) klib_body_accessor: Option<super::KlibBodyAccessor>,
    pub(crate) klib_body_vararg_index: Option<usize>,
    pub(crate) klib_body_parameter_defaults: Option<crate::libraries::ParamFlags>,
    /// Whether a source parameter declares a default value, as the provider's declaration says.
    pub declares_parameter_defaults: bool,
}

impl ExternalCallableRealization {
    /// The target-free view of this selected declaration when it carries a serialized KLIB
    /// identity. Consumers lower through this view instead of importing physical realization
    /// facts into common lowering.
    pub fn klib_body_callable(&self) -> Option<KlibBodyCallable<'_>> {
        let singleton = self.callable.singleton_dispatch.as_deref().copied();
        KlibBodyCallable::new(
            KlibBodyCallableShape {
                name: &self.callable.name,
                kind: self.kind,
                visibility: self.callable.visibility,
                semantic_role: self.callable.semantic_role,
                params: &self.callable.params,
                ret: self.callable.ret,
                parameter_identities: &self.parameter_identities,
                context_count: self.callable.context_count,
                vararg_index: self.klib_body_vararg_index,
                parameter_defaults: self.klib_body_parameter_defaults,
                generic_signature: self.callable.generic_sig.as_deref(),
                type_parameters: &self.klib_body_type_parameters,
                reified_type_parameter_ordinals: &self.callable.reified_type_parameter_ordinals,
                compiler_intrinsic: self
                    .callable
                    .compiler_intrinsic
                    .or(self.callable.member_realization.compiler_intrinsic()),
                range_construction: self.callable.member_realization.range_construction(),
                declaration_modifiers: self.klib_body_declaration_modifiers,
                class_contract: self.klib_body_class_contract.as_ref(),
                accessor: self.klib_body_accessor.as_ref(),
            },
            singleton,
            self.klib_body_companion_receiver.as_ref(),
            self.declaration_owner,
            self.dependency_callable.as_ref(),
        )
    }
}

/// A provider's realization of one normalized Kotlin property. FIR carries only its opaque
/// identity; callers read the semantic name and the independently interned physical accessors here.
#[derive(Clone, Debug)]
pub struct ExternalPropertyRealization {
    pub name: String,
    pub getter: crate::fir::ExternalCallableId,
    pub setter: Option<crate::fir::ExternalCallableId>,
    /// The provider-normalized declaration is its value class's underlying storage property.
    pub declares_value_class_storage: bool,
    /// Provider-normalized payload of a compile-time constant property.
    pub compile_time_constant: Option<LibraryConst>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NonvirtualCallRealization {
    pub owner: TypeName,
    pub descriptor: String,
}

/// Target realization inherited by an overriding virtual call from one exact declaration.
///
/// A declaration provider publishes these candidates while its semantic declaration and target
/// policy are still joined. Later target passes select one through stable override identities;
/// source/member spellings never become lookup input again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverriddenCallRealization {
    /// The source declaration facet that owns this realization. A Java getter can be both an
    /// ordinary function and the physical accessor of a synthetic Kotlin property while sharing
    /// one classfile identity; consumers must not apply the property's ABI to the function call.
    pub kind: OverriddenCallKind,
    pub declaration_owner: TypeName,
    pub physical_name: String,
    pub descriptor: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverriddenCallKind {
    Function,
    PropertyGetter,
}

#[derive(Clone, Debug)]
pub struct DefaultCallRealization {
    /// Exact platform invocation identity selected by the provider. Consumers must not reconstruct
    /// any part of this target from the source declaration.
    pub owner: TypeName,
    pub name: String,
    pub descriptor: String,
    /// Classfile that declares the target body. This can differ from `owner` when a public facade is
    /// the legal invocation owner for a method stored on one of its package parts.
    pub declaration_owner: TypeName,
    pub real_params: Vec<Ty>,
    /// Number of platform mask words before the trailing marker. Zero denotes a marker-only
    /// realization; consumers must not infer this ABI fact from the source parameter count.
    pub mask_count: usize,
    pub ret: Ty,
    pub suspend: bool,
}

impl LibraryCallable {
    /// Target spelling of this already-selected declaration. This is realization data only; source
    /// lookup, overload selection, and diagnostics use [`Self::name`].
    pub fn physical_name(&self) -> &str {
        self.physical_name.as_deref().unwrap_or(&self.name)
    }

    pub fn library(
        owner: impl Into<TypeName>,
        name: impl Into<String>,
        params: Vec<Ty>,
        ret: Ty,
        physical_ret: Ty,
        descriptor: impl Into<String>,
    ) -> Self {
        let parameter_plan = physical_parameter_plan::source_parameter_plan(params.len());
        LibraryCallable {
            deprecation: None,
            external_identity: None,
            annotations: Vec::new(),
            external_default_provider: None,
            external_property_identity: None,
            compiler_core_declaration: None,
            declaration_owner: None,
            owner: owner.into(),
            name: name.into(),
            physical_name: None,
            reflection_name: None,
            compiler_intrinsic: None,
            semantic_role: None,
            overridden_declarations: Box::new([]),
            collection_barrier: None,
            plugin_expression: None,
            inline_body_plan: None,
            physical_params: params.clone(),
            physical_parameter_plan: Some(parameter_plan),
            params,
            ret,
            physical_ret,
            descriptor: descriptor.into(),
            suspend: false,
            is_abstract: false,
            owner_is_interface: false,
            visibility: crate::types::Visibility::Public,
            member_realization: MemberRealization::Dispatch,
            inline: InlineKind::None,
            default_call: false,
            defaults_from_declaration: false,
            vararg_elem: None,
            vararg_index: None,
            signature: None,
            origin: Origin::Library,
            source_receiver: None,
            declared_params: None,
            inline_modifiers: Box::new([]),
            reified_type_parameter_ordinals: Box::new([]),
            context_count: 0,
            contract: None,
            equality_bound: None,
            generic_sig: None,
            singleton_dispatch: None,
            default_realization: None,
            nonvirtual_realization: None,
            overridden_call_realizations: Box::new([]),
            declared_ret: None,
            overridden_results: Box::new([]),
            enhanced_result: crate::libraries::EnhancedResult::NONE,
        }
    }

    /// Normalize a selected classifier constructor into the provider-owned callable identity consumed
    /// after FIR selection. Constructor declarations return their classifier semantically, while the
    /// platform invocation itself returns `Unit`; keep that boundary in one conversion so realization
    /// facets cannot drift between selection and backend registration.
    pub fn constructor(owner: TypeName, member: &LibraryMember) -> Self {
        let mut callable = Self::library(
            member.owner.unwrap_or(owner),
            member.name.clone(),
            member.params.clone(),
            Ty::Unit,
            member.physical_ret,
            member.descriptor.clone(),
        );
        callable.physical_name = member.physical_name.clone();
        callable.reflection_name = Some("<init>".to_string());
        callable.declared_params = Some(member.constructor_declaration_params().into());
        callable.semantic_role = member.semantic_role;
        callable.physical_params = member.physical_params.clone();
        callable.physical_parameter_plan = member.physical_parameter_plan.clone();
        callable.member_realization = member.realization;
        callable.default_realization = member.default_realization.clone();
        callable.external_default_provider = member.external_default_provider;
        callable.nonvirtual_realization = member.nonvirtual_realization.clone();
        callable.visibility = member.visibility;
        callable
    }

    pub fn owner_name(&self) -> String {
        self.owner.render()
    }

    pub fn owner_type(&self) -> TypeName {
        self.owner
    }

    pub fn owner_matches(&self, internal: &str) -> bool {
        self.owner.matches(internal)
    }

    pub fn owner_starts_with(&self, prefix: &str) -> bool {
        self.owner.starts_with(prefix)
    }

    pub fn owner_contains(&self, needle: &str) -> bool {
        self.owner.contains(needle)
    }

    pub fn owner_package_matches(&self, package: &str) -> bool {
        self.owner.package_matches(package)
    }

    pub fn owner_package_matches_name(&self, package: TypeName) -> bool {
        self.owner.parent() == Some(package)
    }

    pub fn owner_package(&self) -> String {
        self.owner.package()
    }
}

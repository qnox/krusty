//! Provider-normalized physical call targets carried with a selected semantic declaration.

use super::{
    physical_parameter_plan, InlineKind, LibraryCallable, LibraryMember, MemberRealization, Origin,
};
use crate::types::{Ty, TypeName};

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
            external_identity: None,
            external_default_provider: None,
            external_property_identity: None,
            owner: owner.into(),
            name: name.into(),
            physical_name: None,
            reflection_name: None,
            compiler_intrinsic: None,
            semantic_role: None,
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
        callable.physical_params = member.physical_params.clone();
        callable.physical_parameter_plan = member.physical_parameter_plan.clone();
        callable.member_realization = member.realization;
        callable.default_realization = member.default_realization.clone();
        callable.external_default_provider = member.external_default_provider;
        callable.nonvirtual_realization = member.nonvirtual_realization.clone();
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

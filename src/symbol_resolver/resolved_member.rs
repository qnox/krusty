//! Selected member declaration and its origin-neutral callable conversion.

use crate::libraries::{LibraryCallable, LibraryMember, Origin};
use crate::types::Ty;

#[derive(Clone, Debug)]
pub struct ResolvedMember {
    /// Semantic dispatch receiver selected with this overload. It is part of the call target rather
    /// than a lowering-side reconstruction, and is identical for module and dependency providers.
    pub receiver: Ty,
    pub member: LibraryMember,
    /// Declaration/ABI parameter types before call-site generic substitution. Checking uses
    /// `member.params`; module linking and argument realization use this parallel physical shape.
    pub physical_params: Vec<Ty>,
    /// One entry per leading context parameter. `Some` is supplied from scope; `None` is an
    /// explicitly named source argument. Ordinary members have an empty vector.
    pub context_args: Vec<Option<crate::resolve::ResolvedContextArgument>>,
    pub ret: Ty,
    pub projected_return_hazard: bool,
    /// A `suspend fun`: the caller threads a `Continuation` and reads the erased result as `ret`.
    pub suspend: bool,
    /// Declaration origin, which affects linkage only; it keeps local lifted functions apart.
    pub origin: Origin,
}

impl ResolvedMember {
    pub(crate) fn from_member(
        receiver: Ty,
        member: LibraryMember,
        ret: Ty,
        projected_return_hazard: bool,
        origin: Origin,
    ) -> Self {
        let physical_params = member.physical_params.clone();
        let suspend = member.suspend();
        Self {
            receiver,
            member,
            physical_params,
            context_args: Vec::new(),
            ret,
            projected_return_hazard,
            suspend,
            origin,
        }
    }

    pub(crate) fn from_callable(
        receiver: Ty,
        callable: LibraryCallable,
        projected_return_hazard: bool,
    ) -> Self {
        let origin = callable.origin;
        let suspend = callable.suspend;
        let physical_params = callable.physical_params.clone();
        let mut member = LibraryMember::new(
            callable.name,
            callable.params,
            callable.ret,
            callable.descriptor,
        );
        member.physical_name = callable.physical_name;
        member.external_identity = callable.external_identity;
        member.external_property_identity = callable.external_property_identity;
        member.physical_params = callable.physical_params;
        member.physical_parameter_plan = callable.physical_parameter_plan;
        member.owner = Some(callable.owner);
        member.physical_ret = callable.physical_ret;
        member.signature = callable.signature;
        member.generic_sig = callable.generic_sig.map(|signature| *signature);
        member.declared_params = callable.declared_params;
        member.set_is_interface(callable.owner_is_interface);
        member.set_is_abstract(callable.is_abstract);
        member.set_suspend(callable.suspend);
        member.realization = callable
            .compiler_intrinsic
            .map(crate::libraries::MemberRealization::Intrinsic)
            .unwrap_or(callable.member_realization);
        member.context_count = callable.context_count;
        member.inline = callable.inline;
        member.inline_body_plan = callable.inline_body_plan;
        member.declared_ret = callable.declared_ret;
        member.overridden_results = callable.overridden_results;
        member.enhanced_result = callable.enhanced_result;
        member.contract = callable.contract;
        member.default_realization = callable.default_realization;
        member.nonvirtual_realization = callable.nonvirtual_realization;
        member.plugin_expression = callable.plugin_expression;
        member.deprecation = callable.deprecation;
        Self {
            receiver,
            ret: member.ret,
            member,
            physical_params,
            context_args: Vec::new(),
            projected_return_hazard,
            suspend,
            origin,
        }
    }
}

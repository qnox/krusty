//! The JVM shape of a member of this module that the backend knows only through checked facts,
//! such as an interface default a class inherits from another file.
//!
//! The declaring file's value-class pass names and erases the member itself. A forwarder emitted
//! elsewhere spells the same member from its declared signature, against the value classes the IR
//! already knows (`IrFile::value_class_underlying_name`), so both sides agree on kotlinc's hashed
//! name and carrier descriptor.

use super::member_names::vc_mangle;
use super::{erase, Under};
use crate::ir::IrFile;
use crate::types::Ty;

/// kotlinc's JVM name for the member: `base`, with the value-class hash when its declared
/// signature mentions a value class, spelled as the declaring file spells it.
pub(crate) fn module_member_jvm_name(
    ir: &IrFile,
    base: &str,
    params: &[Ty],
    ret: &Ty,
    is_suspend: bool,
) -> String {
    vc_mangle(base, params, ret, ir, false, is_suspend)
}

/// The type a declared parameter or result occupies in the member's JVM descriptor: a value class
/// is its carrier, as the declaring file's value-class pass lowers it; any other type is unchanged.
fn module_member_carrier(ir: &IrFile, ty: Ty) -> Ty {
    let mut declarations = Under::new();
    let mut pending = Vec::new();
    super::declaration_inventory::collect_classifier_names(ty, &mut pending);
    while let Some(classifier) = pending.pop() {
        if declarations.contains_key(&classifier) {
            continue;
        }
        let Some(underlying) = super::boxed_value_class_underlying(ir, classifier) else {
            continue;
        };
        declarations.insert(
            classifier,
            underlying.scalar_value_repr().unwrap_or(underlying),
        );
        super::declaration_inventory::collect_classifier_names(underlying, &mut pending);
    }
    erase(&ty, &declarations)
}

/// The parameter and result types a forwarder to an inherited member declares: the carriers of a
/// member this module declares elsewhere, as its declaring file's value-class pass lowers them, or
/// the recorded physical and semantic types of a dependency's member. kotlinc's forwarders declare,
/// guard and annotate the lowered member, so a value-class parameter or result of this module is
/// its carrier throughout.
pub(crate) struct ForwardedMemberTypes {
    pub(crate) physical_params: Vec<Ty>,
    pub(crate) physical_ret: Ty,
    pub(crate) semantic_params: Vec<Ty>,
    pub(crate) semantic_ret: Ty,
}

pub(crate) fn forwarded_member_types(
    ir: &IrFile,
    member: &crate::backend::BackendMemberFact,
    declared_in_module: bool,
) -> ForwardedMemberTypes {
    if declared_in_module {
        let params: Vec<Ty> = member
            .params
            .iter()
            .map(|ty| module_member_carrier(ir, *ty))
            .collect();
        let ret = module_member_carrier(ir, member.ret);
        return ForwardedMemberTypes {
            physical_params: params.clone(),
            physical_ret: ret,
            semantic_params: params,
            semantic_ret: ret,
        };
    }
    // A dependency's physical shape is its JVM realization: a `suspend` member's ends with the
    // continuation, which the forwarder appends itself.
    let declared = member.params.len();
    assert_eq!(
        member.physical_params.len(),
        declared + usize::from(member.suspend),
        "a normalized dependency member publishes one physical type per semantic parameter, \
         plus the continuation of a suspend member"
    );
    ForwardedMemberTypes {
        physical_params: member.physical_params[..declared].to_vec(),
        physical_ret: member.physical_ret,
        semantic_params: member.params.to_vec(),
        semantic_ret: member.ret,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendMemberFact, BackendMemberName};

    fn dependency_member(
        physical_params: &[Ty],
        params: &[Ty],
        suspend: bool,
    ) -> BackendMemberFact {
        BackendMemberFact {
            name: BackendMemberName::Declared("run".into()),
            physical_name: None,
            owner: Some(crate::types::type_name("fixture/Api")),
            physical_params: physical_params.into(),
            params: params.into(),
            ret: Ty::Unit,
            physical_ret: Ty::Unit,
            descriptor: "".into(),
            realization: crate::libraries::MemberRealization::Dispatch,
            suspend,
            abstract_member: false,
            visibility: crate::types::Visibility::Public,
            parameter_identities: Box::new([]),
        }
    }

    #[test]
    fn a_suspend_dependency_member_forwards_its_parameters_without_the_continuation() {
        let continuation = Ty::obj("fixture/Continuation");
        let member = dependency_member(&[Ty::Int, continuation], &[Ty::Int], true);

        let types = forwarded_member_types(&IrFile::default(), &member, false);

        assert_eq!(types.physical_params, vec![Ty::Int]);
        assert_eq!(types.semantic_params, vec![Ty::Int]);
    }

    #[test]
    #[should_panic(expected = "a normalized dependency member publishes one physical type")]
    fn a_dependency_member_without_a_complete_physical_shape_is_rejected() {
        let member = dependency_member(&[], &[Ty::Int], false);

        forwarded_member_types(&IrFile::default(), &member, false);
    }
}

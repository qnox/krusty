//! Declaration-owned member-extension inputs at one dispatch receiver rung.

use super::{Checker, MemberExtFunSig};
use crate::libraries::{LibraryMember, ReceiverInputBounds};
use crate::symbol_resolver::GSigBinds;
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

#[derive(Default)]
pub(super) struct MemberExtensionDeclarations {
    pub(super) functions: Vec<(LibraryMember, MemberExtFunSig)>,
    pub(super) bindings: GSigBinds,
    pub(super) input_bounds: ReceiverInputBounds,
}

/// Signature solving and body checking consume the same provider-normalized declaration frame.
pub(super) fn at_receiver(
    source: &dyn SymbolSource,
    receiver: Ty,
    name: &str,
) -> MemberExtensionDeclarations {
    let Some(owner) = receiver.obj_internal() else {
        return MemberExtensionDeclarations::default();
    };
    let Some(declaration) = source.classifier(owner) else {
        return MemberExtensionDeclarations::default();
    };
    MemberExtensionDeclarations {
        functions: declaration
            .members
            .iter()
            .filter(|member| member.name == name && member.is_member_extension())
            .filter_map(|member| {
                Checker::semantic_member_ext_fun(member)
                    .map(|signature| (member.clone(), signature))
            })
            .collect(),
        bindings: declaration
            .type_params()
            .iter()
            .cloned()
            .zip(receiver.type_args().iter().copied())
            .collect(),
        input_bounds: ReceiverInputBounds::from_parameters(&declaration.type_parameters),
    }
}

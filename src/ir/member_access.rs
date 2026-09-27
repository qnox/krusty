//! Who may call a member: each method's declared visibility, and the calls of `protected`
//! members declared by a class of another package.
//!
//! Kotlin lets a subclass, and any code written inside one, call its supertypes' protected members.
//! A target whose own access rules are narrower than Kotlin's decides from this record how such a
//! call reaches the member; common IR only states what the checked call selected.

use super::{FunId, IrFile, Ty, TypeName};

/// One checked call of a protected member, an accessor call of a protected property included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrProtectedMemberCall {
    /// The class that declares the member, never a subclass inheriting it.
    pub declaring: TypeName,
    /// The classifier of the call's checked dispatch receiver.
    pub receiver: TypeName,
    /// The semantic names of the declaring class's type parameters, in order.
    pub declaring_type_parameters: Vec<String>,
    /// The member's declared parameters, in call order.
    pub parameter_identities: Vec<crate::fir::ResolvedParameterIdentity>,
}

impl IrProtectedMemberCall {
    /// A declared parameter or result type as the receiver sees it: the declaring class's type
    /// parameters bound to the arguments of the receiver's supertype.
    pub fn substitute(&self, ir: &IrFile, ty: Ty) -> Ty {
        let Some(applied) = ir
            .classifier_hierarchies
            .get(&self.receiver)
            .and_then(|hierarchy| {
                hierarchy
                    .iter()
                    .find(|applied| applied.classifier == self.declaring)
            })
        else {
            return ty;
        };
        let bindings = self
            .declaring_type_parameters
            .iter()
            .cloned()
            .zip(applied.applied.type_args().iter().copied())
            .collect();
        crate::types::ty_subst_keep_unbound(ty, &bindings)
    }
}

impl IrFile {
    /// A method's Kotlin declaration visibility. Public is the compact default for generated and
    /// ordinary declarations; every non-public source or generated method is recorded explicitly.
    pub fn method_visibility(&self, function: FunId) -> crate::types::Visibility {
        self.method_visibilities
            .get(&function)
            .copied()
            .unwrap_or(crate::types::Visibility::Public)
    }

    pub fn set_method_visibility(&mut self, function: FunId, visibility: crate::types::Visibility) {
        if visibility.is_public() {
            self.method_visibilities.remove(&function);
        } else {
            self.method_visibilities.insert(function, visibility);
        }
    }
}

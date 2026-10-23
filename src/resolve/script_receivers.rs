//! A script's implicit receiver.
//!
//! kotlinc gives a script two receivers that share one runtime value: the script itself, whose
//! declarations it reaches directly, and its template, which supplies every inherited member
//! (`args`, `hashCode()`). A member reached through the template is a member of the template's
//! type, so it reads the script instance as that type, exactly like a flow-narrowed `this`.

use super::*;

impl Checker<'_> {
    /// Whether `ty` is this file's script class.
    pub(super) fn is_script_receiver(&self, ty: Ty) -> bool {
        self.script_class_name()
            .is_some_and(|script| ty.non_null().obj_internal() == Some(script))
    }

    fn script_class_name(&self) -> Option<TypeName> {
        let script = self.file.script_class?;
        let Decl::Class(class) = self.file.decl(script) else {
            return None;
        };
        self.active_classifier_internal(script, class)
    }

    /// Read every settled template member through the script's template. A member the script
    /// declares itself keeps the script as its receiver.
    pub(super) fn narrow_script_template_receivers(&mut self) {
        if self.script_receiver_members.is_empty() {
            return;
        }
        let Some(script) = self.script_class_name() else {
            return;
        };
        let Some(template) =
            crate::assignable::TypeOracle::direct_supertypes(self, Ty::obj_name(script))
                .into_iter()
                .find_map(|supertype| supertype.obj_internal())
        else {
            return;
        };
        let members = std::mem::take(&mut self.script_receiver_members);
        for member in members {
            let owner = match (
                self.resolved_calls.get(&member),
                self.expr_lowers.get(&member),
            ) {
                (Some(ResolvedCall::Member(selected)), _) => selected.member.owner,
                (_, Some(ExprLowering::MemberPropertyRead { owner, .. })) => Some(*owner),
                _ => None,
            };
            if owner.is_some_and(|owner| owner != script) {
                self.narrowed_this_member.insert(member, template);
            }
        }
    }
}

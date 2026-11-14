//! Fail-closed JVM owner realization at the final call-emission boundary.

use super::Emitter;

impl Emitter<'_> {
    pub(super) fn fail_member_dispatch(&self, missing: crate::member_dispatch::MissingClassifier) {
        krusty::trace_compiler!("emit", "JVM member dispatch: {missing}");
        self.run.set_emit_error(
            "member dispatch is missing a checked backend classifier fact".to_string(),
        );
    }

    pub(super) fn module_member_call_owner(
        &self,
        expression: krusty::ir::ExprId,
        declared: krusty::types::TypeName,
        declared_interface: bool,
    ) -> Option<(String, bool)> {
        match crate::member_dispatch::call_owner(
            self.dispatch_classifiers.as_ref(),
            declared,
            declared_interface,
            self.ir.dispatch_classes.get(&expression).copied(),
        ) {
            Ok((owner, interface)) => Some((owner.render(), interface)),
            Err(missing) => {
                self.fail_member_dispatch(missing);
                None
            }
        }
    }

    pub(super) fn checked_dispatched_accessor(
        &self,
        expression: krusty::ir::ExprId,
        access: krusty::jvm::inline::PropertyAccess,
    ) -> Option<krusty::jvm::inline::PropertyAccess> {
        match self.dispatched_accessor(expression, access) {
            Ok(access) => Some(access),
            Err(missing) => {
                self.fail_member_dispatch(missing);
                None
            }
        }
    }

    pub(super) fn source_virtual_call_owner(
        &self,
        expression: krusty::ir::ExprId,
        declared: krusty::types::TypeName,
        declared_interface: bool,
        receiver: krusty::ir::ExprId,
    ) -> Option<(String, bool)> {
        match crate::member_dispatch::virtual_owner(
            self.dispatch_classifiers.as_ref(),
            declared,
            declared_interface,
            crate::member_dispatch::VirtualReceiver {
                dispatch: self.ir.dispatch_classes.get(&expression).copied(),
                value_type: Some(self.value_ty(receiver)),
                annotation_construction: crate::annotation_constructions::construction_interface(
                    self.ir, receiver,
                ),
                semantic_role: self.ir.semantic_call_roles.get(&expression).copied(),
                declaration_owner: None,
            },
        ) {
            Ok((owner, interface)) => Some((owner.render(), interface)),
            Err(missing) => {
                self.fail_member_dispatch(missing);
                None
            }
        }
    }
}

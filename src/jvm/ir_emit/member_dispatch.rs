//! Fail-closed JVM owner realization at the final call-emission boundary.

use super::Emitter;

impl Emitter<'_> {
    pub(super) fn fail_member_dispatch(
        &self,
        missing: crate::jvm::member_dispatch::MissingClassifier,
    ) {
        crate::trace_compiler!("emit", "JVM member dispatch: {missing}");
        self.run.set_emit_error(
            "member dispatch is missing a checked backend classifier fact".to_string(),
        );
    }

    pub(super) fn module_member_call_owner(
        &self,
        expression: crate::ir::ExprId,
        declared: crate::types::TypeName,
        declared_interface: bool,
    ) -> Option<(String, bool)> {
        match crate::jvm::member_dispatch::call_owner(
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
        expression: crate::ir::ExprId,
        access: crate::jvm::inline::PropertyAccess,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
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
        expression: crate::ir::ExprId,
        declared: crate::types::TypeName,
        declared_interface: bool,
        receiver: crate::types::Ty,
    ) -> Option<(String, bool)> {
        match crate::jvm::member_dispatch::virtual_owner(
            self.dispatch_classifiers.as_ref(),
            declared,
            declared_interface,
            self.ir.dispatch_classes.get(&expression).copied(),
            Some(receiver),
        ) {
            Ok((owner, interface)) => Some((owner.render(), interface)),
            Err(missing) => {
                self.fail_member_dispatch(missing);
                None
            }
        }
    }
}

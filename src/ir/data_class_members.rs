//! Exact common-IR identities for compiler-synthesized data-class declarations.

use super::{FunId, IrFile};
use crate::types::TypeName;

/// Semantic role of one compiler-synthesized data-class function.
///
/// The role is recorded where common lowering binds the compiler-generated declaration to its
/// [`FunId`]. Backends consume that identity directly; neither a physical rename nor a coincidental
/// source spelling can change which declaration owns the role.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum IrDataClassMemberRole {
    /// Zero-based primary-constructor property ordinal.
    Component(u32),
    Copy,
    Equals,
    HashCode,
    ToString,
}

impl IrFile {
    pub(crate) fn record_data_class_member(
        &mut self,
        owner: TypeName,
        role: IrDataClassMemberRole,
        function: FunId,
    ) {
        assert_eq!(
            self.functions
                .get(function as usize)
                .and_then(|declaration| declaration.dispatch_receiver),
            Some(owner),
            "a data-class role is bound to a member of its semantic owner"
        );
        assert!(
            self.synthesized_data_class_members
                .insert((owner, role), function)
                .is_none(),
            "one compiler-generated data-class declaration owns each semantic role"
        );
        // kotlinc gives a synthesized data-class member a `LocalVariableTable` (its receiver and
        // parameters) but no line, and writes that table with the method.
        self.fn_debug_locals.insert(function);
    }

    pub(crate) fn data_class_member(
        &self,
        owner: TypeName,
        role: IrDataClassMemberRole,
    ) -> Option<FunId> {
        self.synthesized_data_class_members
            .get(&(owner, role))
            .copied()
    }

    pub(crate) fn is_data_class_member(&self, owner: TypeName, function: FunId) -> bool {
        self.synthesized_data_class_members
            .iter()
            .any(|((registered_owner, _), registered)| {
                *registered_owner == owner && *registered == function
            })
    }
}

#[cfg(test)]
mod tests {
    use super::IrDataClassMemberRole;
    use crate::ir::{IrFile, IrFunction};
    use crate::types::{type_name, Ty};

    #[test]
    fn roles_retain_exact_function_identity_across_a_physical_rename() {
        let mut ir = IrFile::default();
        let owner = type_name("fixtures/InventoryRecord");
        let component = ir.add_fun(IrFunction {
            name: "component1".to_owned(),
            params: Vec::new(),
            ret: Ty::String,
            body: None,
            is_static: false,
            dispatch_receiver: Some(owner),
            param_checks: Vec::new(),
        });
        ir.record_data_class_member(owner, IrDataClassMemberRole::Component(0), component);

        ir.functions[component as usize].name = "component1-physical".to_owned();
        let unrelated = ir.add_fun(IrFunction {
            name: "component1".to_owned(),
            params: Vec::new(),
            ret: Ty::String,
            body: None,
            is_static: false,
            dispatch_receiver: Some(owner),
            param_checks: Vec::new(),
        });

        assert_eq!(
            ir.data_class_member(owner, IrDataClassMemberRole::Component(0)),
            Some(component)
        );
        assert!(ir.is_data_class_member(owner, component));
        assert!(!ir.is_data_class_member(owner, unrelated));
    }
}

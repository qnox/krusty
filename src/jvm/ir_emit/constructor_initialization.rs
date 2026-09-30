//! JVM ordering of primary-constructor initialization work.
//!
//! Interface-delegate expressions run before constructor-property stores, while property
//! initializers and init blocks run after them. Common IR retains the checked field identities;
//! this backend boundary schedules their physical stores without reconstructing source semantics.

use super::*;

fn interface_delegation_storage(ir: &IrFile, class: crate::ir::ClassId) -> HashSet<u32> {
    ir.checked_classifier_classes
        .iter()
        .filter(|(_, &id)| id == class)
        .flat_map(|(declaration, _)| {
            ir.checked_interface_delegation_fields
                .iter()
                .filter(move |((owner, _), _)| owner == declaration)
                .map(|(_, &field)| field)
        })
        .collect()
}

/// Pull interface-delegation stores out of an initializer. They run before constructor-property
/// stores; everything else keeps its relative order and runs after those stores.
fn partition_interface_delegation(
    ir: &IrFile,
    class: crate::ir::ClassId,
    fields: &HashSet<u32>,
    expression: crate::ir::ExprId,
    delegation: &mut Vec<crate::ir::ExprId>,
    rest: &mut Vec<crate::ir::ExprId>,
) {
    match ir.expr(expression) {
        IrExpr::Block { stmts, value: None } => {
            let mut inner_delegation = Vec::new();
            let mut inner_rest = Vec::new();
            for &statement in stmts {
                partition_interface_delegation(
                    ir,
                    class,
                    fields,
                    statement,
                    &mut inner_delegation,
                    &mut inner_rest,
                );
            }
            if inner_rest.is_empty() {
                delegation.extend(inner_delegation);
            } else if inner_delegation.is_empty() {
                rest.push(expression);
            } else {
                delegation.extend(inner_delegation);
                rest.extend(inner_rest);
            }
        }
        IrExpr::SetField {
            class: owner,
            index,
            ..
        } if *owner == class && fields.contains(index) => {
            delegation.push(expression);
        }
        _ => rest.push(expression),
    }
}

impl Emitter<'_> {
    /// Emit interface-delegation stores and return the initializer statements that still run after
    /// constructor-property parameters are stored.
    pub(super) fn emit_interface_delegation_initializers(
        &mut self,
        class: crate::ir::ClassId,
        init_body: crate::ir::ExprId,
        code: &mut CodeBuilder,
    ) -> Vec<crate::ir::ExprId> {
        let fields = interface_delegation_storage(self.ir, class);
        if fields.is_empty() {
            return vec![init_body];
        }
        let mut delegation = Vec::new();
        let mut rest = Vec::new();
        partition_interface_delegation(
            self.ir,
            class,
            &fields,
            init_body,
            &mut delegation,
            &mut rest,
        );
        for statement in delegation {
            self.emit(statement, code);
        }
        rest
    }

    /// Emit initializer statements left after interface delegation. Source lines are the marks
    /// emission writes; the constructor copies those into its curated line table.
    pub(super) fn emit_initializer_statements(
        &mut self,
        statements: &[crate::ir::ExprId],
        code: &mut CodeBuilder,
    ) {
        for &statement in statements {
            self.emit(statement, code);
        }
    }
}

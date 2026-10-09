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
    /// Store the constructor fields marked pre-super: an inner class's enclosing instance and a
    /// local class's captured values, which a superclass argument may read. Keeping this as
    /// ordering metadata avoids interpreting a JVM field name as source semantics. A `putfield` of
    /// the current class's own field on the still-uninitialized `this` is legal per JVMS 4.10.2.4.
    ///
    /// Returns the offset the constructor's first line starts at. Debug metadata consumes the
    /// position emission actually reached: reconstructing it later from constructor parameters,
    /// constant-pool widths, or assertion policy makes a semantic description masquerade as
    /// bytecode layout and can point inside an opcode.
    pub(super) fn emit_pre_super_field_stores(
        &mut self,
        class: &crate::ir::IrClass,
        owner: &str,
        param_tys: &[Ty],
        code: &mut CodeBuilder,
    ) -> usize {
        let mut debug_start = code.bytes.len();
        for &(param_i, field_i) in &class.pre_super_param_fields {
            let param_i = param_i as usize;
            let field = &class.fields[field_i as usize];
            let param_slot = 1 + param_tys[..param_i]
                .iter()
                .map(|ty| slot_words(*ty))
                .sum::<u16>();
            code.aload(0);
            load(param_tys[param_i], param_slot, code);
            let physical_name = instance_field_jvm_name(self.ir, self.run, class, field_i as usize);
            let field_ref = self.cw.fieldref(
                owner,
                &physical_name,
                &type_descriptor(jvm_value_ty(&field.ty)),
            );
            code.putfield(field_ref, slot_words(field.ty) as i32);
            // A local class's captured values are stored without a line; its first line is what
            // follows them: an inner class's enclosing-instance store, else the delegation.
            let provenance = class
                .ctor_args
                .get(param_i)
                .map(|argument| argument.provenance);
            if class.is_local_class
                && provenance != Some(crate::ir::IrCtorParameterProvenance::EnclosingInstance)
            {
                debug_start = code.bytes.len();
            }
        }
        debug_start
    }

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

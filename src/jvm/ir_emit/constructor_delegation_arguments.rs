//! Operands of a constructor's `super(…)` or `this(…)` delegation.
//!
//! A delegation is not an IR call node, so its arguments are adapted here against the selected
//! target constructor's declared parameters, exactly as a construction's arguments are: a
//! substituted scalar passed to an erased generic parameter is boxed.

use super::*;

impl Emitter<'_> {
    /// Push `this`, the forwarded owner prefix (an enum's `name, ordinal`), and the adapted
    /// delegation arguments. A branchy argument cannot run with the uninitialized `this` on the
    /// stack, so such an argument list is first spilled to temporaries.
    pub(super) fn emit_constructor_delegation_arguments(
        &mut self,
        arguments: &[crate::ir::ExprId],
        declared: &[Ty],
        forwarded_prefix: &[Ty],
        code: &mut CodeBuilder,
    ) {
        if arguments.len() != declared.len() {
            self.run.set_emit_error(format!(
                "constructor delegation passes {} arguments to {} parameters",
                arguments.len(),
                declared.len()
            ));
            return;
        }
        let physical = jvm_tys(declared);
        let spilled = arguments
            .iter()
            .any(|&argument| self.emits_control_flow(argument))
            .then(|| self.spill_to_temps(arguments, code));
        code.aload(0);
        let mut slot = 1;
        for &ty in forwarded_prefix {
            load(ty, slot, code);
            slot += slot_words(ty);
        }
        for (index, &argument) in arguments.iter().enumerate() {
            let source = match &spilled {
                Some(temps) => {
                    let (slot, source, _) = temps[index];
                    load(source, slot, code);
                    source
                }
                None => {
                    self.emit_value(argument, code);
                    self.value_ty(argument)
                }
            };
            let semantic = self
                .ir
                .logical_types
                .get(&argument)
                .copied()
                .unwrap_or(source);
            self.adapt_physical_operand(
                source,
                semantic,
                Some(declared[index]),
                physical[index],
                code,
            );
        }
        if let Some(temps) = spilled {
            self.release_operand_spills(&temps);
        }
    }
}

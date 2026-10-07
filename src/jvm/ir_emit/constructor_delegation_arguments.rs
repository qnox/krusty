//! Operands of a constructor's `super(…)` or `this(…)` delegation.
//!
//! A delegation is not an IR call node, so its arguments are adapted here against the selected
//! target constructor's declared parameters, exactly as a construction's arguments are: a
//! substituted scalar passed to an erased generic parameter is boxed, while an erased generic
//! call result consumed by that same reference parameter is not narrowed first.

use super::*;

impl Emitter<'_> {
    /// Push `this`, the forwarded owner prefix (an enum's `name, ordinal`), and the adapted
    /// delegation arguments. An argument that cannot carry the operand stack (a handler, a
    /// suspension, a loop transfer, or a branching inline splice that takes a literal) cannot run
    /// with the uninitialized `this` on it, so such an argument list is first spilled to
    /// temporaries. Other branching arguments keep the prefix on the stack, as kotlinc does.
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
            .any(|&argument| self.spills_operand_prefix(argument))
            .then(|| {
                arguments
                    .iter()
                    .map(|&argument| {
                        let source = self.emit_consumed_operand(argument, code);
                        self.spill_operand(argument, source, code)
                    })
                    .collect::<Vec<_>>()
            });
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
                None => self.emit_consumed_operand(argument, code),
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

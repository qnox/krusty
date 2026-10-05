//! The construction of one enum entry: its checked argument prelude, the enum's `(String, int)`
//! name and ordinal, the supplied arguments and the defaulted ones' masks.
//!
//! A plain entry is constructed in the enum's `<clinit>`. An entry with a body is constructed in
//! its subclass's constructor, which receives only the name and ordinal and calls the enum
//! constructor itself, as kotlinc compiles it.

use super::*;

impl Emitter<'_> {
    /// Emit `entry`'s argument prelude, then `push_prefix`, which leaves the receiver, name and
    /// ordinal on the stack, then each argument. Returns the parameters the constructor call takes
    /// after the name and ordinal: the selected constructor's, or its default-argument overload's
    /// with the masks and marker.
    pub(super) fn emit_enum_entry_arguments(
        &mut self,
        entry: &crate::ir::IrEnumEntry,
        code: &mut CodeBuilder,
        push_prefix: impl FnOnce(&mut Self, &mut CodeBuilder),
    ) -> Vec<Ty> {
        for &statement in &entry.argument_prelude {
            self.emit(statement, code);
        }
        let args = &entry.args;
        // An entry arg that cannot carry the operand stack (`X(try { 1 } finally {})`) runs on a
        // clean stack: spill all args to temps first, then construct (as the `New` node does).
        let spill = args.iter().any(|&a| self.spills_operand_prefix(a));
        let temps = if spill {
            self.spill_to_temps(args, code)
        } else {
            Vec::new()
        };
        push_prefix(self, code);
        let parameter_types = &entry.constructor_parameter_types;
        if spill {
            let mut supplied = temps.iter();
            for (parameter, ty) in parameter_types.iter().copied().enumerate() {
                if entry.default_parameters.contains(&(parameter as u32)) {
                    push_zero(ty, code, self.cw);
                } else {
                    let (slot, ty, _) = supplied
                        .next()
                        .expect("checked enum constructor supplied-argument count");
                    load(*ty, *slot, code);
                }
            }
            self.release_operand_spills(&temps);
        } else {
            let mut supplied = args.iter().copied();
            for (parameter, ty) in parameter_types.iter().copied().enumerate() {
                if entry.default_parameters.contains(&(parameter as u32)) {
                    push_zero(ty, code, self.cw);
                } else {
                    self.emit_value(
                        supplied
                            .next()
                            .expect("checked enum constructor supplied-argument count"),
                        code,
                    );
                }
            }
        }
        let mut parameters = parameter_types.clone();
        if !entry.default_parameters.is_empty() {
            emit_constructor_default_arguments(
                &entry.default_parameters,
                parameter_types.len(),
                code,
                self.cw,
            );
            parameters.extend(std::iter::repeat_n(
                Ty::Int,
                default_mask_count(parameter_types.len()),
            ));
            parameters.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
        }
        parameters
    }
}

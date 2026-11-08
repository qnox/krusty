//! The construction of an enum's entries in its `<clinit>`.

use super::*;

impl Emitter<'_> {
    /// Construct and store each of `c`'s entries, appending the line entries they mark to
    /// `lines`.
    ///
    /// EnumClassLowering builds each entry's construction as a constructor call at the entry's
    /// offsets, and kotlinc's `visitConstructorCall` marks that line at the `new`, lets the
    /// arguments mark their own lines, and marks the entry's line again at the `invokespecial`.
    /// Consecutive entries on one source line share one entry. `add_method` drops a
    /// `<clinit>`'s marks, so they are collected here.
    pub(super) fn emit_enum_entry_constructions(
        &mut self,
        c: &IrClass,
        fq: &str,
        self_desc: &str,
        clinit: &mut CodeBuilder,
        lines: &mut Vec<(u16, u32)>,
    ) {
        for (i, entry) in c.enum_entries.iter().enumerate() {
            let first_mark = clinit.line_marks().len();
            if entry.decl_line != 0 {
                clinit.mark_line(entry.decl_line);
            }
            for &statement in &entry.argument_prelude {
                self.emit(statement, clinit);
            }
            let args = &entry.args;
            // An entry arg that cannot carry the operand stack (`X(try { 1 } finally {})`) runs on a
            // clean stack: spill all args to temps first, then construct (as the `New` node does).
            let spill = args.iter().any(|&a| self.spills_operand_prefix(a));
            let temps = if spill {
                self.spill_to_temps(args, clinit)
            } else {
                Vec::new()
            };
            // A bodied entry is an instance of its synthesized subclass (`new Enum$ENTRY(...)`); the
            // subclass constructor shares the enum's `(String,int,<user>)V` descriptor.
            let new_class = entry
                .subclass
                .map(TypeName::render)
                .unwrap_or_else(|| fq.to_string());
            let cls = self.cw.class_ref(&new_class);
            clinit.new_obj(cls);
            clinit.dup();
            clinit.push_string(&entry.name, self.cw);
            clinit.push_int(i as i32, self.cw);
            let entry_parameter_types = &entry.constructor_parameter_types;
            // The entry owns the operands a default-argument realization synthesizes for it, so its
            // line goes back into effect at the start of each run of them, as for any call.
            let mut inside_run = false;
            let mut mark_run_start = |synthesized: bool, code: &mut CodeBuilder| {
                if synthesized && !inside_run && entry.decl_line != 0 {
                    code.mark_line(entry.decl_line);
                }
                inside_run = synthesized;
            };
            if spill {
                let mut supplied = temps.iter();
                for (parameter, ty) in entry_parameter_types.iter().copied().enumerate() {
                    let omitted = entry.default_parameters.contains(&(parameter as u32));
                    mark_run_start(omitted, clinit);
                    if omitted {
                        push_zero(ty, clinit, self.cw);
                    } else {
                        let (slot, ty, _) = supplied
                            .next()
                            .expect("checked enum constructor supplied-argument count");
                        load(*ty, *slot, clinit);
                    }
                }
                self.release_operand_spills(&temps);
            } else {
                let mut supplied = args.iter().copied();
                for (parameter, ty) in entry_parameter_types.iter().copied().enumerate() {
                    let omitted = entry.default_parameters.contains(&(parameter as u32));
                    mark_run_start(omitted, clinit);
                    if omitted {
                        push_zero(ty, clinit, self.cw);
                    } else {
                        self.emit_value(
                            supplied
                                .next()
                                .expect("checked enum constructor supplied-argument count"),
                            clinit,
                        );
                    }
                }
            }
            let mut entry_ctor_params = vec![Ty::String, Ty::Int];
            entry_ctor_params.extend(entry_parameter_types.iter().copied());
            let (entry_ctor_desc, entry_ctor_argw) = if entry.default_parameters.is_empty() {
                let words = entry_ctor_params
                    .iter()
                    .map(|ty| slot_words(*ty) as i32)
                    .sum();
                (method_descriptor(&entry_ctor_params, Ty::Unit), words)
            } else {
                mark_run_start(true, clinit);
                emit_constructor_default_arguments(
                    &entry.default_parameters,
                    entry_parameter_types.len(),
                    clinit,
                    self.cw,
                );
                entry_ctor_params.extend(std::iter::repeat_n(
                    Ty::Int,
                    default_mask_count(entry_parameter_types.len()),
                ));
                entry_ctor_params.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
                let words = entry_ctor_params
                    .iter()
                    .map(|ty| slot_words(*ty) as i32)
                    .sum();
                (method_descriptor(&entry_ctor_params, Ty::Unit), words)
            };
            let ctor_ref = self.cw.methodref(&new_class, "<init>", &entry_ctor_desc);
            if entry.decl_line != 0 {
                clinit.mark_line(entry.decl_line);
            }
            clinit.invokespecial(ctor_ref, entry_ctor_argw, 0);
            let fref = self.cw.fieldref(fq, &entry.name, self_desc);
            clinit.putstatic(fref, 1);
            let marks = clinit.line_marks()[first_mark..]
                .iter()
                .map(|&(pc, line)| (pc, u32::from(line)));
            lines.extend(marks);
            lines.dedup();
        }
    }
}

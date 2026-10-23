//! JVM emission of a primary constructor's `super(…)` delegation.
//!
//! kotlinc builds the primary constructor's delegating call at the declaration's start, annotations
//! included, so its operands mark their own lines and the call itself is back on that line. The
//! placeholders of omitted defaults, the mask words and their marker belong to the call as well;
//! the marker of a private constructor's accessor has no position of its own.

use super::*;

impl Emitter<'_> {
    /// Push the delegation's prelude and operands and call the superclass constructor. Returns the
    /// line entries the delegation wrote, for the primary constructor's table.
    pub(super) fn emit_primary_super_call(
        &mut self,
        c: &IrClass,
        superclass: &str,
        ctor: &mut CodeBuilder,
    ) -> Vec<(u16, u32)> {
        let marks_before = ctor.line_marks().len();
        // The table already opens on the delegation's line; an operand on that line adds no entry.
        let delegation_line = c.primary_delegation_line();
        if let Some(line) = delegation_line {
            ctor.mark_line(line);
        }
        for &statement in &c.super_arg_prelude {
            self.emit(statement, ctor);
        }
        // A base whose primary ctor takes a value-class param — or a SEALED base — has a PRIVATE
        // primary. The checker records whether this exact selection is that primary; only then
        // must a subclass `super(…)` reach it through the PUBLIC|SYNTHETIC
        // `(…args, DefaultConstructorMarker)` accessor rather than the inaccessible declaration.
        let (mut super_param_tys, super_accessor) = super_ctor_jvm_tys(self.ir, c, superclass);
        // A local superclass takes its captures in its own physical order.
        let super_ctor_params =
            capture_order::physical_for(self.ir, c.superclass, c.super_ctor_params.clone());
        let super_args = capture_order::physical_for(self.ir, c.superclass, c.super_args.clone());
        // kotlinc places a script's delegation operands, but not the call, where the script starts.
        // A script whose template is not on the classpath passes none, so it has no such line.
        let operand_line = (c.is_script && c.decl_start_line != 0 && !super_args.is_empty())
            .then_some(c.decl_start_line);
        self.emit_constructor_delegation_arguments(
            &super_args,
            &super_ctor_params,
            &[],
            operand_line,
            ctor,
        );
        // The call is back on the delegation's line after an operand on another, at its first
        // default mask when it has one. The marker an accessor call appends carries no position,
        // so without masks the line returns at the call itself.
        let super_defaults = self
            .ir
            .super_constructor_default_arguments
            .get(&c.fq_name_id())
            .map(Vec::as_slice)
            .unwrap_or_default();
        if let Some(line) = delegation_line.filter(|_| !super_defaults.is_empty()) {
            ctor.mark_line(line);
        }
        if !super_defaults.is_empty() {
            super_param_tys = jvm_tys(&super_ctor_params);
            for mask in constructor_default_masks(super_defaults, super_ctor_params.len()) {
                ctor.push_int(mask, self.cw);
                super_param_tys.push(Ty::Int);
            }
            ctor.aconst_null();
            super_param_tys.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
        } else if super_accessor {
            ctor.aconst_null();
        }
        if let Some(line) = delegation_line.filter(|_| super_defaults.is_empty()) {
            ctor.mark_line(line);
        }
        let aw: i32 = super_param_tys.iter().map(|t| slot_words(*t) as i32).sum();
        let super_descriptor = self
            .ir
            .external_super_constructors
            .get(&c.fq_name_id())
            .and_then(|target| target.descriptor.as_deref())
            .map(str::to_owned)
            .unwrap_or_else(|| method_descriptor(&super_param_tys, Ty::Unit));
        let super_init = self.cw.methodref(superclass, "<init>", &super_descriptor);
        ctor.invokespecial(super_init, aw, 0);
        ctor.line_marks()[marks_before..]
            .iter()
            .map(|&(pc, line)| (pc, u32::from(line)))
            .collect()
    }
}

fn super_ctor_jvm_tys(ir: &IrFile, c: &IrClass, superclass: &str) -> (Vec<Ty>, bool) {
    let mut params = jvm_tys(&capture_order::physical_for(
        ir,
        c.superclass,
        c.super_ctor_params.clone(),
    ));
    let uses_accessor = (c.super_ctor.primary() && ir.has_value_param_ctor(superclass))
        || constructor_accessors::reached_through_accessor(
            c.super_ctor,
            Some(c.fq_name_id()),
            c.superclass,
        );
    if uses_accessor {
        params.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
    }
    (params, uses_accessor)
}

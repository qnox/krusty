//! JVM realization of checked string-concatenation parts.
//!
//! This boundary selects `String.valueOf`, `StringBuilder`, or `invokedynamic` shapes and adapts
//! semantic values to the physical reference slots those shapes consume.

use super::*;

impl Emitter<'_> {
    pub(super) fn emit_string_concat(
        &mut self,
        concat: u32,
        parts: &[u32],
        code: &mut CodeBuilder,
    ) {
        if parts.len() == 1 {
            let part = parts[0];
            if matches!(self.ir.expr(part), IrExpr::Const(IrConst::String(_))) {
                // A lone string constant is already a `String`.
                self.emit_value(part, code);
                return;
            }

            // A single interpolation `"$x"` → `String.valueOf(x)` (kotlinc's form). A `Unit`
            // value instead materializes `Unit.INSTANCE` and calls its `toString`.
            let ty = self.value_ty(part);
            if ty == Ty::Unit {
                self.emit_value_as(part, Ty::obj("kotlin/Unit"), code);
                let method = self
                    .cw
                    .methodref("kotlin/Unit", "toString", "()Ljava/lang/String;");
                code.invokevirtual(method, 0, 1);
            } else {
                self.emit_value(part, code);
                let method = self
                    .cw
                    .methodref("java/lang/String", "valueOf", valueof_desc(ty));
                code.invokestatic(method, slot_words(ty) as i32, 1);
            }
            return;
        }

        if self.try_emit_indy_concat(parts, code) {
            return;
        }

        let builder = self.cw.class_ref("java/lang/StringBuilder");
        let constructor = self
            .cw
            .methodref("java/lang/StringBuilder", "<init>", "()V");
        // A part that cannot carry the operand stack (`"${try {…} finally {}}"`) is spilled with
        // every other part to a temp first, then the builder is built.
        if parts.iter().any(|&part| self.spills_operand_prefix(part)) {
            let temps = parts
                .iter()
                .map(|&part| self.spill_string_part(part, code))
                .collect::<Vec<_>>();
            // Spilling the parts leaves the last part's line in effect. The builder is the
            // concatenation, so it opens on the concatenation's line.
            self.mark_expression_start(concat, code);
            code.new_obj(builder);
            code.dup();
            code.invokespecial(constructor, 0, 0);
            for &(slot, ty, _) in &temps {
                load(ty, slot, code);
                self.mark_expression_start(concat, code);
                self.append_top(ty, code);
            }
            self.release_operand_spills(&temps);
        } else {
            code.new_obj(builder);
            code.dup();
            code.invokespecial(constructor, 0, 0);
            for &part in parts {
                self.append_part(concat, part, code);
            }
        }
        let to_string = self.cw.methodref(
            "java/lang/StringBuilder",
            "toString",
            "()Ljava/lang/String;",
        );
        code.invokevirtual(to_string, 0, 1);
    }

    /// Evaluate one concatenation part into a spill slot. A `Unit` part leaves nothing, so the
    /// slot holds `kotlin/Unit.INSTANCE`, the value kotlinc appends.
    fn spill_string_part(
        &mut self,
        part: u32,
        code: &mut CodeBuilder,
    ) -> (u16, Ty, backend_temporaries::TemporaryLease) {
        let ty = if self.value_ty(part) == Ty::Unit {
            let unit = Ty::obj("kotlin/Unit");
            self.emit_value_as(part, unit, code);
            unit
        } else {
            self.emit_value(part, code);
            self.value_ty(part)
        };
        self.spill_operand(part, ty, code)
    }

    fn append(&mut self, concat: u32, expression: u32, code: &mut CodeBuilder) {
        let ty = self.value_ty(expression);
        let semantic = self
            .ir
            .logical_types
            .get(&expression)
            .copied()
            .unwrap_or(ty);
        if ty == Ty::Unit {
            let unit = Ty::obj("kotlin/Unit");
            self.emit_value_as(expression, unit, code);
            self.mark_expression_start(concat, code);
            self.append_top(unit, code);
            return;
        }
        self.emit_value(expression, code);
        // An unsigned operand already rendered by its `toString-impl` is appended at the value
        // class's static type, like any other value class.
        if matches!(
            self.ir.expr(expression),
            IrExpr::Call {
                callee: Callee::Intrinsic {
                    operation: crate::ir::IrIntrinsic::UnsignedToString { .. },
                    ..
                },
                ..
            }
        ) {
            self.mark_expression_start(concat, code);
            self.append_top(Ty::obj("java/lang/Object"), code);
            return;
        }
        if !semantic.is_nullable() {
            if let Some((owner, carrier)) = native_unsigned_impl_target(semantic) {
                let descriptor = method_descriptor(&[carrier], Ty::String);
                let method = self
                    .cw
                    .methodref(&owner.render(), "toString-impl", &descriptor);
                code.invokestatic(method, slot_words(carrier) as i32, 1);
                self.mark_expression_start(concat, code);
                self.append_top(Ty::String, code);
                return;
            }
            // A value class rendered through its `toString-impl` is appended at its own static type,
            // which selects `append(Object)` as kotlinc does, although the rendered text is a String.
            if self.is_value_class_ty(&semantic) {
                self.mark_expression_start(concat, code);
                self.append_top(Ty::obj("java/lang/Object"), code);
                return;
            }
        }
        self.mark_expression_start(concat, code);
        self.append_top(ty, code);
    }

    /// kotlinc compiles a multi-part string template (and a synthesized `toString`) to a single
    /// `invokedynamic makeConcatWithConstants` when targeting JVM 9+ — a `StringConcatFactory`
    /// bootstrap with a recipe string, `` marking each dynamic argument and literal text inline.
    /// Below JVM 9 (and for the branchy-operand shape, whose frame handling this doesn't model yet)
    /// returns `false` so the caller keeps the `StringBuilder` form.
    fn try_emit_indy_concat(&mut self, parts: &[u32], code: &mut CodeBuilder) -> bool {
        const TAG_ARG: char = '\u{1}';
        const TAG_CONST: char = '\u{2}';
        // JVM 9 = major 53; kotlinc's `-Xstring-concat` default flips to `indy-with-constants` there.
        if self.cw.major() < 53 {
            return false;
        }
        // A branchy part records a merge frame mid-build; matching kotlinc's operand-stack shape across
        // that is the same open problem as elsewhere, so leave those on the StringBuilder path.
        if parts.iter().any(|&part| self.emits_control_flow(part)) {
            return false;
        }
        // A `Unit` part is `Unit.INSTANCE` appended as an object. kotlinc builds that with
        // `StringBuilder`, not an `invokedynamic` whose argument descriptor would be `V`.
        if parts.iter().any(|&part| self.value_ty(part) == Ty::Unit) {
            return false;
        }
        if parts.iter().any(|part| {
            self.ir
                .logical_types
                .get(part)
                .is_some_and(|ty| !ty.is_nullable() && ty.is_unsigned())
        }) {
            return false;
        }
        // The recipe is itself a string CONSTANT, so it carries whatever code units the literal
        // parts hold — including an unpaired surrogate, which no Rust `String` can spell.
        let mut recipe = KtStringBuf::new();
        let mut arg_parts: Vec<u32> = Vec::new();
        for &part in parts {
            if let IrExpr::Const(IrConst::String(string)) = self.ir.expr(part) {
                // A literal carrying a recipe tag would have to move to the constants array — rare;
                // fall back rather than encode it wrong.
                if string
                    .units()
                    .any(|unit| unit == TAG_ARG as u16 || unit == TAG_CONST as u16)
                {
                    return false;
                }
                recipe.push_kt(string);
            } else {
                recipe.push(TAG_ARG);
                arg_parts.push(part);
            }
        }
        let recipe = recipe.finish();
        let arg_descs: String = arg_parts
            .iter()
            .map(|&part| type_descriptor(self.value_ty(part)))
            .collect();
        // kotlinc interns constants in instruction order: the operands' entries first, then the
        // call site's, with the recipe (the bootstrap's static argument) before the bootstrap
        // method handle.
        let mut arg_words = 0i32;
        for &part in &arg_parts {
            let ty = self.value_ty(part);
            self.emit_value(part, code);
            arg_words += slot_words(ty) as i32;
        }
        let recipe_const = self.cw.const_string_kt(&recipe);
        let method_handle = self.cw.method_handle_static(
            "java/lang/invoke/StringConcatFactory",
            "makeConcatWithConstants",
            "(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;\
             Ljava/lang/String;[Ljava/lang/Object;)Ljava/lang/invoke/CallSite;",
        );
        let bootstrap = self.cw.add_bootstrap(method_handle, vec![recipe_const]);
        let invocation = self.cw.invoke_dynamic(
            bootstrap,
            "makeConcatWithConstants",
            &format!("({arg_descs})Ljava/lang/String;"),
        );
        code.invokedynamic(invocation, arg_words, 1);
        true
    }

    /// Append one string-template part to the `StringBuilder` beneath it. A single-character string
    /// constant appends as a `char` (kotlinc emits `append(C)` with the char constant, not `append(String)`).
    fn append_part(&mut self, concat: u32, part: u32, code: &mut CodeBuilder) {
        // "single character" is one UTF-16 code UNIT — the width of a `Char` — so a supplementary
        // character (two units) stays on the `append(String)` path, as it must.
        let single_unit = if let IrExpr::Const(IrConst::String(string)) = self.ir.expr(part) {
            string.single_unit()
        } else {
            None
        };
        if let Some(unit) = single_unit {
            // A one-unit literal does not open its own line. It stays on the concatenation's
            // line, including when the literal itself is written on a later line.
            code.push_int(unit as i32, self.cw);
            self.mark_expression_start(concat, code);
            self.append_top(Ty::Char, code);
        } else {
            self.append(concat, part, code);
        }
    }

    /// Append a value already on the operand stack (of type `ty`) to a `StringBuilder` beneath it.
    fn append_top(&mut self, ty: Ty, code: &mut CodeBuilder) {
        // A `String` value reaches here either as `Ty::String` or as `Ty::Obj("java/lang/String")` —
        // the latter when its type was parsed from a method-return descriptor (e.g. a classpath call
        // or the data-class `Arrays.toString(field)` wrapper). Both must pick the `append(String)`
        // overload kotlinc uses, not the less-specific `append(Object)`.
        let is_string = matches!(ty, Ty::String)
            || matches!(ty, Ty::Obj(name, _) if name == "java/lang/String" || name == "kotlin/String");
        let descriptor = match ty {
            _ if is_string => "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
            Ty::Int | Ty::Short | Ty::Byte => "(I)Ljava/lang/StringBuilder;",
            Ty::Long => "(J)Ljava/lang/StringBuilder;",
            Ty::Boolean => "(Z)Ljava/lang/StringBuilder;",
            Ty::Char => "(C)Ljava/lang/StringBuilder;",
            Ty::Double => "(D)Ljava/lang/StringBuilder;",
            Ty::Float => "(F)Ljava/lang/StringBuilder;",
            _ => "(Ljava/lang/Object;)Ljava/lang/StringBuilder;",
        };
        let method = self
            .cw
            .methodref("java/lang/StringBuilder", "append", descriptor);
        code.invokevirtual(method, slot_words(ty) as i32, 1);
    }
}

/// The `String.valueOf` overload descriptor for a single interpolated value's type (`"$x"`).
fn valueof_desc(ty: Ty) -> &'static str {
    match ty {
        Ty::Int | Ty::Short | Ty::Byte => "(I)Ljava/lang/String;",
        Ty::Long => "(J)Ljava/lang/String;",
        Ty::Float => "(F)Ljava/lang/String;",
        Ty::Double => "(D)Ljava/lang/String;",
        Ty::Boolean => "(Z)Ljava/lang/String;",
        Ty::Char => "(C)Ljava/lang/String;",
        _ => "(Ljava/lang/Object;)Ljava/lang/String;",
    }
}

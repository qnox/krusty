//! JVM realization of checked string-concatenation parts.
//!
//! This boundary selects `String.valueOf`, `StringBuilder`, or `invokedynamic` shapes and adapts
//! semantic values to the physical reference slots those shapes consume.

use super::*;
use krusty::kt_string::KtString;
use krusty::types::TyKind;

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
                // A reference is passed to `valueOf(Object)`, which a smart cast reaches uncast.
                self.emit_template_part(part, code);
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
            // The builder is the concatenation, so it opens on the concatenation's line. An
            // `invokedynamic` concatenation has no constructor and leaves the line to its operands.
            self.mark_expression_start(concat, code);
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
        let semantic = self.ir.logical_type(expression).unwrap_or(ty);
        if ty == Ty::Unit {
            let unit = Ty::obj("kotlin/Unit");
            self.emit_value_as(expression, unit, code);
            self.mark_expression_start(concat, code);
            self.append_top(unit, code);
            return;
        }
        let source = self.emit_template_part(expression, code);
        // An unsigned operand already rendered by its `toString-impl` is appended at the value
        // class's static type, like any other value class.
        if matches!(
            self.ir.expr(expression),
            IrExpr::Call {
                callee: call_callee,
                ..
            } if matches!(&**call_callee, Callee::Intrinsic {
                    operation: krusty::ir::IrIntrinsic::UnsignedToString { .. },
                    ..
                })
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
                // Appended at the unsigned type's static type, as kotlinc does.
                self.append_top(Ty::obj("java/lang/Object"), code);
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
        // A smart cast is materialized at the `append` overload its type selects: `String`, or
        // `Object`, which takes it uncast.
        if source != ty {
            self.coerce_reference_on_stack(source, string_builder_slot(ty), code);
        }
        self.mark_expression_start(concat, code);
        self.append_top(ty, code);
    }

    /// Emit a template part for the slot that consumes it. A smart cast is kotlinc's
    /// `IMPLICIT_CAST`, and a generic result keeps its declaration's erased slot, so either part
    /// reaches that slot unnarrowed and the slot casts it if it must: `append(String)` does,
    /// `valueOf(Object)`, `append(Object)` and an `invokedynamic` argument do not. Answers the
    /// stack type left.
    fn emit_template_part(&mut self, part: u32, code: &mut CodeBuilder) -> Ty {
        if let Some(operand) = self.consumed_implicit_cast_operand(part) {
            return self.emit_consumed_operand(operand, code);
        }
        if self.erased_reference_result(part).is_some() {
            return self.emit_consumed_operand(part, code);
        }
        self.emit_value(part, code);
        self.value_ty(part)
    }

    /// Emit one `invokedynamic` argument, answering the type the recipe takes it at. The bootstrap
    /// takes a reference argument in the slot that produced it: a suspension point's result stays
    /// the erased `Object` the call returns, and a captured `var` its `ObjectRef.element`. The
    /// `StringBuilder` form instead narrows both to the `append` overload of the declared type.
    fn emit_indy_concat_operand(&mut self, part: u32, code: &mut CodeBuilder) -> Ty {
        let ty = self.value_ty(part);
        let object = Ty::obj("java/lang/Object");
        if !ty.is_reference() {
            self.emit_value(part, code);
            return ty;
        }
        if self.emit_erased_suspension_result(part, object, code) {
            return object;
        }
        match *self.ir.expr(part) {
            IrExpr::RefGet { holder, elem } if !self.is_value_class_ty(&elem) => {
                self.emit_shared_cell_read(holder, elem, false, code)
            }
            _ => self.emit_template_part(part, code),
        }
    }

    /// kotlinc compiles a multi-part string template (and a synthesized `toString`) to
    /// `invokedynamic makeConcatWithConstants` when targeting JVM 9+ — a `StringConcatFactory`
    /// bootstrap with a recipe string, `\u{1}` marking each dynamic argument and literal text
    /// inline — or, under `-Xstring-concat=indy`, to `makeConcat` with every part an argument.
    /// Below JVM 9 (and for a part that spills the operand stack, or a recipe that would overflow a
    /// constant) returns `false` so the caller keeps the `StringBuilder` form.
    fn try_emit_indy_concat(&mut self, parts: &[u32], code: &mut CodeBuilder) -> bool {
        const TAG_ARG: char = '\u{1}';
        const TAG_CONST: char = '\u{2}';
        // The bootstrap takes at most 200 argument slots (`long` and `double` take two).
        const MAX_ARGUMENT_SLOTS: i32 = 200;
        // JVM 9 = major 53; kotlinc's `-Xstring-concat` default flips to `indy-with-constants` there,
        // unless the invocation selects `inline`.
        let mode = self.string_concat;
        if self.cw.major() < 53 || mode == StringConcatMode::Inline {
            return false;
        }
        let with_constants = mode == StringConcatMode::IndyWithConstants;
        // A branchy part (`v == null ? "null" : …`) merges with the earlier arguments on the
        // stack, as kotlinc's does. One that cannot carry the stack at all (a `try`, a branching
        // spliced inline) is spilled first, which only the `StringBuilder` form models.
        if parts.iter().any(|&part| self.spills_operand_prefix(part)) {
            return false;
        }
        // A `Unit` part is `Unit.INSTANCE` appended as an object. kotlinc builds that with
        // `StringBuilder`, not an `invokedynamic` whose argument descriptor would be `V`.
        if parts.iter().any(|&part| self.value_ty(part) == Ty::Unit) {
            return false;
        }
        if parts.iter().any(|part| {
            self.ir
                .logical_type(*part)
                .is_some_and(|ty| !ty.is_nullable() && ty.is_unsigned())
        }) {
            return false;
        }
        // The recipe is itself a string CONSTANT, so it carries whatever code units the literal
        // parts hold — including an unpaired surrogate, which no Rust `String` can spell. A literal
        // carrying a recipe tag would have to move to the constants array — rare; fall back rather
        // than encode it wrong. Literal text too long for one constant keeps the `StringBuilder`
        // form, whose operands are separate literals, as kotlinc's does (KT-47917).
        let mut recipe_bytes = 0usize;
        for &part in parts {
            if let IrExpr::Const(IrConst::String(string)) = self.ir.expr(part) {
                if with_constants {
                    if string
                        .units()
                        .any(|unit| unit == TAG_ARG as u16 || unit == TAG_CONST as u16)
                    {
                        return false;
                    }
                    recipe_bytes += string.units().map(modified_utf8_len).sum::<usize>();
                }
            } else {
                recipe_bytes += 1;
            }
        }
        if recipe_bytes > usize::from(u16::MAX) {
            return false;
        }
        // kotlinc interns constants in instruction order: the operands' entries first, then the
        // call site's, with the recipe (the bootstrap's static argument) before the bootstrap
        // method handle. A template of more than 200 argument slots is built in chunks, each
        // chunk's string leading the next.
        let mut recipe = KtStringBuf::new();
        let mut arg_descs = String::new();
        let mut arg_words = 0i32;
        for &part in parts {
            if with_constants {
                if let IrExpr::Const(IrConst::String(string)) = self.ir.expr(part) {
                    recipe.push_kt(string);
                    continue;
                }
            }
            let words = slot_words(self.value_ty(part)) as i32;
            if arg_words > 0 && arg_words + words > MAX_ARGUMENT_SLOTS {
                self.emit_indy_concat_call(mode, &recipe.finish(), &arg_descs, arg_words, code);
                recipe = KtStringBuf::new();
                recipe.push(TAG_ARG);
                arg_descs = "Ljava/lang/String;".to_string();
                arg_words = 1;
            }
            let ty = self.emit_indy_concat_operand(part, code);
            recipe.push(TAG_ARG);
            arg_descs.push_str(&type_descriptor(ty));
            arg_words += slot_words(ty) as i32;
        }
        self.emit_indy_concat_call(mode, &recipe.finish(), &arg_descs, arg_words, code);
        true
    }

    /// One `invokedynamic` string concatenation over the `arg_words` slots of arguments already on
    /// the stack, typed `arg_descs`.
    fn emit_indy_concat_call(
        &mut self,
        mode: StringConcatMode,
        recipe: &KtString,
        arg_descs: &str,
        arg_words: i32,
        code: &mut CodeBuilder,
    ) {
        let descriptor = format!("({arg_descs})Ljava/lang/String;");
        let (name, bootstrap) = if mode == StringConcatMode::IndyWithConstants {
            let recipe_const = self.cw.const_string_kt(recipe);
            let method_handle = self.cw.method_handle_static(
                "java/lang/invoke/StringConcatFactory",
                "makeConcatWithConstants",
                "(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;\
                 Ljava/lang/String;[Ljava/lang/Object;)Ljava/lang/invoke/CallSite;",
            );
            (
                "makeConcatWithConstants",
                self.cw.add_bootstrap(method_handle, vec![recipe_const]),
            )
        } else {
            let method_handle = self.cw.method_handle_static(
                "java/lang/invoke/StringConcatFactory",
                "makeConcat",
                "(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;)\
                 Ljava/lang/invoke/CallSite;",
            );
            (
                "makeConcat",
                self.cw.add_bootstrap(method_handle, Vec::new()),
            )
        };
        let invocation = self.cw.invoke_dynamic(bootstrap, name, &descriptor);
        code.invokedynamic(invocation, arg_words, 1);
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
        let descriptor = match ty {
            _ if is_string(ty) => "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
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

/// The parameter type of the `StringBuilder.append` overload [`Emitter::append_top`] selects for a
/// reference of type `ty`.
fn string_builder_slot(ty: Ty) -> Ty {
    if is_string(ty) {
        Ty::String
    } else {
        Ty::obj("java/lang/Object")
    }
}

fn is_string(ty: Ty) -> bool {
    matches!(ty, Ty::String)
        || matches!(ty.kind(), TyKind::Obj(name, _) if name == "java/lang/String" || name == "kotlin/String")
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

/// The bytes one UTF-16 code unit takes in a class file's modified UTF-8.
fn modified_utf8_len(unit: u16) -> usize {
    match unit {
        1..=0x7f => 1,
        0 | 0x80..=0x7ff => 2,
        _ => 3,
    }
}

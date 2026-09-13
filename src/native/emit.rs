//! Common IR to C.
//!
//! C is the target because it is the shortest path to a *genuinely native* binary that does not
//! commit the project to a code generator. `docs/BUILD_AND_NATIVE_PLAN.md` keeps the Cranelift
//! question open on purpose, and picking either Cranelift or LLVM to print `Hello, world!` would
//! answer it by accident. Emitting C answers nothing: the same lowering feeds a real backend later,
//! and in the meantime `cc` is already on every machine that builds this compiler.
//!
//! **What this emitter refuses.** Anything it has not been taught produces a diagnostic naming the
//! construct, and the file emits nothing. It never falls back to a guess. The JVM backend can
//! afford a best effort because `kotlinc` decides what is correct; nothing decides that here yet,
//! so a wrong emission would be indistinguishable from a right one until someone ran it.
//!
//! One GNU C extension is used: the statement expression `({ …; value; })`, which is how a Kotlin
//! block with a value renders in expression position. gcc and clang both support it; a portable
//! spelling would mean hoisting temporaries through a lowering pass, which is real work that
//! belongs with the rest of the native lowering rather than in the first emitter.

use std::collections::HashMap;

use crate::ir::{Callee, IrBinOp, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::jvm::classpath::Classpath;
use crate::types::Ty;

/// How a Kotlin type is carried in C.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CKind {
    /// `void` — Kotlin `Unit` in return position.
    Void,
    /// A machine scalar, spelled as one of the runtime's `kt_*` typedefs.
    Scalar(&'static str),
    /// A `KRef`: every reference, including a boxed `Int?`.
    Ref,
}

impl CKind {
    fn spelling(self) -> &'static str {
        match self {
            Self::Void => "void",
            Self::Scalar(name) => name,
            Self::Ref => "KRef",
        }
    }
}

/// The C carrier for a Kotlin type. A nullable primitive is deliberately NOT a scalar: `Int?` has
/// to represent `null`, so it boxes, exactly as it does on the JVM.
fn c_kind(ty: Ty) -> CKind {
    match ty {
        Ty::Unit => CKind::Void,
        Ty::Boolean => CKind::Scalar("kt_boolean"),
        Ty::Byte => CKind::Scalar("kt_byte"),
        Ty::Short => CKind::Scalar("kt_short"),
        Ty::Int => CKind::Scalar("kt_int"),
        Ty::Long => CKind::Scalar("kt_long"),
        Ty::Char => CKind::Scalar("kt_char"),
        Ty::Float => CKind::Scalar("kt_float"),
        Ty::Double => CKind::Scalar("kt_double"),
        _ => CKind::Ref,
    }
}

/// The runtime's suffix for a scalar carrier, used to name its `kt_*` helpers.
fn scalar_suffix(kind: CKind) -> Option<&'static str> {
    match kind {
        CKind::Scalar(name) => name.strip_prefix("kt_"),
        _ => None,
    }
}

/// The runtime's boxing/unboxing suffix for a scalar carrier.
///
/// `None` for `Float`/`Double`: the runtime has no box for them ON PURPOSE, because it cannot
/// render one and Kotlin's `Double.toString` is not `%g` (see `super::runtime`). Returning a
/// suffix here would emit a call to a function that does not exist, turning a construct the
/// backend should DECLINE into a link error with no diagnostic.
fn box_suffix(kind: CKind) -> Option<&'static str> {
    match scalar_suffix(kind) {
        Some("float" | "double") | None => None,
        suffix => suffix,
    }
}

/// Render a Kotlin string as a C string literal plus its UTF-8 byte length.
///
/// `None` for a string containing an unpaired surrogate. Kotlin admits those (`"\uD800"`), UTF-8
/// does not encode them, and the runtime is UTF-8 — so this is declined rather than mangled.
fn c_string_literal(value: &crate::kt_string::KtString) -> Option<(String, usize)> {
    let text = value.as_str()?;
    let mut literal = String::with_capacity(text.len() + 2);
    literal.push('"');
    for byte in text.bytes() {
        match byte {
            b'"' => literal.push_str("\\\""),
            b'\\' => literal.push_str("\\\\"),
            b'\n' => literal.push_str("\\n"),
            b'\r' => literal.push_str("\\r"),
            b'\t' => literal.push_str("\\t"),
            // Everything else goes out as an octal escape, including plain ASCII letters' UTF-8
            // continuation bytes. Octal (not hex) because a hex escape in C has no length limit:
            // `"\x41" "1"` is fine but `"\x411"` is one huge character.
            0x20..=0x7E => literal.push(byte as char),
            other => literal.push_str(&format!("\\{other:03o}")),
        }
    }
    literal.push('"');
    Some((literal, text.len()))
}

/// Sanitize a Kotlin name into something C will accept as an identifier.
fn c_identifier(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character);
        } else {
            out.push('_');
        }
    }
    out
}

/// A loop the emitter is currently inside.
struct LoopFrame {
    /// The Kotlin label on the loop, if it carried one.
    label: Option<String>,
    /// The unique C label base for this loop's `goto` targets.
    base: String,
    /// Whether `continue` must be a `goto` because the loop has an update to run first.
    continue_is_goto: bool,
}

/// Which way out of a loop a jump takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Exit {
    Break,
    Continue,
}

impl std::fmt::Display for Exit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Break => "break",
            Self::Continue => "continue",
        })
    }
}

/// One emitted C translation unit, plus the entry point it declares (if any).
pub(super) struct CTranslationUnit {
    pub source: String,
    /// The C symbol of this file's Kotlin `main`, when it declares one.
    pub entry: Option<String>,
}

pub(super) struct Emitter<'a> {
    ir: &'a IrFile,
    classpath: &'a Classpath,
    /// C symbol per IR function index.
    symbols: Vec<String>,
    /// Declared type of each value slot in the function being emitted.
    values: HashMap<u32, Ty>,
    /// The C carrier of the enclosing function's result. A `return` has to know it, and a `return`
    /// can appear arbitrarily deep — including inside a block that is itself in EXPRESSION position
    /// (`val x = if (c) return else 1`), where there is no enclosing statement to thread it from.
    result: CKind,
    /// Loops enclosing the statement being emitted, innermost last.
    loops: Vec<LoopFrame>,
    label_count: u32,
    out: String,
}

/// The construct an emission declined, phrased for a diagnostic.
pub(super) type Unsupported = String;

impl<'a> Emitter<'a> {
    pub(super) fn new(ir: &'a IrFile, classpath: &'a Classpath) -> Self {
        Self {
            symbols: function_symbols(ir),
            ir,
            classpath,
            values: HashMap::new(),
            result: CKind::Void,
            loops: Vec::new(),
            label_count: 0,
            out: String::new(),
        }
    }

    pub(super) fn emit_file(mut self) -> Result<CTranslationUnit, Unsupported> {
        if !self.ir.classes.is_empty() {
            return Err("a class declaration".to_string());
        }
        if !self.ir.statics.is_empty() {
            return Err("a top-level property".to_string());
        }

        self.out.push_str("/* Generated by krusty. */\n");
        self.out.push_str("#include \"krusty_rt.h\"\n\n");

        // Forward declarations first, so order of definition never decides what resolves.
        for (index, function) in self.ir.functions.iter().enumerate() {
            self.out
                .push_str(&format!("{};\n", self.signature(index, function)?));
        }
        self.out.push('\n');

        let mut entry = None;
        for index in 0..self.ir.functions.len() {
            let function = &self.ir.functions[index];
            let Some(body) = function.body else {
                return Err(format!("a body-less function `{}`", function.name));
            };
            if function.dispatch_receiver.is_some() {
                return Err(format!("an instance method `{}`", function.name));
            }
            if function.name == "main" && function.params.is_empty() {
                entry = Some(self.symbols[index].clone());
            }

            self.values = HashMap::new();
            self.loops.clear();
            for (slot, ty) in function.params.iter().enumerate() {
                self.values.insert(slot as u32, *ty);
            }
            self.collect_variable_types(body);

            let signature = self.signature(index, function)?;
            self.out.push_str(&format!("{signature} {{\n"));
            self.result = c_kind(function.ret);
            self.statement(body, 1)?;
            self.out.push_str("}\n\n");
        }

        Ok(CTranslationUnit {
            source: self.out,
            entry,
        })
    }

    fn signature(
        &self,
        index: usize,
        function: &crate::ir::IrFunction,
    ) -> Result<String, Unsupported> {
        let mut parameters = Vec::with_capacity(function.params.len());
        for (slot, ty) in function.params.iter().enumerate() {
            match c_kind(*ty) {
                CKind::Void => return Err(format!("a `Unit` parameter of `{}`", function.name)),
                kind => parameters.push(format!("{} v{slot}", kind.spelling())),
            }
        }
        let parameters = if parameters.is_empty() {
            "void".to_string()
        } else {
            parameters.join(", ")
        };
        Ok(format!(
            "{} {}({parameters})",
            c_kind(function.ret).spelling(),
            self.symbols[index]
        ))
    }

    /// Record the declared type of every local the function introduces.
    fn collect_variable_types(&mut self, root: u32) {
        let mut pending = vec![root];
        let mut seen = std::collections::HashSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let IrExpr::Variable { index, ty, .. } = self.ir.expr(id) {
                self.values.insert(*index, *ty);
            }
            crate::ir::for_each_child(&self.ir.exprs, id, &mut |child| pending.push(child));
        }
    }

    // ---- types -------------------------------------------------------------------------------

    /// The Kotlin type an expression produces, as far as the emitter needs it: enough to decide
    /// between a scalar and a reference carrier. `None` means "not determined", and every caller
    /// that cannot proceed without it declines.
    fn ty_of(&self, id: u32) -> Option<Ty> {
        Some(match self.ir.expr(id) {
            IrExpr::Const(constant) => match constant {
                IrConst::Boolean(_) => Ty::Boolean,
                IrConst::Byte(_) => Ty::Byte,
                IrConst::Short(_) => Ty::Short,
                IrConst::Int(_) => Ty::Int,
                IrConst::Long(_) => Ty::Long,
                IrConst::Float(_) => Ty::Float,
                IrConst::Double(_) => Ty::Double,
                IrConst::Char(_) => Ty::Char,
                IrConst::String(_) => Ty::String,
                IrConst::Null => Ty::Null,
            },
            IrExpr::UnitInstance => Ty::obj("kotlin/Unit"),
            IrExpr::GetValue(slot) => *self.values.get(slot)?,
            IrExpr::TypeOp {
                op, type_operand, ..
            } => match op {
                IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf => Ty::Boolean,
                _ => *type_operand,
            },
            IrExpr::StringConcat(_) => Ty::String,
            IrExpr::PrimitiveNeg { ty, .. } => *ty,
            IrExpr::PrimitiveBinOp { op, lhs, .. } => match op {
                IrBinOp::Lt
                | IrBinOp::Le
                | IrBinOp::Gt
                | IrBinOp::Ge
                | IrBinOp::Eq
                | IrBinOp::Ne
                | IrBinOp::RefEq
                | IrBinOp::RefNe
                | IrBinOp::And
                | IrBinOp::Or => Ty::Boolean,
                // Kotlin has no `Byte.plus(Byte): Byte`: arithmetic on the narrow integer types
                // produces `Int`, and a result typed `Byte` here would pick the wrong carrier.
                _ => match self.ty_of(*lhs)? {
                    Ty::Byte | Ty::Short | Ty::Char => Ty::Int,
                    other => other,
                },
            },
            IrExpr::Block { value, .. } => match value {
                Some(value) => self.ty_of(*value)?,
                None => Ty::Unit,
            },
            IrExpr::When { branches } => self.ty_of(branches.first()?.1)?,
            IrExpr::Call { callee, .. } => self.callee_result(callee)?,
            _ => return None,
        })
    }

    fn callee_result(&self, callee: &Callee) -> Option<Ty> {
        Some(match callee {
            Callee::Local(function) => self.ir.functions[*function as usize].ret,
            Callee::External { ret, .. } | Callee::Intrinsic { ret, .. } => *ret,
            _ => return None,
        })
    }

    // ---- statements --------------------------------------------------------------------------

    fn statement(&mut self, id: u32, depth: usize) -> Result<(), Unsupported> {
        match self.ir.expr(id).clone() {
            IrExpr::Block { stmts, value } => {
                for statement in stmts {
                    self.statement(statement, depth)?;
                }
                if let Some(value) = value {
                    self.statement(value, depth)?;
                }
            }
            IrExpr::Return(value) => {
                let rendered = match value {
                    // A `return e` whose `e` is `Unit`-typed still has to EVALUATE `e`.
                    Some(value) if self.result == CKind::Void => {
                        let expression = self.expression(value)?;
                        format!("{expression};\n{}return;", indent_of(depth))
                    }
                    Some(value) => format!("return {};", self.expression(value)?),
                    None => "return;".to_string(),
                };
                self.line(depth, &rendered);
            }
            IrExpr::Variable {
                index, ty, init, ..
            } => {
                self.values.insert(index, ty);
                let kind = c_kind(ty);
                if kind == CKind::Void {
                    // A `Unit` local holds nothing, but its initializer is still program text that
                    // runs. Emit the initializer and drop the (absent) value.
                    if let Some(init) = init {
                        let expression = self.expression(init)?;
                        self.line(depth, &format!("{expression};"));
                    }
                } else {
                    let declaration = match init {
                        Some(init) => {
                            format!("{} v{index} = {};", kind.spelling(), self.expression(init)?)
                        }
                        None => format!("{} v{index};", kind.spelling()),
                    };
                    self.line(depth, &declaration);
                }
            }
            IrExpr::SetValue { var, value } => {
                let value = self.expression(value)?;
                self.line(depth, &format!("v{var} = {value};"));
            }
            IrExpr::When { branches } => {
                let mut first = true;
                for (condition, body) in branches {
                    match condition {
                        Some(condition) => {
                            let condition = self.expression(condition)?;
                            let keyword = if first { "if" } else { "else if" };
                            self.line(depth, &format!("{keyword} ({condition}) {{"));
                        }
                        None => self.line(depth, "else {"),
                    }
                    self.statement(body, depth + 1)?;
                    self.line(depth, "}");
                    first = false;
                }
            }
            IrExpr::While {
                cond,
                body,
                update,
                post_test,
                label,
            } => self.loop_statement(cond, body, update, post_test, label, depth)?,
            IrExpr::Break { label } => {
                let jump = self.jump(label.as_deref(), Exit::Break)?;
                self.line(depth, &jump);
            }
            IrExpr::Continue { label } => {
                let jump = self.jump(label.as_deref(), Exit::Continue)?;
                self.line(depth, &jump);
            }
            _ => {
                let expression = self.expression(id)?;
                self.line(depth, &format!("{expression};"));
            }
        }
        Ok(())
    }

    /// Emit a `while`/`do…while`, with the `goto` scaffolding C needs for anything a plain
    /// `break`/`continue` cannot express.
    ///
    /// Two things force `goto`. A Kotlin label (`break@outer`) has no C equivalent at all. And a
    /// loop whose `update` is a STATEMENT SEQUENCE — which is what a lowered `for` produces, since
    /// the step carries its own overflow guard — cannot put that update in a `for` header, so the
    /// update lands at the end of the body and every `continue`, labeled or not, has to jump to a
    /// point before it or the loop would not advance.
    fn loop_statement(
        &mut self,
        cond: u32,
        body: u32,
        update: Option<u32>,
        post_test: bool,
        label: Option<String>,
        depth: usize,
    ) -> Result<(), Unsupported> {
        self.label_count += 1;
        let base = format!(
            "kt_loop{}{}",
            self.label_count,
            label
                .as_deref()
                .map(c_identifier)
                .map_or(String::new(), |l| format!("_{l}"))
        );
        self.loops.push(LoopFrame {
            label,
            base: base.clone(),
            // With an update to run, `continue` must reach it; C's own `continue` would skip it.
            continue_is_goto: update.is_some(),
        });

        // The body is emitted into a buffer first so the `goto` targets can be omitted unless the
        // body actually jumped to them; an emitted-but-unused C label is dead text in the output.
        let saved = std::mem::take(&mut self.out);
        self.statement(body, depth + 1)?;
        let rendered_body = std::mem::replace(&mut self.out, saved);
        let rendered_update = match update {
            Some(update) => {
                let saved = std::mem::take(&mut self.out);
                self.statement(update, depth + 1)?;
                Some(std::mem::replace(&mut self.out, saved))
            }
            None => None,
        };
        self.loops.pop();

        // The update is scanned too: a lowered `for` puts its overflow guard's `break` there, and
        // a label emitted only for jumps found in the body would leave that one undefined.
        let jumped_to = |target: &str| {
            let jump = format!("goto {base}_{target};");
            rendered_body.contains(&jump)
                || rendered_update
                    .as_deref()
                    .is_some_and(|update| update.contains(&jump))
        };
        let jumped_to_continue = jumped_to("continue");
        let jumped_to_break = jumped_to("break");

        let condition = self.expression(cond)?;
        if post_test {
            self.line(depth, "do {");
        } else {
            self.line(depth, &format!("while ({condition}) {{"));
        }
        self.out.push_str(&rendered_body);
        if jumped_to_continue {
            self.line(depth + 1, &format!("{base}_continue: ;"));
        }
        if let Some(update) = rendered_update {
            self.out.push_str(&update);
        }
        if post_test {
            self.line(depth, &format!("}} while ({condition});"));
        } else {
            self.line(depth, "}");
        }
        if jumped_to_break {
            self.line(depth, &format!("{base}_break: ;"));
        }
        Ok(())
    }

    /// The C statement realizing a `break`/`continue`, labeled or not.
    fn jump(&self, label: Option<&str>, exit: Exit) -> Result<String, Unsupported> {
        let frame = match label {
            Some(label) => self
                .loops
                .iter()
                .rev()
                .find(|frame| frame.label.as_deref() == Some(label)),
            None => self.loops.last(),
        };
        let Some(frame) = frame else {
            return Err(match label {
                Some(label) => format!("`{exit}@{label}`, whose loop is not in scope"),
                None => format!("`{exit}` outside a loop"),
            });
        };
        let needs_goto = label.is_some() || (exit == Exit::Continue && frame.continue_is_goto);
        Ok(if needs_goto {
            format!("goto {}_{exit};", frame.base)
        } else {
            format!("{exit};")
        })
    }

    fn line(&mut self, depth: usize, text: &str) {
        self.out.push_str(&indent_of(depth));
        self.out.push_str(text);
        self.out.push('\n');
    }

    // ---- expressions -------------------------------------------------------------------------

    fn expression(&mut self, id: u32) -> Result<String, Unsupported> {
        match self.ir.expr(id).clone() {
            IrExpr::Const(constant) => constant_expression(&constant),
            IrExpr::UnitInstance => Ok("kt_unit()".to_string()),
            IrExpr::GetValue(slot) => Ok(format!("v{slot}")),
            IrExpr::PrimitiveNeg { operand, ty } => {
                let rendered = self.expression(operand)?;
                // `-Int.MIN_VALUE` is `Int.MIN_VALUE` in Kotlin and undefined in C.
                match scalar_suffix(c_kind(ty)) {
                    Some(width @ ("int" | "long")) => {
                        let unsigned = if width == "int" {
                            "uint32_t"
                        } else {
                            "uint64_t"
                        };
                        Ok(format!("((kt_{width})(0u - ({unsigned})({rendered})))"))
                    }
                    _ => Ok(format!("(-{rendered})")),
                }
            }
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => self.binary(op, lhs, rhs),
            IrExpr::StringConcat(parts) => {
                let mut rendered = "kt_string_utf8(\"\", 0)".to_string();
                for part in parts {
                    let part = self.reference(part)?;
                    rendered = format!("kt_string_plus({rendered}, {part})");
                }
                Ok(rendered)
            }
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion | IrTypeOp::Cast | IrTypeOp::CastNonNull,
                arg,
                type_operand,
            } => self.coerce(arg, type_operand),
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => self.call(&callee, dispatch_receiver, &args),
            IrExpr::Block { stmts, value } => {
                // A block with a value in expression position becomes a GNU statement expression.
                let Some(value) = value else {
                    return Err("a block with no value in expression position".to_string());
                };
                let saved = std::mem::take(&mut self.out);
                for statement in stmts {
                    self.statement(statement, 0)?;
                }
                let body = std::mem::replace(&mut self.out, saved);
                let value = self.expression(value)?;
                let body = body.replace('\n', " ");
                Ok(format!("({{ {body}{value}; }})"))
            }
            IrExpr::When { branches } => {
                // Right-fold the arms into nested conditionals. An arm that is not an expression
                // (a `break`, a `return`) has already been handled in statement position; reaching
                // one here means the IR wants its value, which a conditional cannot supply.
                let mut arms = branches.into_iter().rev();
                let Some((_, otherwise)) = arms.next() else {
                    return Err("an empty `when` in expression position".to_string());
                };
                let mut rendered = self.expression(otherwise)?;
                for (condition, body) in arms {
                    let Some(condition) = condition else {
                        return Err("a `when` whose `else` is not last".to_string());
                    };
                    rendered = format!(
                        "({} ? {} : {rendered})",
                        self.expression(condition)?,
                        self.expression(body)?
                    );
                }
                Ok(rendered)
            }
            other => Err(describe(&other)),
        }
    }

    /// Realize a built-in binary operator.
    ///
    /// Three families of operator cannot be C's spelling of the same symbol, and each is a silent
    /// wrong answer rather than a compile error if it is emitted naively:
    ///
    /// * `==` on references is Kotlin's STRUCTURAL equality; C's compares addresses.
    /// * `+`, `-`, `*` and unary `-` WRAP on overflow in Kotlin; signed overflow is undefined in C,
    ///   which a compiler is free to assume never happens.
    /// * `/`, `%` and the shifts are defined by Kotlin for operands C leaves undefined.
    fn binary(&mut self, op: IrBinOp, lhs: u32, rhs: u32) -> Result<String, Unsupported> {
        let operand = self.ty_of(lhs).map(c_kind);
        let scalar = operand.and_then(scalar_suffix);

        if matches!(op, IrBinOp::Eq | IrBinOp::Ne) {
            match operand {
                Some(CKind::Ref) => return Err("structural equality on references".to_string()),
                // Neither a known scalar nor a known reference: emitting either equality would be
                // a guess about which one Kotlin means.
                _ if scalar.is_none() => {
                    return Err("an equality on an undetermined operand type".to_string())
                }
                _ => {}
            }
        }

        let left = self.expression(lhs)?;
        let right = self.expression(rhs)?;

        // Wrapping arithmetic, spelled through the unsigned type of the same width: unsigned
        // overflow is defined to wrap in C, which is exactly Kotlin's rule.
        let wrapping = match (op, scalar) {
            (IrBinOp::Add, Some(width @ ("int" | "long"))) => Some((width, "+")),
            (IrBinOp::Sub, Some(width @ ("int" | "long"))) => Some((width, "-")),
            (IrBinOp::Mul, Some(width @ ("int" | "long"))) => Some((width, "*")),
            _ => None,
        };
        if let Some((width, operator)) = wrapping {
            let unsigned = if width == "int" {
                "uint32_t"
            } else {
                "uint64_t"
            };
            return Ok(format!(
                "((kt_{width})(({unsigned})({left}) {operator} ({unsigned})({right})))"
            ));
        }

        let helper = match (op, scalar) {
            (IrBinOp::Div, Some(width @ ("int" | "long"))) => Some(format!("kt_div_{width}")),
            (IrBinOp::Rem, Some(width @ ("int" | "long"))) => Some(format!("kt_rem_{width}")),
            (IrBinOp::Shl, Some(width @ ("int" | "long"))) => Some(format!("kt_shl_{width}")),
            (IrBinOp::Shr, Some(width @ ("int" | "long"))) => Some(format!("kt_shr_{width}")),
            (IrBinOp::Ushr, Some(width @ ("int" | "long"))) => Some(format!("kt_ushr_{width}")),
            // IEEE division and remainder are the same in both languages.
            (IrBinOp::Div | IrBinOp::Rem, Some("float" | "double")) => None,
            (IrBinOp::Div | IrBinOp::Rem | IrBinOp::Shl | IrBinOp::Shr | IrBinOp::Ushr, _) => {
                return Err(format!("`{}` on this operand type", c_operator(op)?))
            }
            _ => None,
        };
        if let Some(helper) = helper {
            return Ok(format!("{helper}({left}, {right})"));
        }

        Ok(format!("({left} {} {right})", c_operator(op)?))
    }

    /// An expression in a position that requires a `KRef`, boxing a scalar if necessary.
    fn reference(&mut self, id: u32) -> Result<String, Unsupported> {
        let rendered = self.expression(id)?;
        let Some(ty) = self.ty_of(id) else {
            return Ok(rendered);
        };
        let kind = c_kind(ty);
        match (box_suffix(kind), scalar_suffix(kind)) {
            (Some(suffix), _) => Ok(format!("kt_box_{suffix}({rendered})")),
            (None, Some(_)) => Err(unrenderable(kind)),
            (None, None) => Ok(rendered),
        }
    }

    /// Realize a representation change between two carriers.
    fn coerce(&mut self, arg: u32, target: Ty) -> Result<String, Unsupported> {
        let rendered = self.expression(arg)?;
        let target = c_kind(target);
        let source = self.ty_of(arg).map(c_kind);
        match (source, target) {
            // Unknown source: the only safe move is to leave the value alone.
            (None, _) => Ok(rendered),
            (Some(source), target) if source == target => Ok(rendered),
            (Some(source @ CKind::Scalar(_)), CKind::Ref) => match box_suffix(source) {
                Some(suffix) => Ok(format!("kt_box_{suffix}({rendered})")),
                None => Err(unrenderable(source)),
            },
            (Some(CKind::Ref), CKind::Scalar(_)) => match box_suffix(target) {
                Some(suffix) => Ok(format!("kt_unbox_{suffix}({rendered})")),
                None => Err(unrenderable(target)),
            },
            // Scalar to a different scalar is a widening/narrowing C cast.
            (Some(CKind::Scalar(_)), CKind::Scalar(name)) => Ok(format!("(({name}){rendered})")),
            (Some(CKind::Ref), CKind::Ref) => Ok(rendered),
            (Some(_), CKind::Void) => Ok(rendered),
            (Some(CKind::Void), _) => Err("a coercion from `Unit`".to_string()),
        }
    }

    fn call(
        &mut self,
        callee: &Callee,
        dispatch_receiver: Option<u32>,
        args: &[u32],
    ) -> Result<String, Unsupported> {
        let receiver = dispatch_receiver;
        match callee {
            Callee::Intrinsic { operation, .. } => {
                self.intrinsic(operation, dispatch_receiver, args)
            }
            _ if dispatch_receiver.is_some() && !matches!(callee, Callee::External { .. }) => {
                Err(format!("a {} call with a receiver", callee_kind(callee)))
            }
            Callee::Local(function) => {
                let symbol = self.symbols[*function as usize].clone();
                let arguments = self.arguments(args)?;
                Ok(format!("{symbol}({arguments})"))
            }
            Callee::External { target, params, .. } => {
                let Some(realization) = self.classpath.external_callable(*target) else {
                    return Err("an unresolvable dependency call".to_string());
                };
                let owner = realization.callable.owner.render();
                let name = realization.callable.name.clone();
                match receiver {
                    // A member: the receiver is the runtime function's first argument.
                    Some(receiver) => {
                        let Some(symbol) = super::intrinsics::runtime_member(&owner, &name, params)
                        else {
                            return Err(undeclared(&owner, &name));
                        };
                        let mut rendered = vec![self.reference(receiver)?];
                        for argument in args {
                            rendered.push(self.reference(*argument)?);
                        }
                        Ok(format!("{symbol}({})", rendered.join(", ")))
                    }
                    None => {
                        let Some(symbol) =
                            super::intrinsics::runtime_function(&owner, &name, params)
                        else {
                            return Err(undeclared(&owner, &name));
                        };
                        let arguments = self.arguments(args)?;
                        Ok(format!("{symbol}({arguments})"))
                    }
                }
            }
            other => Err(format!("a {} call", callee_kind(other))),
        }
    }

    /// Realize a compiler-supplied operation the frontend selected in place of a call.
    fn intrinsic(
        &mut self,
        operation: &crate::ir::IrIntrinsic,
        receiver: Option<u32>,
        args: &[u32],
    ) -> Result<String, Unsupported> {
        use crate::ir::IrIntrinsic;
        match operation {
            IrIntrinsic::PrimitiveCompare { operand } => {
                let (Some(receiver), [argument]) = (receiver, args) else {
                    return Err("a malformed `compareTo`".to_string());
                };
                let Some(suffix) = scalar_suffix(c_kind(*operand)) else {
                    return Err("`compareTo` on a non-scalar operand".to_string());
                };
                Ok(format!(
                    "kt_compare_{suffix}({}, {})",
                    self.expression(receiver)?,
                    self.expression(*argument)?
                ))
            }
            IrIntrinsic::StringPlus => {
                let (Some(receiver), [argument]) = (receiver, args) else {
                    return Err("a malformed `String.plus`".to_string());
                };
                Ok(format!(
                    "kt_string_plus({}, {})",
                    self.reference(receiver)?,
                    self.reference(*argument)?
                ))
            }
            IrIntrinsic::NullableAnyToString => {
                let Some(receiver) = receiver else {
                    return Err("a malformed `toString`".to_string());
                };
                Ok(format!("kt_to_string({})", self.reference(receiver)?))
            }
            other => Err(format!("the `{other:?}` intrinsic")),
        }
    }

    fn arguments(&mut self, args: &[u32]) -> Result<String, Unsupported> {
        let mut rendered = Vec::with_capacity(args.len());
        for argument in args {
            rendered.push(self.expression(*argument)?);
        }
        Ok(rendered.join(", "))
    }
}

/// The diagnostic for a value whose type the runtime cannot represent as a reference.
fn unrenderable(kind: CKind) -> Unsupported {
    format!(
        "a `{}` in a position that requires a reference (the runtime cannot render a \
         floating-point value; see src/native/runtime.rs)",
        match scalar_suffix(kind) {
            Some("float") => "Float",
            _ => "Double",
        }
    )
}

/// The diagnostic for a dependency declaration the runtime has no implementation of.
fn undeclared(owner: &str, name: &str) -> Unsupported {
    format!("the declaration `{}.{name}`", owner.replace('/', "."))
}

fn indent_of(depth: usize) -> String {
    "    ".repeat(depth)
}

/// One C symbol per IR function, unique within the file.
fn function_symbols(ir: &IrFile) -> Vec<String> {
    let package = ir
        .package
        .as_deref()
        .map(c_identifier)
        .filter(|package| !package.is_empty());
    // Kotlin overloads share a name and C has no overloading, so a repeat gets a suffix. The
    // suffixed spelling is itself checked for collisions: a file declaring both `greet` and
    // `greet__1` would otherwise hand two functions the same symbol, and the linker would silently
    // pick one of them.
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    ir.functions
        .iter()
        .map(|function| {
            let base = match &package {
                Some(package) => format!("kt_{package}_{}", c_identifier(&function.name)),
                None => format!("kt_{}", c_identifier(&function.name)),
            };
            let mut candidate = base.clone();
            let mut ordinal = 0;
            while !taken.insert(candidate.clone()) {
                ordinal += 1;
                candidate = format!("{base}__{ordinal}");
            }
            candidate
        })
        .collect()
}

fn constant_expression(constant: &IrConst) -> Result<String, Unsupported> {
    Ok(match constant {
        IrConst::Boolean(value) => value.to_string(),
        IrConst::Byte(value) => format!("(kt_byte){value}"),
        IrConst::Short(value) => format!("(kt_short){value}"),
        // `INT32_MIN` has no C literal: `-2147483648` is a negation of an out-of-range positive.
        IrConst::Int(value) => format!("(kt_int)INT32_C({value})"),
        IrConst::Long(value) => format!("(kt_long)INT64_C({value})"),
        IrConst::Float(value) if value.is_finite() => format!("{value:?}f"),
        IrConst::Double(value) if value.is_finite() => format!("{value:?}"),
        IrConst::Float(_) | IrConst::Double(_) => {
            return Err("a non-finite floating-point constant".to_string())
        }
        IrConst::Char(value) => format!("(kt_char){value}"),
        IrConst::String(value) => {
            let Some((literal, length)) = c_string_literal(value) else {
                return Err("a string constant containing an unpaired surrogate".to_string());
            };
            format!("kt_string_utf8({literal}, {length})")
        }
        IrConst::Null => "NULL".to_string(),
    })
}

fn c_operator(op: IrBinOp) -> Result<&'static str, Unsupported> {
    Ok(match op {
        IrBinOp::Add => "+",
        IrBinOp::Sub => "-",
        IrBinOp::Mul => "*",
        IrBinOp::Div => "/",
        IrBinOp::Rem => "%",
        IrBinOp::Lt => "<",
        IrBinOp::Le => "<=",
        IrBinOp::Gt => ">",
        IrBinOp::Ge => ">=",
        // `Eq`/`Ne` reach here only for scalar operands; `binary` declines the reference case,
        // where Kotlin means structural equality and C's `==` would compare addresses.
        IrBinOp::Eq | IrBinOp::RefEq => "==",
        IrBinOp::Ne | IrBinOp::RefNe => "!=",
        IrBinOp::And => "&&",
        IrBinOp::Or => "||",
        IrBinOp::BitAnd => "&",
        IrBinOp::BitOr => "|",
        IrBinOp::BitXor => "^",
        IrBinOp::Shl => "<<",
        IrBinOp::Shr => ">>",
        // Reached only for the diagnostic text of an operand type the helpers do not cover.
        IrBinOp::Ushr => "ushr",
    })
}

fn callee_kind(callee: &Callee) -> &'static str {
    match callee {
        Callee::Local(_) => "local",
        Callee::ClassStatic { .. }
        | Callee::ClassStaticWithDefaults { .. }
        | Callee::ClassStaticDefault { .. } => "class-static",
        Callee::LocalDefault(_) | Callee::LocalWithDefaults { .. } => "defaulted",
        Callee::Intrinsic { .. } => "intrinsic",
        Callee::CrossFile { .. } => "cross-file",
        Callee::Module { .. } | Callee::ModuleWithDefaults { .. } => "same-module",
        Callee::External { .. } => "dependency",
        Callee::Static { .. } => "static",
        Callee::Virtual { .. } => "virtual",
        Callee::Super { .. } => "super",
        Callee::Special { .. } => "special",
    }
}

/// A short phrase naming the construct, for the declining diagnostic.
fn describe(node: &IrExpr) -> String {
    let debug = format!("{node:?}");
    let head = debug
        .split(|c: char| !c.is_ascii_alphanumeric())
        .find(|piece| !piece.is_empty())
        .unwrap_or("expression");
    format!("`{head}`")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kt_string::KtStringBuf;

    fn kt(text: &str) -> crate::kt_string::KtString {
        let mut buffer = KtStringBuf::new();
        buffer.push_str(text);
        buffer.finish()
    }

    #[test]
    fn a_nullable_primitive_is_carried_as_a_reference() {
        assert_eq!(c_kind(Ty::Int), CKind::Scalar("kt_int"));
        assert_eq!(
            c_kind(Ty::nullable(Ty::Int)),
            CKind::Ref,
            "`Int?` must represent `null`, so it boxes exactly as it does on the JVM"
        );
        assert_eq!(c_kind(Ty::Unit), CKind::Void);
        assert_eq!(c_kind(Ty::String), CKind::Ref);
    }

    #[test]
    fn a_string_literal_escapes_without_running_escapes_together() {
        let (literal, length) = c_string_literal(&kt("a\"b\\c\n")).expect("encodable");
        assert_eq!(literal, "\"a\\\"b\\\\c\\n\"");
        assert_eq!(length, 6);
    }

    #[test]
    fn a_non_ascii_literal_uses_octal_escapes_with_a_fixed_width() {
        // A hex escape in C has no length limit, so `"\xC3\xA9" "1"` is fine but `"\xC3\xA91"` is one
        // enormous character. Octal is exactly three digits, which cannot run into the next byte.
        let (literal, length) = c_string_literal(&kt("é1")).expect("encodable");
        assert_eq!(literal, "\"\\303\\2511\"");
        assert_eq!(length, 3, "two UTF-8 bytes for `é`, one for `1`");
    }

    #[test]
    fn an_unpaired_surrogate_is_declined_rather_than_mangled() {
        let lone = crate::kt_string::KtString::from_units(vec![0xD800]);
        assert!(
            c_string_literal(&lone).is_none(),
            "UTF-8 cannot encode a lone surrogate; emitting something else would be a silent \
             miscompilation"
        );
    }

    #[test]
    fn an_identifier_keeps_only_what_c_accepts() {
        assert_eq!(c_identifier("greet"), "greet");
        assert_eq!(c_identifier("my.package"), "my_package");
        assert_eq!(c_identifier("$fir_control_0_1"), "_fir_control_0_1");
    }

    #[test]
    fn overloads_get_distinct_c_symbols() {
        // C has no overloading. Two Kotlin functions sharing a name must not share a symbol, or
        // the linker would silently pick one of them.
        let mut ir = IrFile::default();
        for _ in 0..3 {
            ir.functions.push(crate::ir::IrFunction {
                name: "greet".to_string(),
                params: Vec::new(),
                ret: Ty::Unit,
                body: None,
                is_static: true,
                dispatch_receiver: None,
                param_checks: Vec::new(),
            });
        }
        assert_eq!(
            function_symbols(&ir),
            vec!["kt_greet", "kt_greet__1", "kt_greet__2"]
        );
    }

    #[test]
    fn a_name_that_collides_with_a_generated_suffix_still_gets_its_own_symbol() {
        // `greet` twice yields `kt_greet` and `kt_greet__1`. A Kotlin function actually NAMED
        // `greet__1` would otherwise be handed `kt_greet__1` as well, and the linker would quietly
        // keep one of the two bodies.
        let mut ir = IrFile::default();
        for name in ["greet", "greet__1", "greet"] {
            ir.functions.push(crate::ir::IrFunction {
                name: name.to_string(),
                params: Vec::new(),
                ret: Ty::Unit,
                body: None,
                is_static: true,
                dispatch_receiver: None,
                param_checks: Vec::new(),
            });
        }
        let symbols = function_symbols(&ir);
        assert_eq!(symbols, vec!["kt_greet", "kt_greet__1", "kt_greet__2"]);
        assert_eq!(
            symbols
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3
        );
    }

    #[test]
    fn a_package_qualifies_the_symbol() {
        let mut ir = IrFile::default();
        ir.package = Some("com.example.app".to_string());
        ir.functions.push(crate::ir::IrFunction {
            name: "main".to_string(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        assert_eq!(function_symbols(&ir), vec!["kt_com_example_app_main"]);
    }

    #[test]
    fn the_minimum_int_has_a_c_spelling() {
        // `-2147483648` is not an `int` literal in C; it is a negation of an out-of-range positive
        // one, which is `long`. `INT32_C` is what makes the constant typed correctly.
        assert_eq!(
            constant_expression(&IrConst::Int(i32::MIN)).expect("emittable"),
            "(kt_int)INT32_C(-2147483648)"
        );
        assert_eq!(
            constant_expression(&IrConst::Long(i64::MIN)).expect("emittable"),
            "(kt_long)INT64_C(-9223372036854775808)"
        );
    }

    #[test]
    fn a_non_finite_constant_is_declined() {
        // `inf` and `nan` have no C literal spelling at all.
        assert!(constant_expression(&IrConst::Double(f64::NAN)).is_err());
        assert!(constant_expression(&IrConst::Float(f32::INFINITY)).is_err());
    }
}

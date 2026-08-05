//! `krusty-ir` to JavaScript source emission.

use crate::ir::{Callee, IrBinOp, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::kt_string::KtString;
use crate::types::Ty;

/// Emit a whole file's IR as a JavaScript module (one `class` per IR class, one `function` per
/// top-level function).
pub fn emit_file(ir: &IrFile) -> String {
    let mut out = String::new();
    for c in &ir.classes {
        // Constructor params are the leading `ctor_param_count` fields, named `v1..=vN` to match the
        // IR value numbering (value 0 = `this`); fields after them are body properties set by
        // `init_body`.
        let n_params = c.ctor_param_count as usize;
        let params: Vec<String> = (1..=n_params).map(|i| format!("v{i}")).collect();
        out.push_str(&format!("class {} {{\n", class_simple(&c.fq_name())));
        out.push_str(&format!("  constructor({}) {{\n", params.join(", ")));
        for (i, f) in c.fields.iter().take(n_params).enumerate() {
            let n = &f.name;
            out.push_str(&format!("    this.{n} = v{};\n", i + 1));
        }
        if let Some(init_body) = c.init_body {
            emit_stmt(ir, init_body, 2, true, &mut out);
        }
        out.push_str("  }\n");
        for &fid in &c.methods {
            let f = &ir.functions[fid as usize];
            let Some(body) = f.body else { continue };
            // Instance method: value 0 = `this`, params are values 1..n.
            let params: Vec<String> = (0..f.params.len()).map(|i| format!("v{}", i + 1)).collect();
            out.push_str(&format!("  {}({}) {{\n", f.name, params.join(", ")));
            emit_stmt(ir, body, 2, true, &mut out);
            out.push_str("  }\n");
        }
        out.push_str("}\n");
    }
    // Top-level properties: module-level `let`s initialized in declaration order (after classes,
    // which a `new`-using initializer may reference; before functions, which JS hoists).
    for s in &ir.statics {
        out.push_str(&format!(
            "let {} = {};\n",
            s.name,
            emit_expr(ir, s.init, false)
        ));
    }
    for (i, f) in ir.functions.iter().enumerate() {
        if f.dispatch_receiver.is_some() {
            continue; // emitted as a class method above
        }
        let Some(body) = f.body else { continue };
        let _ = i;
        let params: Vec<String> = (0..f.params.len()).map(|i| format!("v{i}")).collect();
        out.push_str(&format!("function {}({}) {{\n", f.name, params.join(", ")));
        emit_stmt(ir, body, 1, false, &mut out);
        out.push_str("}\n");
    }
    out
}

fn class_simple(fq: &str) -> &str {
    fq.rsplit('/').next().unwrap_or(fq)
}

/// `x instanceof T` in JS — `String` is a primitive (`typeof`), a class is a real `instanceof`.
fn js_instanceof(arg: &str, t: &Ty) -> String {
    let nn = t.non_null();
    // A bare `Ty::String` has no `obj_internal()` (the Array→Obj migration landmine), so key it
    // explicitly → JS `typeof === "string"`. Other bare primitives (`is Int`) have no JS class/`typeof`
    // mapping here, so they keep the safe `false` default rather than a nonexistent `instanceof Int`.
    if nn == Ty::String
        || nn
            .obj_internal()
            .is_some_and(|n| n.matches("kotlin/String"))
    {
        return format!("(typeof {arg} === \"string\")");
    }
    if let Some(fq_name) = nn.obj_internal() {
        return format!("({arg} instanceof {})", class_simple(&fq_name.render()));
    }
    "false".to_string()
}

fn indent(n: usize, out: &mut String) {
    for _ in 0..n {
        out.push_str("  ");
    }
}

/// `inst` = inside an instance method (value 0 renders as `this`).
fn emit_stmt(ir: &IrFile, e: u32, depth: usize, inst: bool, out: &mut String) {
    match ir.expr(e) {
        IrExpr::Block { stmts, value } => {
            for &s in stmts {
                emit_stmt(ir, s, depth, inst, out);
            }
            if let Some(v) = value {
                indent(depth, out);
                out.push_str(&emit_expr(ir, *v, inst));
                out.push_str(";\n");
            }
        }
        IrExpr::Return(v) => {
            indent(depth, out);
            match v {
                Some(v) => out.push_str(&format!("return {};\n", emit_expr(ir, *v, inst))),
                None => out.push_str("return;\n"),
            }
        }
        IrExpr::Variable { index, init, .. } => {
            indent(depth, out);
            match init {
                Some(i) => out.push_str(&format!("let v{index} = {};\n", emit_expr(ir, *i, inst))),
                None => out.push_str(&format!("let v{index};\n")),
            }
        }
        IrExpr::SetValue { var, value } => {
            indent(depth, out);
            out.push_str(&format!(
                "{} = {};\n",
                val_name(*var, inst),
                emit_expr(ir, *value, inst)
            ));
        }
        IrExpr::SetField {
            receiver,
            class,
            index,
            value,
        } => {
            indent(depth, out);
            let name = &ir.classes[*class as usize].fields[*index as usize].name;
            out.push_str(&format!(
                "{}.{} = {};\n",
                emit_expr(ir, *receiver, inst),
                name,
                emit_expr(ir, *value, inst)
            ));
        }
        IrExpr::SetStatic { index, value } => {
            indent(depth, out);
            out.push_str(&format!(
                "{} = {};\n",
                ir.statics[*index as usize].name,
                emit_expr(ir, *value, inst)
            ));
        }
        IrExpr::While {
            cond,
            body,
            update,
            post_test,
            label,
        } => {
            if let Some(l) = label {
                indent(depth, out);
                out.push_str(&format!("{l}:\n"));
            }
            indent(depth, out);
            if *post_test {
                // `do { body } while (cond)` — post-test loop.
                out.push_str("do {\n");
                emit_stmt(ir, *body, depth + 1, inst, out);
                indent(depth, out);
                out.push_str(&format!("}} while ({});\n", emit_expr(ir, *cond, inst)));
            } else {
                // `update` (a `for`-loop increment) goes in the loop header so `continue` runs it,
                // matching a JS `for (; cond; update)`; a plain `while` has no update.
                match update {
                    Some(u) => out.push_str(&format!(
                        "for (; {}; {}) {{\n",
                        emit_expr(ir, *cond, inst),
                        emit_expr(ir, *u, inst)
                    )),
                    None => out.push_str(&format!("while ({}) {{\n", emit_expr(ir, *cond, inst))),
                }
                emit_stmt(ir, *body, depth + 1, inst, out);
                indent(depth, out);
                out.push_str("}\n");
            }
        }
        IrExpr::Break { label } => {
            indent(depth, out);
            out.push_str(
                &label
                    .as_ref()
                    .map(|l| format!("break {l};\n"))
                    .unwrap_or_else(|| "break;\n".to_string()),
            );
        }
        IrExpr::Continue { label } => {
            indent(depth, out);
            out.push_str(
                &label
                    .as_ref()
                    .map(|l| format!("continue {l};\n"))
                    .unwrap_or_else(|| "continue;\n".to_string()),
            );
        }
        // A `when`/`if` in STATEMENT position → an `if / else if / else` chain with STATEMENT bodies.
        // Rendering it as the expression ternary (the `other` arm below) would evaluate a `break`,
        // `continue` or `return` branch as a value and drop it — e.g. `if (c) break` would no-op.
        IrExpr::When { branches } => {
            let mut first = true;
            for (cond, body) in branches {
                indent(depth, out);
                match cond {
                    Some(c) => {
                        let kw = if first { "if" } else { "else if" };
                        out.push_str(&format!("{kw} ({}) {{\n", emit_expr(ir, *c, inst)));
                    }
                    None => out.push_str("else {\n"),
                }
                emit_stmt(ir, *body, depth + 1, inst, out);
                indent(depth, out);
                out.push_str("}\n");
                first = false;
            }
        }
        other => {
            indent(depth, out);
            out.push_str(&emit_expr_node(ir, other, inst));
            out.push_str(";\n");
        }
    }
}

fn val_name(i: u32, inst: bool) -> String {
    if inst && i == 0 {
        "this".to_string()
    } else {
        format!("v{i}")
    }
}

fn emit_expr(ir: &IrFile, e: u32, inst: bool) -> String {
    emit_expr_node(ir, ir.expr(e), inst)
}

fn emit_args(ir: &IrFile, args: &[u32], inst: bool) -> String {
    args.iter()
        .map(|&x| emit_expr(ir, x, inst))
        .collect::<Vec<_>>()
        .join(", ")
}

fn emit_expr_node(ir: &IrFile, node: &IrExpr, inst: bool) -> String {
    match node {
        IrExpr::Const(c) => match c {
            IrConst::Boolean(b) => b.to_string(),
            IrConst::Int(v) => v.to_string(),
            IrConst::Long(v) => v.to_string(),
            IrConst::Short(v) => v.to_string(),
            IrConst::Byte(v) => v.to_string(),
            IrConst::Float(v) => v.to_string(),
            IrConst::Double(v) => v.to_string(),
            // A `Char` is a UTF-16 code unit; `\uXXXX` reproduces it exactly (JS strings admit lone
            // surrogates, so no code-point round-trip is needed).
            IrConst::Char(c) => format!("'\\u{c:04X}'"),
            IrConst::String(s) => js_string(s),
            IrConst::Null => "null".to_string(),
        },
        IrExpr::GetValue(i) => val_name(*i, inst),
        IrExpr::GetStatic(i) => ir.statics[*i as usize].name.clone(),
        IrExpr::GetField {
            receiver,
            class,
            index,
        } => {
            let name = &ir.classes[*class as usize].fields[*index as usize].name;
            format!("{}.{}", emit_expr(ir, *receiver, inst), name)
        }
        IrExpr::PropertyRead {
            receiver,
            owner,
            name,
            ..
        } => {
            let receiver = emit_expr(ir, *receiver, inst);
            // Plain JavaScript has no Kotlin accessor ABI, but a source-written Kotlin accessor is still
            // executable user code: common lowering retains that body as an IrFunction. Calling it here
            // is the JS realization of the same semantic property operation; using `receiver.name`
            // unconditionally would bypass computed/custom getters and silently read the backing field.
            match declared_property_accessor(ir, *owner, name, false) {
                Some(accessor) => format!("{receiver}.{}()", accessor.name),
                None => format!("{receiver}.{name}"),
            }
        }
        IrExpr::New { internal, args, .. } => {
            let fq = internal.render();
            let name = class_simple(&fq);
            format!("new {}({})", name, emit_args(ir, args, inst))
        }
        IrExpr::MethodCall {
            class,
            index,
            receiver,
            args,
        } => {
            let fid = ir.classes[*class as usize].methods[*index as usize];
            let name = &ir.functions[fid as usize].name;
            // An omitted argument (`None`) takes its default — `undefined` lets JS apply the native default.
            let a: Vec<String> = args
                .iter()
                .map(|x| {
                    x.map(|e| emit_expr(ir, e, inst))
                        .unwrap_or_else(|| "undefined".to_string())
                })
                .collect();
            format!(
                "{}.{}({})",
                emit_expr(ir, *receiver, inst),
                name,
                a.join(", ")
            )
        }
        IrExpr::PrimitiveBinOp { op, lhs, rhs } => {
            format!(
                "({} {} {})",
                emit_expr(ir, *lhs, inst),
                js_op(*op),
                emit_expr(ir, *rhs, inst)
            )
        }
        IrExpr::PrimitiveNeg { operand, .. } => {
            format!("(-{})", emit_expr(ir, *operand, inst))
        }
        IrExpr::Call {
            callee,
            dispatch_receiver,
            args,
        } => match callee {
            Callee::Local(fid) => {
                let name = &ir.functions[*fid as usize].name;
                format!("{}({})", name, emit_args(ir, args, inst))
            }
            Callee::LocalDefault(fid) => {
                let name = format!("{}$default", ir.functions[*fid as usize].name);
                format!("{}({})", name, emit_args(ir, args, inst))
            }
            // A resolved JVM static call has no JS equivalent — emit the receiver-first form by name.
            Callee::Static { name, .. } => {
                format!("{}({})", name, emit_args(ir, args, inst))
            }
            // A cross-file top-level function — by name (JS has a flat function namespace).
            Callee::CrossFile { name, .. } => {
                format!("{}({})", name, emit_args(ir, args, inst))
            }
            // A resolved JVM instance call → `receiver.name(args)`.
            Callee::Virtual { name, .. } => {
                let recv = dispatch_receiver
                    .map(|r| emit_expr(ir, r, inst))
                    .unwrap_or_default();
                format!("{}.{}({})", recv, name, emit_args(ir, args, inst))
            }
            // `super.name(args)` → JS `<base>.prototype.name.call(this, …)` is the closest, but the JS
            // backend doesn't track the base name; emit the plain super form.
            Callee::Special { name, .. } => {
                format!("super.{}({})", name, emit_args(ir, args, inst))
            }
            Callee::External(fq) => match fq.as_str() {
                "kotlin/String.plus" => {
                    let r = emit_expr(ir, dispatch_receiver.unwrap(), inst);
                    format!("({} + {})", r, emit_expr(ir, args[0], inst))
                }
                "kotlin/String.length" | "kotlin/Array.size" => {
                    format!("{}.length", emit_expr(ir, dispatch_receiver.unwrap(), inst))
                }
                "kotlin/String.get" => format!(
                    "{}[{}]",
                    emit_expr(ir, dispatch_receiver.unwrap(), inst),
                    emit_expr(ir, args[0], inst)
                ),
                "kotlin/String.hashCode" => {
                    let recv = emit_expr(ir, dispatch_receiver.unwrap(), inst);
                    format!(
                        "(()=>{{const s={recv};let h=0;for(let i=0;i<s.length;i++){{h=((h*31)+s.charCodeAt(i))|0;}}return h;}})()"
                    )
                }
                "kotlin/Any.toString" => format!(
                    "String({})",
                    emit_expr(ir, dispatch_receiver.unwrap(), inst)
                ),
                // Arrays are a regular type the JS backend lowers to a JS `Array`.
                "kotlin/Array.get" => format!(
                    "{}[{}]",
                    emit_expr(ir, dispatch_receiver.unwrap(), inst),
                    emit_expr(ir, args[0], inst)
                ),
                "kotlin/Array.set" => format!(
                    "({}[{}] = {})",
                    emit_expr(ir, dispatch_receiver.unwrap(), inst),
                    emit_expr(ir, args[0], inst),
                    emit_expr(ir, args[1], inst)
                ),
                // Primitive arrays lower to JS typed arrays (the real Kotlin/JS representation —
                // zero-filled, `.length`, indexable). Boolean has no typed array; use a filled Array.
                _ if fq.ends_with("Array.<init>") => {
                    let n = emit_expr(ir, args[0], inst);
                    match fq.trim_start_matches("kotlin/").trim_end_matches(".<init>") {
                        "IntArray" => format!("new Int32Array({n})"),
                        "DoubleArray" => format!("new Float64Array({n})"),
                        "FloatArray" => format!("new Float32Array({n})"),
                        "ByteArray" => format!("new Int8Array({n})"),
                        "ShortArray" => format!("new Int16Array({n})"),
                        "CharArray" => format!("new Uint16Array({n})"),
                        "BooleanArray" => format!("new Array({n}).fill(false)"),
                        _ => format!("new Array({n}).fill(0)"), // LongArray etc.
                    }
                }
                _ => "undefined".to_string(),
            },
        },
        IrExpr::TypeOp {
            op,
            arg,
            type_operand,
        } => {
            let a = emit_expr(ir, *arg, inst);
            match op {
                IrTypeOp::InstanceOf => js_instanceof(&a, type_operand),
                IrTypeOp::NotInstanceOf => format!("(!{})", js_instanceof(&a, type_operand)),
                // JS is untyped — a cast is the value itself.
                _ => a,
            }
        }
        IrExpr::When { branches } => {
            let mut s = String::new();
            let mut closes = 0;
            let mut tail = "undefined".to_string();
            for (cond, body) in branches {
                match cond {
                    Some(c) => {
                        s.push_str(&format!(
                            "({} ? {} : ",
                            emit_expr(ir, *c, inst),
                            emit_expr(ir, *body, inst)
                        ));
                        closes += 1;
                    }
                    None => tail = emit_expr(ir, *body, inst),
                }
            }
            s.push_str(&tail);
            for _ in 0..closes {
                s.push(')');
            }
            s
        }
        // Assignments are valid JS *expressions* (`x = e`), not only statements. They appear in an
        // expression position most importantly as a `for`-loop update (`for (; cond; i = i + 1)`),
        // where rendering them as `undefined` would drop the increment and spin forever.
        IrExpr::SetValue { var, value } => {
            format!(
                "({} = {})",
                val_name(*var, inst),
                emit_expr(ir, *value, inst)
            )
        }
        IrExpr::SetField {
            receiver,
            class,
            index,
            value,
        } => {
            let name = &ir.classes[*class as usize].fields[*index as usize].name;
            format!(
                "({}.{} = {})",
                emit_expr(ir, *receiver, inst),
                name,
                emit_expr(ir, *value, inst)
            )
        }
        IrExpr::PropertyWrite {
            receiver,
            owner,
            name,
            value,
            ..
        } => {
            let receiver = emit_expr(ir, *receiver, inst);
            let value = emit_expr(ir, *value, inst);
            // As for reads, a source-written setter body is a real method in the common IR and must run.
            // A plain/default property has no such method and maps naturally to a JS field assignment.
            match declared_property_accessor(ir, *owner, name, true) {
                Some(accessor) => format!("{receiver}.{}({value})", accessor.name),
                None => format!("({receiver}.{name} = {value})"),
            }
        }
        IrExpr::SetStatic { index, value } => {
            format!(
                "({} = {})",
                ir.statics[*index as usize].name,
                emit_expr(ir, *value, inst)
            )
        }
        // A `vararg` argument pack is a JS array literal; a spread entry contributes its elements
        // rather than itself, which is exactly JS's own `...` spread. (The JVM backend needs the
        // platform `SpreadBuilder` for the same node; JS has the operation natively.)
        IrExpr::Vararg {
            elements, spreads, ..
        } => {
            let items: Vec<String> = elements
                .iter()
                .enumerate()
                .map(|(index, &element)| {
                    let value = emit_expr(ir, element, inst);
                    if spreads.get(index).copied().unwrap_or(false) {
                        format!("...{value}")
                    } else {
                        value
                    }
                })
                .collect();
            format!("[{}]", items.join(", "))
        }
        // The JS backend covers a subset of the IR. A node it cannot represent must NOT masquerade as a
        // value: `undefined` silently compiles a wrong program (a property read read back as `undefined`
        // instead of the value, with no error anywhere). JS has no compile step of its own, so the honest
        // realization is a throw at the point of use.
        other => format!(
            "(() => {{ throw new Error(\"krusty: JS backend cannot emit {}\"); }})()",
            expr_kind(other)
        ),
    }
}

/// The source-written accessor body for a property declared in this IR file. Default accessors are
/// intentionally absent: they are target realization, so JS uses its native field operation for them.
fn declared_property_accessor<'a>(
    ir: &'a IrFile,
    owner: crate::types::TypeName,
    name: &str,
    setter: bool,
) -> Option<&'a crate::ir::IrFunction> {
    let class = ir.classes.iter().find(|class| class.fq_name == owner)?;
    let property = class
        .properties
        .iter()
        .find(|property| property.name == name)?;
    let function = if setter {
        property.setter
    } else {
        property.getter
    }?;
    ir.functions.get(function as usize)
}

/// Variant name of an IR node, for the unsupported-node message.
fn expr_kind(e: &IrExpr) -> String {
    format!("{e:?}")
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect()
}

fn js_op(op: IrBinOp) -> &'static str {
    match op {
        IrBinOp::Add => "+",
        IrBinOp::Sub => "-",
        IrBinOp::Mul => "*",
        IrBinOp::Div => "/",
        IrBinOp::Rem => "%",
        IrBinOp::Lt => "<",
        IrBinOp::Le => "<=",
        IrBinOp::Gt => ">",
        IrBinOp::Ge => ">=",
        IrBinOp::Eq => "===",
        IrBinOp::Ne => "!==",
        IrBinOp::RefEq => "===",
        IrBinOp::RefNe => "!==",
        IrBinOp::And => "&&",
        IrBinOp::Or => "||",
        IrBinOp::BitAnd => "&",
        IrBinOp::BitOr => "|",
        IrBinOp::BitXor => "^",
        IrBinOp::Shl => "<<",
        IrBinOp::Shr => ">>",
        IrBinOp::Ushr => ">>>",
    }
}

/// A JS string literal for a Kotlin string value. A JS string is also a UTF-16 code-unit sequence,
/// so an unpaired surrogate is written as `\uXXXX` and survives verbatim.
fn js_string(s: &KtString) -> String {
    let mut out = String::from("\"");
    for unit in s.units() {
        match char::from_u32(unit as u32) {
            Some('"') => out.push_str("\\\""),
            Some('\\') => out.push_str("\\\\"),
            Some('\n') => out.push_str("\\n"),
            Some(c) => out.push(c),
            None => out.push_str(&format!("\\u{unit:04X}")),
        }
    }
    out.push('"');
    out
}

//! A second backend: `krusty-ir` → JavaScript source.
//!
//! Its sole purpose is to **validate the front-end/back-end boundary** — it consumes the same
//! backend-agnostic [`IrFile`] the JVM backend does, with no shared lowering and no dependency on
//! the JVM module. If the same IR runs correctly on both the JVM and Node, the IR is genuinely
//! target-neutral. Covers the core subset (`ir_lower`'s output): functions, simple classes,
//! control flow, and stdlib intrinsics realized from the JS platform.

use crate::ir::{Callee, IrBinOp, IrConst, IrExpr, IrFile, IrTypeOp};
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
        out.push_str(&format!("class {} {{\n", class_simple(&c.fq_name)));
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
    if let Some(fq_name) = t.non_null().obj_internal() {
        match fq_name {
            "kotlin/String" => return format!("(typeof {arg} === \"string\")"),
            _ => return format!("({arg} instanceof {})", class_simple(fq_name)),
        }
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
            IrConst::Char(c) => format!("{:?}", c),
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
        IrExpr::New { class, args, .. } => {
            let name = class_simple(&ir.classes[*class as usize].fq_name);
            let a: Vec<String> = args.iter().map(|&x| emit_expr(ir, x, inst)).collect();
            format!("new {}({})", name, a.join(", "))
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
        IrExpr::Call {
            callee,
            dispatch_receiver,
            args,
        } => match callee {
            Callee::Local(fid) => {
                let name = &ir.functions[*fid as usize].name;
                let a: Vec<String> = args.iter().map(|&x| emit_expr(ir, x, inst)).collect();
                format!("{}({})", name, a.join(", "))
            }
            // A resolved JVM static call has no JS equivalent — emit the receiver-first form by name.
            Callee::Static { name, .. } => {
                let a: Vec<String> = args.iter().map(|&x| emit_expr(ir, x, inst)).collect();
                format!("{}({})", name, a.join(", "))
            }
            // A cross-file top-level function — by name (JS has a flat function namespace).
            Callee::CrossFile { name, .. } => {
                let a: Vec<String> = args.iter().map(|&x| emit_expr(ir, x, inst)).collect();
                format!("{}({})", name, a.join(", "))
            }
            // A resolved JVM instance call → `receiver.name(args)`.
            Callee::Virtual { name, .. } | Callee::CrossFileVirtual { name, .. } => {
                let recv = dispatch_receiver
                    .map(|r| emit_expr(ir, r, inst))
                    .unwrap_or_default();
                let a: Vec<String> = args.iter().map(|&x| emit_expr(ir, x, inst)).collect();
                format!("{}.{}({})", recv, name, a.join(", "))
            }
            // `super.name(args)` → JS `<base>.prototype.name.call(this, …)` is the closest, but the JS
            // backend doesn't track the base name; emit the plain super form.
            Callee::Special { name, .. } => {
                let a: Vec<String> = args.iter().map(|&x| emit_expr(ir, x, inst)).collect();
                format!("super.{}({})", name, a.join(", "))
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
        _ => "undefined".to_string(),
    }
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

fn js_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

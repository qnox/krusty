//! A second backend: `krusty-ir` → JavaScript source.
//!
//! Its sole purpose is to **validate the front-end/back-end boundary** — it consumes the same
//! backend-agnostic [`IrFile`] the JVM backend does, with no shared lowering and no dependency on
//! the JVM module. If the same IR runs correctly on both the JVM and Node, the IR is genuinely
//! target-neutral. Like the JVM IR emitter, it covers the core subset (`ir_lower`'s output).

use crate::ir::{Callee, IrBinOp, IrConst, IrExpr, IrFile};

/// Emit a whole file's IR as a JavaScript module (one `function` per IR function).
pub fn emit_file(ir: &IrFile) -> String {
    let mut out = String::new();
    for f in &ir.functions {
        let params: Vec<String> = (0..f.params.len()).map(|i| format!("v{i}")).collect();
        out.push_str(&format!("function {}({}) {{\n", f.name, params.join(", ")));
        if let Some(body) = f.body {
            emit_stmt(ir, body, 1, &mut out);
        }
        out.push_str("}\n");
    }
    out
}

fn indent(n: usize, out: &mut String) {
    for _ in 0..n {
        out.push_str("  ");
    }
}

/// Emit a statement-position IR node (block / return / variable / assignment / expr-stmt).
fn emit_stmt(ir: &IrFile, e: u32, depth: usize, out: &mut String) {
    match ir.expr(e) {
        IrExpr::Block { stmts, value } => {
            for &s in stmts {
                emit_stmt(ir, s, depth, out);
            }
            if let Some(v) = value {
                indent(depth, out);
                out.push_str(&emit_expr(ir, *v));
                out.push_str(";\n");
            }
        }
        IrExpr::Return(v) => {
            indent(depth, out);
            match v {
                Some(v) => out.push_str(&format!("return {};\n", emit_expr(ir, *v))),
                None => out.push_str("return;\n"),
            }
        }
        IrExpr::Variable { index, init, .. } => {
            indent(depth, out);
            match init {
                Some(i) => out.push_str(&format!("let v{index} = {};\n", emit_expr(ir, *i))),
                None => out.push_str(&format!("let v{index};\n")),
            }
        }
        IrExpr::SetValue { var, value } => {
            indent(depth, out);
            out.push_str(&format!("v{var} = {};\n", emit_expr(ir, *value)));
        }
        IrExpr::While { cond, body } => {
            indent(depth, out);
            out.push_str(&format!("while ({}) {{\n", emit_expr(ir, *cond)));
            emit_stmt(ir, *body, depth + 1, out);
            indent(depth, out);
            out.push_str("}\n");
        }
        other => {
            indent(depth, out);
            out.push_str(&emit_expr_node(ir, other));
            out.push_str(";\n");
        }
    }
}

/// Emit an expression-position IR node as a JS expression string.
fn emit_expr(ir: &IrFile, e: u32) -> String {
    emit_expr_node(ir, ir.expr(e))
}

fn emit_expr_node(ir: &IrFile, node: &IrExpr) -> String {
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
        IrExpr::GetValue(i) => format!("v{i}"),
        IrExpr::PrimitiveBinOp { op, lhs, rhs } => {
            format!("({} {} {})", emit_expr(ir, *lhs), js_op(*op), emit_expr(ir, *rhs))
        }
        IrExpr::Call { callee, dispatch_receiver, args } => match callee {
            Callee::Local(fid) => {
                let name = &ir.functions[*fid as usize].name;
                let a: Vec<String> = args.iter().map(|&x| emit_expr(ir, x)).collect();
                format!("{}({})", name, a.join(", "))
            }
            // The JS platform's realization of a stdlib intrinsic (cf. the JVM `StringBuilder` form).
            Callee::Intrinsic(fq) => match fq.as_str() {
                // `String.plus`: JS `+` coerces the rhs to string when the lhs is a string.
                "kotlin/String.plus" => {
                    let r = emit_expr(ir, dispatch_receiver.unwrap());
                    format!("({} + {})", r, emit_expr(ir, args[0]))
                }
                _ => "undefined".to_string(),
            },
        },
        // `if`/`when` (an expression) → a chained ternary; the final `else` (None condition) is the tail.
        IrExpr::When { branches } => {
            let mut s = String::new();
            let mut closes = 0;
            let mut tail = "undefined".to_string();
            for (cond, body) in branches {
                match cond {
                    Some(c) => {
                        s.push_str(&format!("({} ? {} : ", emit_expr(ir, *c), emit_expr(ir, *body)));
                        closes += 1;
                    }
                    None => tail = emit_expr(ir, *body),
                }
            }
            s.push_str(&tail);
            for _ in 0..closes {
                s.push(')');
            }
            s
        }
        IrExpr::Return(_) | IrExpr::Block { .. } | IrExpr::Variable { .. } | IrExpr::SetValue { .. } => {
            // Statement-position nodes shouldn't be requested as expressions in the core subset.
            "undefined".to_string()
        }
        IrExpr::TypeOp { .. } | IrExpr::While { .. } => "undefined".to_string(),
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
        IrBinOp::And => "&&",
        IrBinOp::Or => "||",
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

//! Flat, id-ordered text rendering of a lowered `IrFile`.
//!
//! Mirrors `crate::ast_print`: the IR is index-based, so the expression arena is listed in id order
//! rather than rebuilt into a tree. Declarations print first so the arena listing has context.

use crate::ir::IrFile;
use std::fmt;

/// Render an `IrFile`'s declarations and expression arena as text.
pub fn render_ir_file(ir: &IrFile) -> String {
    let mut out = String::new();
    write_ir_file(&mut out, ir).expect("writing to a String cannot fail");
    out
}

/// Write an `IrFile`'s declarations and expression arena to `out`.
///
/// The fallible sink is shared with the bounded whole-document renderer. Propagating an exhausted
/// sink out of each loop prevents a truncated dump from paying to format the rest of a large IR.
pub fn write_ir_file(out: &mut impl fmt::Write, ir: &IrFile) -> fmt::Result {
    writeln!(
        out,
        "package: {}",
        ir.package.as_deref().unwrap_or("<none>")
    )?;
    writeln!(out, "source_line_count: {}", ir.source_line_count)?;

    writeln!(out, "\nfunctions ({})", ir.functions.len())?;
    for (id, function) in ir.functions.iter().enumerate() {
        writeln!(out, "  [{id}] {function:?}")?;
    }

    writeln!(out, "\nclasses ({})", ir.classes.len())?;
    for (id, class) in ir.classes.iter().enumerate() {
        writeln!(out, "  [{id}] {class:?}")?;
    }

    writeln!(out, "\nstatics ({})", ir.statics.len())?;
    for (id, static_property) in ir.statics.iter().enumerate() {
        writeln!(out, "  [{id}] {static_property:?}")?;
    }

    writeln!(out, "\nexprs ({})", ir.exprs.len())?;
    for (id, expr) in ir.exprs.iter().enumerate() {
        let key = id as u32;
        let line = match ir.expr_source_lines.get(&key) {
            Some(line) => format!("line={line}"),
            None => "line=?".to_string(),
        };
        writeln!(out, "  [{id}] {line} {expr:?}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lower(_source: &str) -> IrFile {
        let mut ir = IrFile::default();
        let value = ir.add_expr(crate::ir::IrExpr::Const(crate::ir::IrConst::String(
            "OK".to_string().into(),
        )));
        ir.functions.push(crate::ir::IrFunction {
            name: "box".to_string(),
            params: Vec::new(),
            ret: crate::types::Ty::String,
            body: Some(value),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir
    }

    #[test]
    fn renders_functions_and_every_expression_slot() {
        let ir = lower("fun box(): String = \"OK\"\n");
        let text = render_ir_file(&ir);

        assert!(text.contains("functions (1)"), "{text}");
        assert!(text.contains("box"), "{text}");
        assert!(
            text.contains(&format!("exprs ({})", ir.exprs.len())),
            "{text}"
        );
        for id in 0..ir.exprs.len() {
            assert!(
                text.contains(&format!("\n  [{id}] ")),
                "missing expr {id}:\n{text}"
            );
        }
    }

    #[test]
    fn expression_lines_carry_source_line_when_known() {
        let ir = lower("fun box(): String = \"OK\"\n");
        let text = render_ir_file(&ir);
        assert!(text.contains("line="), "{text}");
    }

    #[test]
    fn absent_package_is_rendered_explicitly() {
        let ir = lower("fun box(): String = \"OK\"\n");
        assert!(render_ir_file(&ir).contains("package: <none>"));
    }
}

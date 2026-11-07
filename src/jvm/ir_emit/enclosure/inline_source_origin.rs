//! The identity source map kotlinc writes for a class generated inside an `inline` function
//! (`visitSMAP(…, intoInline = true)`): code inlining that function copies the class and maps its
//! lines through it, so the map is written even when the class inlines nothing. A call-site copy
//! of such a class keeps its declaration's map, under the declaration class's path.

use crate::ir::{FunId, IrClass, IrFile};
use crate::jvm::classfile::ClassWriter;

/// What starting an inline class's map found about the class.
pub(in crate::jvm::ir_emit) struct InlineClassOrigin {
    /// The class is a call-site copy of a class an inline declaration declares.
    pub(in crate::jvm::ir_emit) regenerated: bool,
    /// The copy's declaration class's `EnclosingMethod`, which kotlinc's transformer visits
    /// before replacing it.
    pub(in crate::jvm::ir_emit) declaration_enclosure: Option<(String, Option<(String, String)>)>,
}

/// Start the identity map of `class`, generated inside an inline function. A call-site copy, whose
/// `implementation` specializes its declaration's, maps under that declaration, from its source
/// file; a class declared in another file's inline function, compiled here for a call
/// site that inlines it, maps from that file; any other class from this file.
pub(in crate::jvm::ir_emit) fn start_inline_class_source_map(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    class: &IrClass,
    implementation: Option<FunId>,
    facade: &str,
    writer: &mut ClassWriter,
) -> InlineClassOrigin {
    let regenerated = implementation
        .and_then(|function| regenerated_inline_origin(ir, override_results, function, facade));
    if let Some(origin) = regenerated {
        writer.set_source_file(Some(origin.source_file));
        writer.start_inline_origin_source_map_at(origin.lines, &origin.path);
        return InlineClassOrigin {
            regenerated: true,
            declaration_enclosure: origin.declaration_enclosure,
        };
    }
    match foreign_declaration_source(ir, class) {
        Some((source_file, lines)) => {
            writer.set_source_file(Some(source_file));
            writer.start_inline_origin_source_map(lines);
        }
        None => {
            let lines = u16::try_from(ir.source_line_count)
                .unwrap_or(u16::MAX)
                .max(1);
            writer.start_inline_origin_source_map(lines);
        }
    }
    InlineClassOrigin {
        regenerated: false,
        declaration_enclosure: None,
    }
}

struct RegeneratedInlineOrigin {
    source_file: String,
    path: String,
    lines: u16,
    declaration_enclosure: Option<(String, Option<(String, String)>)>,
}

/// The declaration source and original enclosure a call-site-specialized lambda class was
/// regenerated from.
fn regenerated_inline_origin(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    mut function: FunId,
    facade: &str,
) -> Option<RegeneratedInlineOrigin> {
    let template = crate::jvm::lambda_classes::specialization_template(ir, function)?;
    let declaration_facade = ir
        .foreign_template_facade(template)
        .map_or(facade, |facade| {
            crate::jvm::names::classfile_internal_name_of(facade)
        });
    let declaration_enclosure = super::scope_enclosure(
        ir,
        override_results,
        crate::ir::IrEnclosure::Function(template),
        declaration_facade,
    );
    function = crate::jvm::lambda_classes::specialization_source(ir, function);
    let source = if ir.foreign_inline_templates.contains(&template) {
        ir.foreign_template_source(template)?.source
    } else {
        ir.source_debug.current()?
    };
    let debug = ir.source_debug.file(source)?;
    let source_file = debug
        .name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&debug.name)
        .to_string();
    let path = ir.lambda_class_names.get(&function)?.render();
    let lines = u16::try_from(debug.line_count).unwrap_or(u16::MAX).max(1);
    Some(RegeneratedInlineOrigin {
        source_file,
        path,
        lines,
        declaration_enclosure,
    })
}

/// The source file name and line count of the file whose inline function declares `class`, when
/// that is another file.
fn foreign_declaration_source(ir: &IrFile, class: &IrClass) -> Option<(String, u16)> {
    let template = crate::jvm::inner_classes::foreign_declaration(ir, class)?;
    let debug = ir
        .source_debug
        .file(ir.foreign_template_source(template)?.source)?;
    let source_file = debug.name.rsplit(['/', '\\']).next().unwrap_or(&debug.name);
    let lines = u16::try_from(debug.line_count).unwrap_or(u16::MAX).max(1);
    Some((source_file.to_string(), lines))
}

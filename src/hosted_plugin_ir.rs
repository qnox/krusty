//! The module's own declarations as the IR krusty hands kotlinc compiler plugins
//! ([`krusty_plugin_ir::ModuleIr`]), built from Pass 2's checked FIR.
//!
//! Checked FIR keeps what kotlinc's fir2ir output keeps and common IR has already lowered away
//! (string templates, operator calls, constants as written), so the plugin IR is built from it,
//! beside common lowering. Code the plugin IR cannot spell yet is refused by what it is and where,
//! never approximated.

mod call_spans;
mod encoder;
mod function;
mod types;

use crate::diag::{DiagSink, Span};
use crate::fir::SourceFileId;
use crate::frontend::StreamingSourceSetAnalysis;

/// What the plugin IR cannot spell yet, and where.
pub(crate) struct Refusal {
    source: SourceFileId,
    span: Span,
    what: String,
}

/// The [`ModuleIr`](krusty_plugin_ir::ModuleIr) of an analyzed source set whose inputs are at
/// `paths` (one per input, as kotlinc names the files, parallel to `stems`). Pass 2 checks and
/// lowers the module as an ordinary compilation does, without a target; its diagnostics, and a
/// refusal of code the plugin IR cannot spell yet, go to `diags`, and then there is no IR. So does
/// a `paths` that does not name every input, before anything is analyzed further.
pub fn plugin_module_ir(
    analysis: impl Into<StreamingSourceSetAnalysis>,
    stems: &[String],
    paths: &[String],
    module_name: &str,
    diags: &mut DiagSink,
) -> Option<krusty_plugin_ir::ModuleIr> {
    if paths.len() != stems.len() {
        diags.error(
            Span::new(0, 0),
            format!(
                "compiler plugins need the path of every source: {} paths were given for {} sources",
                paths.len(),
                stems.len()
            ),
        );
        return None;
    }
    let mut encoder = encoder::ModuleIrEncoder::new(paths);
    crate::compiler::lower_analyzed_observing(analysis, stems, module_name, &mut encoder, diags);
    if diags.has_errors() {
        return None;
    }
    match encoder.finish() {
        Ok(module) => Some(module),
        Err(refusal) => {
            diags.set_file(refusal.source.raw());
            diags.error(
                refusal.span,
                format!(
                    "compiler plugins cannot run on this code yet: krusty's plugin IR does not cover {}",
                    refusal.what
                ),
            );
            None
        }
    }
}

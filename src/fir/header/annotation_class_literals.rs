//! Class literals written as annotation arguments, kept past the source AST for later passes.

use super::{Expr, File};

/// The class literals written as the FIRST argument of each annotation in `annotation_args`, as
/// dotted source paths keyed by annotation ordinal.
///
/// `@Serializable(with = pkg.Type::class)` is a semantic fact a later pass must be able to read, and
/// the annotation's argument expressions do not survive the source AST. Declaration and value-
/// parameter annotations are extracted by this one function so the two cannot disagree about which
/// written shapes count.
pub(super) fn annotation_class_literals(
    file: &File,
    annotation_args: &[Vec<crate::ast::ExprId>],
) -> Vec<(u32, String)> {
    fn qualifier_segments(
        file: &File,
        expression: crate::ast::ExprId,
        out: &mut Vec<String>,
    ) -> bool {
        match file.expr(expression) {
            Expr::Name(name) => {
                out.push(name.clone());
                true
            }
            Expr::Member { receiver, name } => {
                if !qualifier_segments(file, *receiver, out) {
                    return false;
                }
                out.push(name.clone());
                true
            }
            _ => false,
        }
    }

    let mut literals = Vec::new();
    for (annotation_ordinal, arguments) in annotation_args.iter().enumerate() {
        let Some(&argument) = arguments.first() else {
            continue;
        };
        let Expr::CallableRef {
            receiver: Some(receiver),
            name,
        } = file.expr(argument)
        else {
            continue;
        };
        if name != "class" {
            continue;
        }
        let mut segments = Vec::new();
        if !qualifier_segments(file, *receiver, &mut segments) || segments.is_empty() {
            continue;
        }
        literals.push((
            u32::try_from(annotation_ordinal).expect("too many annotations on one declaration"),
            segments.join("."),
        ));
    }
    literals
}

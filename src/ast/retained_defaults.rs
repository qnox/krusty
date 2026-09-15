//! Which default-argument spans survive when a file's body arenas are released.
//!
//! A declaration's metadata outlives its bodies: a parameter default is still needed to report and
//! to realize a default call long after the expression arena it was parsed into is gone. These walks
//! collect exactly those spans before the arenas are dropped, so nothing else has to keep the whole
//! arena alive to answer for one default.

use crate::ast::{ClassDecl, Decl, ExprId, File, Param};
use crate::diag::Span;
use std::collections::HashMap;

pub(crate) fn retain_default_span(
    default: Option<ExprId>,
    expr_spans: &[Span],
    retained: &mut HashMap<ExprId, Span>,
) {
    let Some(default) = default else {
        return;
    };
    if let Some(&span) = expr_spans.get(default.0 as usize) {
        retained.insert(default, span);
    }
}

pub(crate) fn retain_param_default_spans(
    params: &[Param],
    expr_spans: &[Span],
    retained: &mut HashMap<ExprId, Span>,
) {
    for param in params {
        retain_default_span(param.default, expr_spans, retained);
    }
}

pub(crate) fn retain_class_default_spans(
    class: &ClassDecl,
    expr_spans: &[Span],
    retained: &mut HashMap<ExprId, Span>,
) {
    for param in &class.props {
        retain_default_span(param.default, expr_spans, retained);
    }
    for function in &class.methods {
        retain_param_default_spans(&function.params, expr_spans, retained);
    }
    for constructor in &class.secondary_ctors {
        retain_param_default_spans(&constructor.params, expr_spans, retained);
    }
}

//! Source-shaped callable rendering for diagnostics.
//!
//! These displays preserve declaration spelling while semantic selection remains identity-based.
//! Conflict collection also needs a strict byte budget, so rendering owns its bounded writer rather
//! than allocating an unbounded string and truncating a possibly multi-byte source spelling.

use std::fmt::{self, Write};

use crate::ast::{Expr, File, FunBody, FunDecl, TypeRef};
use crate::types::Ty;

use super::{
    source_type_resolution::{function_type_ref_shape, FunctionTypeRefShape},
    streamed_callable_header_by_declaration,
};

pub(super) struct BoundedSourceDisplay {
    text: String,
    max_bytes: usize,
}

impl BoundedSourceDisplay {
    pub(super) fn new(max_bytes: usize) -> Self {
        Self {
            text: String::with_capacity(max_bytes.min(256)),
            max_bytes,
        }
    }

    pub(super) fn finish(self) -> String {
        self.text
    }
}

impl fmt::Write for BoundedSourceDisplay {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self
            .text
            .len()
            .checked_add(text.len())
            .is_none_or(|len| len > self.max_bytes)
        {
            return Err(fmt::Error);
        }
        self.text.push_str(text);
        Ok(())
    }
}

pub(super) fn write_source_type_leaf(out: &mut BoundedSourceDisplay, ty: &TypeRef) -> fmt::Result {
    for (index, segment) in ty.name.split('/').enumerate() {
        if index > 0 {
            out.write_str(".")?;
        }
        out.write_str(segment)?;
    }
    Ok(())
}

pub(super) fn write_source_type_display_with(
    out: &mut BoundedSourceDisplay,
    ty: &TypeRef,
    write_leaf: &mut dyn FnMut(&mut BoundedSourceDisplay, &TypeRef) -> fmt::Result,
) -> fmt::Result {
    if ty.is_star_projection() {
        return out.write_str("*");
    }
    if ty.in_projection() {
        out.write_str("in ")?;
    } else if ty.out_projection() {
        out.write_str("out ")?;
    }
    if let Some(shape) = function_type_ref_shape(ty) {
        write_function_type_shape_with(out, shape, write_leaf)?;
    } else {
        write_leaf(out, ty)?;
        if !ty.targs.is_empty() {
            out.write_str("<")?;
            write_source_type_list_with(out, &ty.targs, write_leaf)?;
            out.write_str(">")?;
        } else if let Some(arg) = ty.arg.as_deref() {
            out.write_str("<")?;
            write_source_type_display_with(out, arg, write_leaf)?;
            out.write_str(">")?;
        }
    }
    if ty.nullable() {
        out.write_str("?")?;
    }
    Ok(())
}

pub(super) fn write_function_type_shape_with(
    out: &mut BoundedSourceDisplay,
    shape: FunctionTypeRefShape<'_>,
    write_leaf: &mut dyn FnMut(&mut BoundedSourceDisplay, &TypeRef) -> fmt::Result,
) -> fmt::Result {
    let (receiver, params) = if shape.has_receiver && shape.params.len() > shape.context_count {
        (
            Some(&shape.params[shape.context_count]),
            &shape.params[shape.context_count + 1..],
        )
    } else {
        (None, &shape.params[shape.context_count..])
    };
    if shape.suspend {
        out.write_str("suspend ")?;
    }
    if shape.context_count > 0 {
        out.write_str("context(")?;
        write_source_type_list_with(out, &shape.params[..shape.context_count], write_leaf)?;
        out.write_str(") ")?;
    }
    if let Some(receiver) = receiver {
        write_source_type_display_with(out, receiver, write_leaf)?;
        out.write_str(".")?;
    }
    out.write_str("(")?;
    write_source_type_list_with(out, params, write_leaf)?;
    out.write_str(") -> ")?;
    if let Some(ret) = shape.ret {
        write_source_type_display_with(out, ret, write_leaf)?;
    } else {
        out.write_str("Unit")?;
    }
    Ok(())
}

fn write_source_type_display(out: &mut BoundedSourceDisplay, ty: &TypeRef) -> fmt::Result {
    write_source_type_display_with(out, ty, &mut write_source_type_leaf)
}

pub(super) fn write_source_type_list_with(
    out: &mut BoundedSourceDisplay,
    types: &[TypeRef],
    write_leaf: &mut dyn FnMut(&mut BoundedSourceDisplay, &TypeRef) -> fmt::Result,
) -> fmt::Result {
    for (index, ty) in types.iter().enumerate() {
        if index > 0 {
            out.write_str(", ")?;
        }
        write_source_type_display_with(out, ty, write_leaf)?;
    }
    Ok(())
}

pub(super) fn source_function_display(file: &File, function: &FunDecl, resolved_ret: Ty) -> String {
    render_source_function_display(file, function, Some(resolved_ret), usize::MAX)
        .expect("unbounded source display")
}

pub(super) fn source_function_conflict_display(
    file: &File,
    function: &FunDecl,
    max_bytes: usize,
) -> Option<String> {
    render_source_function_display(file, function, None, max_bytes)
}

pub(super) fn streamed_function_conflict_display(
    headers: &crate::fir::StreamedHeaderModule,
    declaration: crate::fir::DeclarationId,
    max_bytes: usize,
) -> Option<String> {
    let stub = headers.stub(declaration)?;
    let header = streamed_callable_header_by_declaration(headers, declaration)?;
    let name = headers.lookup_names.get(stub.lookup_name?)?;
    let mut bounds = header.bounds.iter().peekable();
    let type_parameters = header
        .type_parameters
        .iter()
        .zip(&header.type_parameter_flags)
        .map(|(parameter, flags)| FunctionTypeParameterDisplay {
            name: parameter,
            reified: flags.is_reified(),
            non_null: flags.is_non_null(),
            bound: match bounds.peek() {
                Some((bound_name, _)) if bound_name == parameter => {
                    bounds.next().map(|(_, bound)| bound)
                }
                _ => None,
            },
        })
        .collect::<Vec<_>>();
    let parameters = header
        .parameters
        .iter()
        .map(|parameter| FunctionValueParameterDisplay {
            name: &parameter.name,
            ty: &parameter.ty,
            vararg: parameter.is_vararg,
            default: parameter.has_default,
        })
        .collect::<Vec<_>>();
    render_function_display(
        header.context_count,
        stub.flags.has(crate::fir::DeclarationFlags::SUSPEND),
        &type_parameters,
        header.receiver.as_ref(),
        name,
        &parameters,
        header.explicit_result.as_ref(),
        None,
        max_bytes,
    )
}

fn render_source_function_display(
    file: &File,
    function: &FunDecl,
    resolved_ret: Option<Ty>,
    max_bytes: usize,
) -> Option<String> {
    let mut bounds = function.type_param_bounds.iter().peekable();
    let type_parameters = function
        .type_params
        .iter()
        .map(|parameter| FunctionTypeParameterDisplay {
            name: parameter,
            reified: function.reified_type_params.contains(parameter),
            non_null: function.non_null_type_params.contains(parameter),
            bound: match bounds.peek() {
                Some((bound_name, _)) if bound_name == parameter => {
                    bounds.next().map(|(_, bound)| bound)
                }
                _ => None,
            },
        })
        .collect::<Vec<_>>();
    let parameters = function
        .params
        .iter()
        .map(|parameter| FunctionValueParameterDisplay {
            name: &parameter.name,
            ty: &parameter.ty,
            vararg: parameter.is_vararg,
            default: parameter.default.is_some(),
        })
        .collect::<Vec<_>>();
    let source_ret = function.ret.as_ref().or_else(|| {
        let FunBody::Expr(body) = function.body else {
            return None;
        };
        // Conflict keys already use the finalized semantic signature. This bounded source display
        // may be assembled after Pass 1 has compacted the ordinary body arena, so the optional
        // spelling refinement must never reopen or index that released syntax.
        let Expr::Name(name) = file.expr_arena.get(body.0 as usize)? else {
            return None;
        };
        function
            .params
            .iter()
            .find(|parameter| parameter.name == *name)
            .map(|parameter| &parameter.ty)
    });
    render_function_display(
        function.context_count,
        function.is_suspend(),
        &type_parameters,
        function.receiver.as_ref(),
        &function.name,
        &parameters,
        source_ret,
        resolved_ret,
        max_bytes,
    )
}

struct FunctionTypeParameterDisplay<'a> {
    name: &'a str,
    reified: bool,
    non_null: bool,
    bound: Option<&'a TypeRef>,
}

struct FunctionValueParameterDisplay<'a> {
    name: &'a str,
    ty: &'a TypeRef,
    vararg: bool,
    default: bool,
}

#[allow(clippy::too_many_arguments)]
fn render_function_display(
    context_count: usize,
    suspend: bool,
    type_parameters: &[FunctionTypeParameterDisplay<'_>],
    receiver: Option<&TypeRef>,
    name: &str,
    parameters: &[FunctionValueParameterDisplay<'_>],
    source_result: Option<&TypeRef>,
    resolved_result: Option<Ty>,
    max_bytes: usize,
) -> Option<String> {
    let mut out = BoundedSourceDisplay::new(max_bytes);
    let context_count = context_count.min(parameters.len());
    if context_count > 0 {
        out.write_str("context(").ok()?;
        write_function_display_params(&mut out, &parameters[..context_count]).ok()?;
        out.write_str(") ").ok()?;
    }
    if suspend {
        out.write_str("suspend ").ok()?;
    }
    out.write_str("fun ").ok()?;
    if !type_parameters.is_empty() {
        out.write_str("<").ok()?;
        for (index, parameter) in type_parameters.iter().enumerate() {
            if index > 0 {
                out.write_str(", ").ok()?;
            }
            if parameter.reified {
                out.write_str("reified ").ok()?;
            }
            out.write_str(parameter.name).ok()?;
            if let Some(bound) = parameter.bound {
                out.write_str(" : ").ok()?;
                write_source_type_display(&mut out, bound).ok()?;
            } else if parameter.non_null {
                out.write_str(" : Any").ok()?;
            }
        }
        out.write_str("> ").ok()?;
    }
    if let Some(receiver) = receiver {
        write_source_type_display(&mut out, receiver).ok()?;
        out.write_str(".").ok()?;
    }
    out.write_str(name).ok()?;
    out.write_str("(").ok()?;
    write_function_display_params(&mut out, &parameters[context_count..]).ok()?;
    out.write_str(")").ok()?;
    if let Some(result) = source_result {
        out.write_str(": ").ok()?;
        write_source_type_display(&mut out, result).ok()?;
    } else if let Some(result) = resolved_result {
        out.write_str(": ").ok()?;
        out.write_str(&result.source_name()).ok()?;
    }
    Some(out.finish())
}

fn write_function_display_params(
    out: &mut BoundedSourceDisplay,
    parameters: &[FunctionValueParameterDisplay<'_>],
) -> fmt::Result {
    for (index, parameter) in parameters.iter().enumerate() {
        if index > 0 {
            out.write_str(", ")?;
        }
        if parameter.vararg {
            out.write_str("vararg ")?;
        }
        out.write_str(parameter.name)?;
        out.write_str(": ")?;
        write_source_type_display(out, parameter.ty)?;
        if parameter.default {
            out.write_str(" = ...")?;
        }
    }
    Ok(())
}

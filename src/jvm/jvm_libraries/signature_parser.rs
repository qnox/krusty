//! JVM generic-signature syntax, retaining every written class segment until construction.

use super::{declared_desc_to_ty, java_type_nullability};
use crate::jvm::classpath::kotlin_type_name_to_ty;
use crate::jvm::jvm_class_map::to_kotlin_type_name;
use crate::types::{type_name, Ty, TypeName};

/// One written segment before a declaration-owned builder combines captured formals.
/// `None` means omitted arguments, not an explicit empty instantiation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ClassSegment {
    pub(super) identity: TypeName,
    pub(super) arguments: Option<Vec<Ty>>,
}

pub(super) struct ClassApplication {
    pub(super) ty: Ty,
    pub(super) discarded_outer_arguments: bool,
}

/// Standalone parsing preserves its existing innermost-only interpretation. A provider that
/// authenticates complete layouts instead consumes every segment through `parse_type_with`.
pub(super) fn ordinary_application(segments: &[ClassSegment]) -> Option<ClassApplication> {
    let last = segments.last()?;
    let identity = to_kotlin_type_name(last.identity);
    let ty = match last.arguments.as_deref() {
        Some(arguments) => Ty::obj_args_name(identity, arguments),
        None => kotlin_type_name_to_ty(identity),
    };
    Some(ClassApplication {
        ty,
        discarded_outer_arguments: segments[..segments.len() - 1]
            .iter()
            .any(|segment| segment.arguments.is_some()),
    })
}

pub(super) struct ParsedGsig<'a> {
    pub(super) ty: Ty,
    pub(super) erasure: Option<String>,
    pub(super) has_free: bool,
    pub(super) field_inexact: bool,
    pub(super) rest: &'a str,
}

pub(super) fn parse_type_with<'a>(
    s: &'a str,
    for_field: bool,
    build: &impl Fn(&[ClassSegment]) -> Option<ClassApplication>,
) -> Option<ParsedGsig<'a>> {
    let b = s.as_bytes();
    match *b.first()? {
        b'T' => {
            let end = s.find(';')?;
            (end > 1).then(|| ParsedGsig {
                ty: Ty::ty_param(&s[1..end], Ty::obj("kotlin/Any")),
                erasure: for_field.then(|| "Ljava/lang/Object;".to_string()),
                has_free: true,
                field_inexact: false,
                rest: &s[end + 1..],
            })
        }
        b'[' => {
            if s[1..].trim_start_matches('[').starts_with('V') {
                return None;
            }
            let parsed = parse_type_with(&s[1..], for_field, build)?;
            Some(ParsedGsig {
                ty: Ty::array(parsed.ty),
                erasure: parsed.erasure.map(|erased| format!("[{erased}")),
                has_free: parsed.has_free,
                field_inexact: parsed.field_inexact,
                rest: parsed.rest,
            })
        }
        b'L' => {
            let mut rest = &s[1..];
            let end = rest.find(['<', '.', ';'])?;
            let first_component = &rest[..end];
            if !valid_gsig_class_component(first_component, true) {
                return None;
            }
            rest = &rest[end..];

            let (arguments, mut has_free, mut field_inexact, tail) =
                parse_gsig_type_args(rest, for_field, build)?;
            rest = tail;
            let mut binary_name = first_component.to_owned();
            let mut identity = type_name(first_component);
            let mut segments = vec![ClassSegment {
                identity,
                arguments,
            }];
            while let Some(suffix) = rest.strip_prefix('.') {
                let end = suffix.find(['<', '.', ';'])?;
                let component = &suffix[..end];
                if !valid_gsig_class_component(component, false) {
                    return None;
                }
                binary_name.push('$');
                binary_name.push_str(component);
                identity = identity.nested_child(component);
                rest = &suffix[end..];
                let (arguments, suffix_has_free, suffix_inexact, tail) =
                    parse_gsig_type_args(rest, for_field, build)?;
                has_free |= suffix_has_free;
                field_inexact |= suffix_inexact;
                segments.push(ClassSegment {
                    identity,
                    arguments,
                });
                rest = tail;
            }
            let after = rest.strip_prefix(';')?;
            let application = build(&segments)?;
            field_inexact |= application.discarded_outer_arguments;
            Some(ParsedGsig {
                ty: application.ty,
                erasure: for_field.then(|| format!("L{binary_name};")),
                has_free,
                field_inexact,
                rest: after,
            })
        }
        c => {
            let t = match c {
                b'I' | b'B' | b'S' => declared_desc_to_ty(&s[..1]),
                b'J' => Ty::Long,
                b'F' => Ty::Float,
                b'D' => Ty::Double,
                b'Z' => Ty::Boolean,
                b'C' => Ty::Char,
                b'V' => Ty::Unit,
                _ => return None,
            };
            Some(ParsedGsig {
                ty: t,
                erasure: for_field.then(|| (c as char).to_string()),
                has_free: false,
                field_inexact: false,
                rest: &s[1..],
            })
        }
    }
}

fn valid_gsig_class_component(component: &str, allow_package: bool) -> bool {
    if component.is_empty() || component.contains(['[', '>', ':']) {
        return false;
    }
    if allow_package {
        !component.starts_with('/') && !component.ends_with('/') && !component.contains("//")
    } else {
        !component.contains('/')
    }
}

type ParsedArguments<'a> = (Option<Vec<Ty>>, bool, bool, &'a str);

fn parse_gsig_type_args<'a>(
    s: &'a str,
    for_field: bool,
    build: &impl Fn(&[ClassSegment]) -> Option<ClassApplication>,
) -> Option<ParsedArguments<'a>> {
    let Some(mut rest) = s.strip_prefix('<') else {
        return Some((None, false, false, s));
    };
    if rest.starts_with('>') {
        return None;
    }
    let mut args = Vec::new();
    let mut has_free = false;
    let mut field_inexact = false;
    while !rest.starts_with('>') {
        if let Some(tail) = rest.strip_prefix('*') {
            args.push(Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any"))));
            field_inexact = true;
            rest = tail;
            continue;
        }
        let (arg, variance) = if let Some(arg) = rest.strip_prefix('+') {
            (arg, Some(crate::types::TypeVariance::Out))
        } else if let Some(arg) = rest.strip_prefix('-') {
            (arg, Some(crate::types::TypeVariance::In))
        } else {
            (rest, None)
        };
        if !matches!(arg.as_bytes().first(), Some(b'L' | b'T' | b'[')) {
            return None;
        }
        let parsed = parse_type_with(arg, for_field, build)?;
        has_free |= parsed.has_free;
        field_inexact |= variance.is_some() || parsed.field_inexact;
        args.push(match variance {
            Some(crate::types::TypeVariance::Out) => Ty::out_projection(parsed.ty),
            Some(crate::types::TypeVariance::In) => Ty::in_projection(parsed.ty),
            Some(crate::types::TypeVariance::Invariant) | None => parsed.ty,
        });
        rest = parsed.rest;
    }
    Some((Some(args), has_free, field_inexact, rest.strip_prefix('>')?))
}

pub(super) fn parse_formals(s: &str) -> (Vec<String>, Vec<Vec<Ty>>, &str) {
    parse_formals_with(s, &ordinary_application)
}

pub(super) fn parse_formals_with<'a>(
    s: &'a str,
    build: &impl Fn(&[ClassSegment]) -> Option<ClassApplication>,
) -> (Vec<String>, Vec<Vec<Ty>>, &'a str) {
    let Some(rest) = s.strip_prefix('<') else {
        return (Vec::new(), Vec::new(), s);
    };
    let original = s;
    let mut rest = rest;
    let mut formals = Vec::new();
    let mut formal_bounds = Vec::new();
    while !rest.starts_with('>') {
        let Some(colon) = rest.find(':') else {
            return (Vec::new(), Vec::new(), original);
        };
        if colon == 0 {
            return (Vec::new(), Vec::new(), original);
        }
        formals.push(rest[..colon].to_string());
        rest = &rest[colon + 1..];
        let mut bounds = Vec::new();
        // A missing class bound is followed by one or more interface bounds.
        if !rest.starts_with(':') {
            let Some(parsed) = parse_type_with(rest, false, build) else {
                return (Vec::new(), Vec::new(), original);
            };
            bounds.push(java_type_nullability(parsed.ty, None));
            rest = parsed.rest;
        }
        while let Some(tail) = rest.strip_prefix(':') {
            let Some(parsed) = parse_type_with(tail, false, build) else {
                return (Vec::new(), Vec::new(), original);
            };
            bounds.push(java_type_nullability(parsed.ty, None));
            rest = parsed.rest;
        }
        formal_bounds.push(bounds);
    }
    (formals, formal_bounds, &rest[1..])
}

#[cfg(test)]
mod tests;

//! Type-parameter declarations and `where` constraints.

use super::{Parser, TokenKind};
use crate::ast::{AnnotatedTypeParameter, TypeRef};

/// Where kotlinc anchors `CYCLIC_GENERIC_UPPER_BOUND`: a classifier reports each bound that closes
/// a cycle, a callable reports each type parameter that sits on one.
#[derive(Clone, Copy)]
pub(super) enum CyclicBoundAnchor {
    Bound,
    TypeParameter,
}

/// The type parameter a bound names directly (`T`, `T?`), the only bound shape that can make a
/// cycle: a bound that merely mentions a parameter (`List<T>`) is a legal recursive bound.
fn directly_named_parameter<'a>(bound: &'a TypeRef, type_params: &[String]) -> Option<&'a str> {
    let direct = bound.targs.is_empty()
        && bound.arg.is_none()
        && bound.fun_params.is_empty()
        && bound.qualifier_arguments.is_empty()
        && !bound.definitely_non_null();
    (direct && type_params.contains(&bound.name)).then_some(bound.name.as_str())
}

impl Parser<'_> {
    /// Report `CYCLIC_GENERIC_UPPER_BOUND` for the type parameters of one declaration, whose
    /// inline and `where` bounds are in `bounds`, and drop the bounds that close a cycle so no
    /// later phase walks an infinite chain of bounds.
    pub(super) fn report_cyclic_type_parameter_bounds(
        &mut self,
        type_params: &[String],
        bounds: &mut Vec<(String, TypeRef)>,
        anchor: CyclicBoundAnchor,
    ) {
        let count = type_params.len();
        let position = |name: &str| type_params.iter().position(|declared| declared == name);
        let edges = bounds
            .iter()
            .map(|(owner, bound)| {
                Some((
                    position(owner)?,
                    position(directly_named_parameter(bound, type_params)?)?,
                ))
            })
            .collect::<Vec<_>>();
        let mut reach = vec![vec![false; count]; count];
        for (from, to) in edges.iter().flatten() {
            reach[*from][*to] = true;
        }
        for via in 0..count {
            let through = reach[via].clone();
            for row in reach.iter_mut().filter(|row| row[via]) {
                for (slot, reaches) in row.iter_mut().zip(&through) {
                    *slot |= *reaches;
                }
            }
        }
        let cycle_names = |member: usize| {
            (0..count)
                .filter(|other| reach[member][*other] && reach[*other][member])
                .map(|other| type_params[other].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let message = |member: usize| {
            format!(
                "type parameter has cyclic upper bounds: {}.",
                cycle_names(member)
            )
        };
        let closes_cycle =
            |edge: &Option<(usize, usize)>| edge.is_some_and(|(from, to)| reach[to][from]);
        match anchor {
            CyclicBoundAnchor::Bound => {
                for ((_, bound), edge) in bounds.iter().zip(&edges) {
                    if let Some((from, _)) = edge.filter(|_| closes_cycle(edge)) {
                        self.diags.error_named(
                            bound.span,
                            crate::diag::names::CYCLIC_GENERIC_UPPER_BOUND,
                            message(from),
                        );
                    }
                }
            }
            CyclicBoundAnchor::TypeParameter => {
                for member in (0..count).filter(|member| reach[*member][*member]) {
                    let span = self
                        .type_parameter_spans
                        .iter()
                        .find(|(declared, _)| *declared == type_params[member])
                        .map(|(_, span)| *span);
                    if let Some(span) = span {
                        self.diags.error_named(
                            span,
                            crate::diag::names::CYCLIC_GENERIC_UPPER_BOUND,
                            message(member),
                        );
                    }
                }
            }
        }
        let mut edges = edges.iter();
        bounds.retain(|_| !closes_cycle(edges.next().expect("one edge per bound")));
    }

    /// Parse an optional `where T : Bound, U : Bound2` clause and retain each bound's owner.
    pub(super) fn parse_where_clause(
        &mut self,
        declared_type_params: &[String],
        declaration_name: &str,
    ) -> Vec<(String, TypeRef)> {
        let mut bounds = Vec::new();
        let save = self.i;
        self.skip_semicolons();
        if !(self.at(TokenKind::Ident) && self.keyword_text("where")) {
            self.i = save;
            return bounds;
        }
        self.bump();
        loop {
            self.skip_semicolons();
            let mut name = String::new();
            let name_span = self.tok().span;
            if self.at(TokenKind::Ident) {
                name = self.text().to_string();
                self.bump();
            }
            if !name.is_empty()
                && !declared_type_params
                    .iter()
                    .any(|declared| declared == &name)
            {
                self.diags.error(
                    name_span,
                    format!("'{name}' does not refer to a type parameter of '{declaration_name}'."),
                );
            }
            if self.eat(TokenKind::Colon) {
                let bound = self.parse_type();
                if let Some((_, parameter)) = self
                    .type_parameter_spans
                    .iter()
                    .find(|(declared, _)| *declared == name)
                {
                    self.file
                        .type_parameter_bound_owners
                        .insert(bound.span.lo, *parameter);
                }
                if !name.is_empty() {
                    bounds.push((name.clone(), bound));
                }
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        bounds
    }

    /// Parse `<T, reified U : Bound, out V>` and preserve semantic bounds plus source ownership.
    #[allow(clippy::type_complexity)]
    pub(super) fn parse_type_params(
        &mut self,
        declaration_start: u32,
    ) -> (
        Vec<String>,
        crate::fx_hash::HashSet<String>,
        crate::fx_hash::HashSet<String>,
        Vec<(String, TypeRef)>,
        Vec<crate::types::TypeVariance>,
    ) {
        let mut names = Vec::new();
        let mut non_null = crate::fx_hash::HashSet::default();
        let mut reified = crate::fx_hash::HashSet::default();
        let mut bounds = Vec::new();
        let mut variances = Vec::new();
        self.type_parameter_spans.clear();
        self.type_parameter_bound_spans.clear();
        if !self.eat(TokenKind::Lt) {
            return (names, non_null, reified, bounds, variances);
        }
        loop {
            let parameter_start = self.tok().span.lo;
            let mut is_reified = false;
            let mut variance = crate::types::TypeVariance::Invariant;
            let mut annotations = Vec::new();
            let mut annotation_args = Vec::new();
            loop {
                if self.at(TokenKind::At) {
                    let (annotation, args) = self.parse_annotation();
                    if let Some(annotation) = annotation {
                        annotations.push(annotation);
                        annotation_args.push(args);
                    }
                } else if self.at(TokenKind::Ident) && self.keyword_text("reified") {
                    is_reified = true;
                    self.bump();
                } else if self.at(TokenKind::Ident) && self.keyword_text("out") {
                    variance = crate::types::TypeVariance::Out;
                    self.bump();
                } else if self.at(TokenKind::KwIn) {
                    variance = crate::types::TypeVariance::In;
                    self.bump();
                } else {
                    break;
                }
            }
            let (name, name_span) = if self.at(TokenKind::Ident) {
                let span = self.tok().span;
                let name = self.text().to_string();
                self.bump();
                (name, span)
            } else {
                (String::new(), self.tok().span)
            };
            if !name.is_empty() {
                names.push(name.clone());
                variances.push(variance);
                if is_reified {
                    reified.insert(name.clone());
                }
                if !annotations.is_empty() {
                    self.file
                        .declaration_type_parameter_annotations
                        .entry(declaration_start)
                        .or_default()
                        .push(AnnotatedTypeParameter {
                            name: name.clone(),
                            span: name_span,
                            annotations,
                            annotation_args,
                        });
                }
            }
            let mut inline_bound = None;
            if self.eat(TokenKind::Colon) {
                let bound_start = self.tok().span.lo;
                let bound = self.parse_type();
                self.type_parameter_bound_spans
                    .push(crate::diag::Span::new(bound_start, bound.span.hi));
                if bound.name == "Any" && !bound.nullable() && !name.is_empty() {
                    non_null.insert(name.clone());
                }
                inline_bound = Some(bound.span);
                if !name.is_empty() {
                    bounds.push((name.clone(), bound));
                }
            }
            if !name.is_empty() {
                let end = inline_bound.map_or(name_span.hi, |bound| bound.hi);
                let span = crate::diag::Span::new(parameter_start, end);
                if let Some(bound) = inline_bound {
                    self.file.type_parameter_bound_owners.insert(bound.lo, span);
                }
                self.type_parameter_spans.push((name.clone(), span));
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::Gt, "'>'");
        (names, non_null, reified, bounds, variances)
    }
}

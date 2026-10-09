//! Type-parameter declarations and `where` constraints.

use super::{Parser, TokenKind};
use crate::ast::{AnnotatedTypeParameter, TypeRef};
use crate::diag::Span;

/// One declaration's type-parameter list as written; empty when the declaration has none.
#[derive(Default)]
pub(super) struct TypeParameterList {
    pub(super) names: Vec<String>,
    /// The parameters bounded by a non-null `Any` (`T : Any`).
    pub(super) non_null: std::collections::HashSet<String>,
    /// The `reified` parameters, which an `inline` function may use concretely (`is T`, `as T`,
    /// `T::class`) and which codegen specializes per call site.
    pub(super) reified: std::collections::HashSet<String>,
    pub(super) bounds: Vec<(String, TypeRef)>,
    pub(super) variances: Vec<crate::types::TypeVariance>,
}

impl Parser<'_> {
    /// Parse an optional `where T : Bound, U : Bound2` clause and retain each bound's owner.
    pub(super) fn parse_where_clause(
        &mut self,
        declared_type_params: &[String],
        declaration_name: &str,
    ) -> Vec<(String, TypeRef)> {
        let mut bounds = Vec::new();
        let save = self.i;
        self.skip_newlines();
        if !(self.at(TokenKind::Ident) && self.keyword_text("where")) {
            self.i = save;
            return bounds;
        }
        self.bump();
        loop {
            self.skip_newlines();
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

    /// Parse the `<T, reified U : Bound, out V>` type-parameter list of the declaration starting at
    /// `declaration_start`, if one follows, preserving semantic bounds plus source ownership; an
    /// absent list is empty. The list's own span is recorded under the same key, for diagnostics
    /// about the list as a whole.
    pub(super) fn parse_type_params(&mut self, declaration_start: u32) -> TypeParameterList {
        let mut list = TypeParameterList::default();
        let open = self.tok().span;
        self.type_parameter_spans.clear();
        if !self.eat(TokenKind::Lt) {
            return list;
        }
        let TypeParameterList {
            names,
            non_null,
            reified,
            bounds,
            variances,
        } = &mut list;
        loop {
            self.skip_plain_newlines();
            let parameter_start = self.tok().span.lo;
            let mut is_reified = false;
            let mut variance = crate::types::TypeVariance::Invariant;
            let mut annotations = Vec::new();
            let mut annotation_args = Vec::new();
            loop {
                self.skip_plain_newlines();
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
            self.skip_plain_newlines();
            let mut inline_bound = None;
            if self.eat(TokenKind::Colon) {
                self.skip_plain_newlines();
                let bound = self.parse_type();
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
            self.skip_plain_newlines();
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        let close = self.tok().span;
        self.expect(TokenKind::Gt, "'>'");
        self.file
            .type_parameter_lists
            .insert(declaration_start, Span::new(open.lo, close.hi));
        list
    }
}

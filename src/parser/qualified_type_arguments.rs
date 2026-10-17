//! Qualified classifier segments and their written type-argument lists.

use super::*;

impl Parser<'_> {
    pub(super) fn parse_annotation_name(&mut self) -> (String, Vec<QualifierSegmentArguments>) {
        let mut segments = Vec::new();
        let mut lists = Vec::new();
        if self.at(TokenKind::Ident) {
            segments.push(self.text().to_string());
            self.bump();
            loop {
                let segment = segments.len() as u32 - 1;
                self.parse_segment_type_arguments(segment, &mut lists);
                if !self.at(TokenKind::Dot) {
                    break;
                }
                self.bump();
                if self.at(TokenKind::Ident) {
                    segments.push(self.text().to_string());
                    self.bump();
                }
            }
        } else {
            self.parse_type_args();
        }
        let last = segments.len().saturating_sub(1) as u32;
        lists.retain(|list| list.segment != last);
        (segments.join("."), lists)
    }

    pub(super) fn parse_segment_type_arguments(
        &mut self,
        segment: u32,
        lists: &mut Vec<QualifierSegmentArguments>,
    ) -> Vec<TypeRef> {
        if !self.at(TokenKind::Lt) {
            return Vec::new();
        }
        let (arguments, span) = self.parse_type_argument_list();
        if !arguments.is_empty() {
            lists.push(QualifierSegmentArguments {
                segment,
                span,
                arguments: arguments.clone().into_boxed_slice(),
            });
        }
        arguments
    }

    pub(super) fn parse_type_args(&mut self) -> Vec<TypeRef> {
        self.parse_type_argument_list().0
    }

    pub(super) fn parse_type_argument_list(&mut self) -> (Vec<TypeRef>, Span) {
        let open = self.tok().span;
        let args = self.parse_type_argument_list_contents();
        let close = self.t[self.i.saturating_sub(1)].span;
        (args, Span::new(open.lo, close.hi.max(open.hi)))
    }

    fn parse_type_argument_list_contents(&mut self) -> Vec<TypeRef> {
        let mut args = Vec::new();
        if !self.eat(TokenKind::Lt) {
            return args;
        }
        self.skip_newlines();
        while !self.at(TokenKind::Gt) && !self.at(TokenKind::Eof) {
            let (in_projection, out_projection, projection_span) = self.skip_variance();
            self.skip_plain_newlines();
            if self.at(TokenKind::Star) {
                let span = self.tok().span;
                self.bump();
                args.push(TypeRef {
                    name: "Any".to_string(),
                    flags: TrFlags::default()
                        .with_nullable(true)
                        .with_definitely_non_null(false)
                        .with_star_projection(true),
                    arg: None,
                    targs: Vec::new(),
                    span,
                    fun_params: Vec::new(),
                    fun_context_count: 0,
                    qualifier_arguments: Box::new([]),
                });
            } else {
                let mut argument = self.parse_type();
                argument.set_projection(in_projection, out_projection);
                if let Some(projection_span) = projection_span {
                    self.file
                        .type_projection_spans
                        .insert(argument.span, projection_span);
                }
                args.push(argument);
            }
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::Gt, "'>'");
        args
    }
}

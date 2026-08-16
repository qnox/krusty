use crate::diag::DiagSink;
use crate::lexer::{
    lex_formatting_tokens, FormattingToken as LexicalToken, FormattingTokenKind as LexicalKind,
};
use crate::token::TokenKind as CoreKind;

const MAX_FORMATTING_INPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_FORMATTING_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_FORMATTING_TOKENS: usize = 256 * 1024;
const MAX_FORMATTING_NESTING: usize = 128;
const MAX_LAYOUT_PASSES: usize = 8;

#[derive(Clone, Copy, Default)]
pub struct FormattingOptions {
    pub tab_size: u32,
    pub insert_spaces: bool,
    pub trim_trailing_whitespace: bool,
    pub insert_final_newline: bool,
    pub trim_final_newlines: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Word,
    Literal,
    Shebang,
    LineComment,
    BlockComment,
    Newline,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Dot,
    Semicolon,
    Operator,
    AnnotationAt,
    LabelAt,
    JumpAt,
    TemplateExpressionStart,
    TemplateExpressionEnd,
}

#[derive(Clone, Copy, Debug)]
struct Token {
    kind: Kind,
    lo: u32,
    hi: u32,
    logical_lo: u32,
    logical_hi: u32,
}

#[derive(Clone, Copy, Debug)]
struct AttachedMarker {
    token_index: usize,
    lo: u32,
    hi: u32,
}

impl Token {
    fn text(self, source: &str) -> &str {
        &source[self.lo as usize..self.hi as usize]
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TopLine {
    Package,
    Import,
    TypeDeclaration,
    CallableDeclaration,
    /// Secondary constructors attach to the previous member without a blank line.
    Constructor,
    /// Type aliases attach to the previous declaration without a blank line.
    Typealias,
    Property,
    Attached,
    Closure,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BraceStyle {
    Normal,
    Lambda,
    Expanded,
}

/// Per-brace classification from `classify_braces`.
struct BraceStyles {
    styles: Vec<BraceStyle>,
    /// Braces that open a class-like body (`class`/`interface`/`object`/enum/companion),
    /// whose direct members are declarations separated by blank lines.
    class_body: Vec<bool>,
}

pub fn format_kotlin(source: &str, options: FormattingOptions) -> Option<String> {
    if source.len() > MAX_FORMATTING_INPUT_BYTES {
        return None;
    }
    let mut formatted = sort_imports(source)?;
    for _ in 0..MAX_LAYOUT_PASSES {
        let next = format_once(&formatted, options)?;
        if next == formatted {
            return Some(next);
        }
        formatted = next;
    }
    None
}

/// ktlint `import-ordering` rule (`ktlint_official`): an import section is sorted in plain
/// ASCII order with aliased imports placed after all non-aliased ones; blank lines inside
/// the section are dropped. A comment line anywhere inside the section makes ktlint leave
/// it untouched. Runs as a token-accurate pre-pass — comments and string contents are
/// never mistaken for imports — so the layout engine and its token guard always see the
/// sorted stream.
fn sort_imports(source: &str) -> Option<String> {
    let scan = formatting_tokens(source)?;
    let tokens = &scan.tokens;
    // Import line spans: (line_start, content_end, line_end, has_alias); line_end includes
    // the newline when the line has one.
    let mut lines: Vec<(usize, usize, usize, bool)> = Vec::new();
    let mut brace_depth = 0usize;
    let mut line_start = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        match token.kind {
            Kind::Newline => line_start = token.hi as usize,
            Kind::LBrace => brace_depth += 1,
            Kind::RBrace => brace_depth = brace_depth.saturating_sub(1),
            Kind::Word if token.text(source) == "import" && brace_depth == 0 => {
                let at_line_start = index == 0 || tokens[index - 1].kind == Kind::Newline;
                let at_column_zero = token.lo as usize == line_start;
                if !at_line_start || !at_column_zero {
                    continue;
                }
                let mut content_end = token.hi as usize;
                let mut end = content_end;
                let mut has_alias = false;
                for following in tokens[index + 1..].iter() {
                    if following.kind == Kind::Newline {
                        end = following.hi as usize;
                        break;
                    }
                    if !matches!(following.kind, Kind::LineComment | Kind::Semicolon) {
                        content_end = following.hi as usize;
                        if following.kind == Kind::Word && following.text(source) == "as" {
                            has_alias = true;
                        }
                    }
                    end = following.hi as usize;
                }
                lines.push((token.lo as usize, content_end, end, has_alias));
            }
            _ => {}
        }
    }
    // Sections: import lines separated only by blank lines belong to one section; anything
    // else (code, a comment line) splits sections.
    let mut sections: Vec<std::ops::Range<usize>> = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        let section_start = index;
        while let Some(next) = lines.get(index + 1) {
            let gap = &source[lines[index].2..next.0];
            if gap
                .bytes()
                .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
            {
                index += 1;
            } else {
                break;
            }
        }
        sections.push(section_start..index + 1);
        index += 1;
    }
    // A comment line anywhere in the import region makes ktlint leave every section
    // untouched, not just the one containing the comment.
    if let (Some(first), Some(last)) = (lines.first(), lines.last()) {
        let commented = tokens.iter().any(|token| {
            matches!(token.kind, Kind::LineComment | Kind::BlockComment)
                && token.lo as usize > first.0
                && (token.lo as usize) < last.2
        });
        if commented {
            return Some(source.to_string());
        }
    }
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0usize;
    for section in sections {
        let run = &lines[section];
        if run.len() == 1 {
            continue;
        }
        output.push_str(&source[cursor..run[0].0]);
        let mut sorted: Vec<&(usize, usize, usize, bool)> = run.iter().collect();
        sorted.sort_by_key(|line| (line.3, &source[line.0..line.1]));
        let last = sorted.len() - 1;
        for (position, line) in sorted.iter().enumerate() {
            output.push_str(&source[line.0..line.2]);
            if !source[line.0..line.2].ends_with('\n') && position != last {
                output.push('\n');
            }
        }
        cursor = run[run.len() - 1].2;
    }
    if cursor == 0 {
        return Some(source.to_string());
    }
    output.push_str(&source[cursor..]);
    Some(output)
}

fn format_once(source: &str, options: FormattingOptions) -> Option<String> {
    let source_scan = formatting_tokens(source)?;
    let tokens = &source_scan.tokens;
    let indent = indentation(options)?;
    let line_ending = source_line_ending(source, tokens);
    let previous_significant = previous_significant_tokens(tokens);
    let next_significant = next_significant_tokens(tokens);
    let generic_angles = generic_angle_roles(tokens, source, &next_significant);
    let enum_entries = enum_entries(tokens, source, &next_significant, &generic_angles);
    let (token_brace_styles, class_body_braces) = {
        let mut classified = classify_braces(tokens, source);
        // Enum bodies without member declarations keep the layout they were written in.
        for (index, keep) in enum_entries.keep_inline.iter().enumerate() {
            if *keep {
                classified.styles[index] = BraceStyle::Lambda;
            }
        }
        // An enum entry body is an anonymous class body and expands like one.
        for (index, force) in enum_entries.force_expanded.iter().enumerate() {
            if *force {
                classified.styles[index] = BraceStyle::Expanded;
            }
        }
        (classified.styles, classified.class_body)
    };
    let expanded_parens = classify_expanded_parens(tokens, &generic_angles);
    let signature_wrapping =
        signature_wrapping(tokens, source, &previous_significant, &generic_angles);
    let mut expanded_parens = expanded_parens;
    for (index, marked) in signature_wrapping.parens.iter().enumerate() {
        if *marked {
            expanded_parens[index] = true;
        }
    }
    // ktlint `parameter-list-wrapping`/`trailing-comma-on-call-site` (`ktlint_official`):
    // a multiline call argument list expands, breaks after each argument comma, and gets
    // a trailing comma before its `)` drops to its own line. The same applies to a
    // multiline type-argument list before its closing `>`.
    let call_site_wrapping =
        multiline_call_parens(tokens, source, &previous_significant, &generic_angles);
    let mut trailing_comma_rparens = signature_wrapping.trailing_rparens.clone();
    for (index, marked) in call_site_wrapping.parens.iter().enumerate() {
        if *marked {
            expanded_parens[index] = true;
            if tokens[index].kind == Kind::RParen {
                trailing_comma_rparens[index] = true;
            }
        }
    }
    let multiline_angles = multiline_angle_brackets(tokens, &generic_angles);
    let wide_colons = wide_colons(tokens, source, &previous_significant, &generic_angles);
    let expression_wraps = expression_wraps(
        tokens,
        source,
        &next_significant,
        &previous_significant,
        &token_brace_styles,
        &generic_angles,
        &signature_wrapping.parens,
    );
    let dropped_semicolons = redundant_semicolons(tokens, source);
    let dropped_braces = empty_classifier_body_braces(tokens, source);
    let dropped_parens = empty_primary_constructor_parens(tokens, source);
    let expression_bodies = expression_bodies(tokens, source, &previous_significant);
    let when_bracing = when_entry_bracing(tokens, source, &next_significant);
    let elvis_wraps = elvis_wraps(tokens, source);
    let script_style = tokens
        .first()
        .is_some_and(|token| token.kind == Kind::Shebang);
    let mut output = String::with_capacity(source.len().min(MAX_FORMATTING_OUTPUT_BYTES));
    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut generic_depth = 0usize;
    let mut line_start = true;
    let mut previous = None;
    let mut previous_top_line = None;
    let mut brace_style_stack = Vec::new();
    // Whether each open brace is a class-like body, and the declaration-line state of the
    // enclosing level saved while inside one.
    let mut class_body_stack: Vec<bool> = Vec::new();
    let mut top_line_save: Vec<Option<TopLine>> = Vec::new();
    // The declaration-line state current when each open brace's statement began. A body
    // closing a property statement (`val o = object { ... }`) does not read as a
    // declaration boundary for blank lines; a function or class body close does.
    let mut brace_top_line: Vec<Option<TopLine>> = Vec::new();
    // ktlint indents a property accessor (`get`/`set`) one level under its property and
    // keeps that extra level through the accessor body. Each entry of brace_extra_stack
    // marks whether the matching open brace contributes that extra level.
    let mut brace_extra_stack: Vec<bool> = Vec::new();
    let mut pending_accessor = false;
    // Indent level of the line currently being emitted; the closing `"""` of a raw string
    // aligns with it when code follows on the same line.
    let mut line_level = 0usize;
    let mut prefix_marker_index = 0usize;
    let mut suffix_marker_index = 0usize;
    // Indent levels of rewritten `when` entry lines, for their inserted `}` lines, and
    // the output spans of braces the when-entry rewrite inserted (for the token guard).
    let mut entry_levels: Vec<usize> = Vec::new();
    let mut inserted_spans: Vec<(u32, u32)> = Vec::new();

    for (index, token) in tokens.iter().copied().enumerate() {
        if token.kind == Kind::Newline {
            pending_accessor = false;
            if expression_bodies.dropped[index]
                || signature_wrapping.collapsed_newlines[index]
                || when_bracing.drop_newline[index]
                || elvis_wraps.drop_newline[index]
            {
                // Dropped wrapper newlines of a converted expression body, newlines
                // inside a collapsed single-parameter signature, or the newline between
                // a rewritten when entry's arrow and its body.
                continue;
            }
            if !options.trim_trailing_whitespace {
                preserve_trailing_whitespace(&mut output, source, tokens, index)?;
            } else {
                preserve_ktlint_trailing_tab(&mut output, source, tokens, index)?;
            }
            push_markers(
                &mut output,
                source,
                &source_scan.prefix_markers,
                &mut prefix_marker_index,
                index,
            )?;
            if !ends_with_blank_line(&output, line_ending) {
                push(&mut output, line_ending)?;
            }
            push_markers(
                &mut output,
                source,
                &source_scan.suffix_markers,
                &mut suffix_marker_index,
                index,
            )?;
            line_start = true;
            previous = None;
            continue;
        }

        if token.kind == Kind::Semicolon
            && (dropped_semicolons[index] || enum_entries.drop_semicolon[index])
        {
            // ktlint `no-semi`: a semicolon followed only by comments, more semicolons,
            // or the end of the statement's line is redundant and is dropped. So is the
            // entries `;` of a rewritten enum body when no members follow.
            continue;
        }
        if matches!(token.kind, Kind::LBrace | Kind::RBrace) && dropped_braces[index] {
            // ktlint `no-empty-class-body`: an empty classifier body loses its braces
            // (`class A {}` -> `class A`).
            continue;
        }
        if matches!(token.kind, Kind::LParen | Kind::RParen) && dropped_parens[index] {
            // ktlint `no-empty-primary-constructor`: `class E()` -> `class E`.
            continue;
        }
        if matches!(token.kind, Kind::LBrace | Kind::RBrace | Kind::Word)
            && expression_bodies.dropped[index]
        {
            // ktlint `function-expression-body`: `{ return x }` -> `= x`; the braces and
            // the `return` keyword are dropped.
            continue;
        }

        // ktlint `multiline-expression-wrapping`: a right-hand side of `=` that spans
        // multiple lines starts on the line after the `=`.
        if expression_wraps.starts[index] && !line_start {
            push(&mut output, line_ending)?;
            line_start = true;
            previous = None;
        }
        // ktlint's operator-leading style makes Elvis the exception to its usual
        // trailing-binary-operator layout: `a ?:\n b` becomes `a\n ?: b`.
        if elvis_wraps.relocate[index] && !line_start {
            push(&mut output, line_ending)?;
            line_start = true;
            previous = None;
        }
        // ktlint `when-entry-bracing`: a rewritten entry's inserted block closes ahead
        // of the next entry's condition or the `when`'s own `}`.
        if when_bracing.close_before[index] {
            if !line_start {
                push(&mut output, line_ending)?;
            }
            let entry_level = entry_levels.pop().unwrap_or(0);
            push_indentation(&mut output, &indent, entry_level)?;
            let brace_lo = output.len() as u32;
            push(&mut output, "}")?;
            inserted_spans.push((brace_lo, output.len() as u32));
            push(&mut output, line_ending)?;
            line_start = true;
            previous = None;
        }
        // Rewritten entries are separated by a blank line.
        if when_bracing.blank_before[index] {
            ensure_blank_line(&mut output, line_ending)?;
            line_start = true;
            previous = None;
        }
        // ktlint enum-entry formatting: the last entry gains a trailing comma before the
        // entries `;` or the body `}`.
        if enum_entries.comma_before[index]
            && !previous_significant[index].is_some_and(|prev| tokens[prev].kind == Kind::Comma)
        {
            let splice = line_start && output.ends_with(line_ending);
            if splice {
                output.truncate(output.len() - line_ending.len());
            }
            push(&mut output, ",")?;
            if splice {
                push(&mut output, line_ending)?;
            }
        }
        // Entries after a comma and the entries `;` start on a fresh line.
        if enum_entries.break_before[index] && !line_start {
            push(&mut output, line_ending)?;
            line_start = true;
            previous = None;
        }
        // A blank line separates the entries `;` from the member declarations.
        if enum_entries.blank_before[index] {
            ensure_blank_line(&mut output, line_ending)?;
            line_start = true;
            previous = None;
        }

        // ktlint curly-brace spacing: a `{` sitting alone on its line after a declaration
        // or control-flow header joins the header line (`fun f()\n{` -> `fun f() {`).
        let joins_previous_line = token.kind == Kind::LBrace
            && line_start
            && output.ends_with(line_ending)
            && previous_significant[index].is_some_and(|prev| {
                !matches!(
                    tokens[prev].kind,
                    Kind::Newline
                        | Kind::LBrace
                        | Kind::RBrace
                        | Kind::Semicolon
                        | Kind::LineComment
                        | Kind::BlockComment
                        | Kind::Shebang
                )
            });
        if joins_previous_line {
            output.truncate(output.len() - line_ending.len());
            push(&mut output, " ")?;
            line_start = false;
        }

        let mut closing_extra = 0usize;
        let mut saved_top_line = None;
        let closing_style = if token.kind == Kind::RBrace {
            let style = brace_style_stack.pop().unwrap_or(BraceStyle::Normal);
            saved_top_line = brace_top_line.pop().flatten();
            // Leaving a class body restores the enclosing level's declaration-line state.
            if class_body_stack.pop().unwrap_or(false) {
                previous_top_line = top_line_save.pop().unwrap_or(previous_top_line);
            }
            if brace_extra_stack.pop().unwrap_or(false) {
                closing_extra = 1;
            }
            // ktlint expands a block only around code: empty bodies (`fun f() {}`) and
            // comment-only bodies (`class I { /* c */ }`) stay on one line.
            let expands = previous.is_some_and(|prev: usize| {
                !matches!(
                    tokens[prev].kind,
                    Kind::LBrace | Kind::LineComment | Kind::BlockComment
                )
            });
            if style == BraceStyle::Expanded && !line_start && expands {
                push(&mut output, line_ending)?;
                line_start = true;
                previous = None;
            }
            brace_depth = brace_depth.saturating_sub(1);
            Some(style)
        } else {
            None
        };
        // ktlint `function-signature`/`class-signature`: a wrapped signature ends its
        // parameter list with a trailing comma before the closing paren drops a line.
        if token.kind == Kind::RParen
            && trailing_comma_rparens[index]
            && previous_significant[index]
                .is_some_and(|prev| !matches!(tokens[prev].kind, Kind::Comma | Kind::LParen))
        {
            // When `)` already starts its line the comma belongs at the end of the
            // previous one.
            let splice = line_start && output.ends_with(line_ending);
            if splice {
                output.truncate(output.len() - line_ending.len());
            }
            push(&mut output, ",")?;
            if splice {
                push(&mut output, line_ending)?;
            }
        }
        if token.kind == Kind::Operator
            && generic_angles[index] < 0
            && multiline_angles[index]
            && previous_significant[index].is_some_and(|prev| tokens[prev].kind != Kind::Comma)
        {
            // Same trailing comma ahead of a multiline type-argument list's `>`.
            let splice = line_start && output.ends_with(line_ending);
            if splice {
                output.truncate(output.len() - line_ending.len());
            }
            push(&mut output, ",")?;
            if splice {
                push(&mut output, line_ending)?;
            }
        }
        if token.kind == Kind::RParen && expanded_parens[index] && !line_start {
            push(&mut output, line_ending)?;
            line_start = true;
            previous = None;
        }
        if token.kind == Kind::RParen {
            if !call_site_wrapping.lambda_frames[index] {
                paren_depth = paren_depth.saturating_sub(1);
            }
        } else if token.kind == Kind::RBracket {
            bracket_depth = bracket_depth.saturating_sub(1);
        }

        let first_on_line = line_start;
        if first_on_line {
            if token.kind == Kind::Word
                && matches!(token.text(source), "get" | "set")
                && next_significant[index].is_some_and(|next| tokens[next].kind == Kind::LParen)
                && property_accessor(tokens, source, &previous_significant, index)
            {
                pending_accessor = true;
            }
            // A closing `>` dedents to the level of the line that opened the type-
            // argument list; ktlint indents generic content one level per bracket.
            let generic_level = if generic_angles[index] < 0 {
                generic_depth.saturating_sub((-generic_angles[index]) as usize)
            } else {
                generic_depth
            };
            // ktlint separates declarations with a blank line at the top level and at
            // class-body member level alike; statements inside function bodies are not
            // declarations and keep their line structure. Enum-managed break lines are
            // excluded: they split a source line, which `top_line` cannot see.
            let member_level = class_body_stack.last().copied() == Some(true);
            if paren_depth == 0
                && bracket_depth == 0
                && (brace_depth == 0 || member_level)
                && expression_wraps.extra[index] == 0
                && !enum_entries.break_before[index]
                && !enum_entries.blank_before[index]
            {
                if let Some(current) = top_line(tokens, index, source) {
                    // A comment after the import block attaches to the following
                    // declaration and is separated from the imports by a blank line —
                    // unless an import line follows it (comment inside the import
                    // section) or nothing follows at all.
                    let blank = needs_top_level_blank(previous_top_line, current, script_style)
                        || previous_top_line == Some(TopLine::Import)
                            && current == TopLine::Attached
                            && next_code_line_is_import(tokens, index, source) == Some(false);
                    if blank {
                        ensure_blank_line(&mut output, line_ending)?;
                    }
                    previous_top_line = Some(current);
                }
            }
            let level = brace_depth
                .saturating_add(paren_depth)
                .saturating_add(bracket_depth)
                .saturating_add(generic_level)
                .saturating_add(brace_extra_stack.iter().filter(|extra| **extra).count())
                .saturating_add(closing_extra)
                .saturating_add(usize::from(pending_accessor))
                .saturating_add(usize::from(when_bracing.body_extra[index]))
                .saturating_add(usize::from(expression_wraps.extra[index]))
                .saturating_add(usize::from(elvis_wraps.relocate[index]));
            line_level = level;
            push_indentation(&mut output, &indent, level)?;
            line_start = false;
        } else if let Some(previous_index) = previous {
            let previous_token = tokens[previous_index];
            if expression_bodies.starts[index] {
                // First token of a converted expression body: `{ return x }` -> `= x`.
                push(&mut output, " = ")?;
            } else if needs_space(
                tokens,
                source,
                previous_index,
                index,
                &generic_angles,
                &previous_significant,
                brace_style_stack.last().copied(),
            ) || (token.kind == Kind::Colon && wide_colons[index])
            {
                push(&mut output, " ")?;
            }
            if previous_token.kind == Kind::LineComment {
                return None;
            }
        }

        push_markers(
            &mut output,
            source,
            &source_scan.prefix_markers,
            &mut prefix_marker_index,
            index,
        )?;
        if token.kind == Kind::LineComment {
            let text = spaced_line_comment(token.text(source));
            push(&mut output, &text)?;
        } else if token.kind == Kind::Literal
            && tokens
                .get(index + 1)
                .is_some_and(|next| next.kind != Kind::Newline)
        {
            // ktlint `string-template-indent`: when code continues after the closing
            // `"""` on the same line (`.trimIndent()`), that line indents like a code
            // line. Content lines of the raw string stay untouched.
            match reindent_raw_string_close(token.text(source), &indent, line_level) {
                Some(rewritten) => push(&mut output, &rewritten)?,
                None => push(&mut output, token.text(source))?,
            }
        } else {
            push(&mut output, token.text(source))?;
        }
        push_markers(
            &mut output,
            source,
            &source_scan.suffix_markers,
            &mut suffix_marker_index,
            index,
        )?;

        if token.kind == Kind::LBrace {
            brace_style_stack.push(token_brace_styles[index]);
            brace_extra_stack.push(pending_accessor);
            brace_top_line.push(previous_top_line);
            // Entering a class body starts a fresh declaration-line context.
            class_body_stack.push(class_body_braces[index]);
            if class_body_braces[index] {
                top_line_save.push(previous_top_line);
                previous_top_line = None;
            }
            brace_depth = brace_depth.saturating_add(1);
        } else if token.kind == Kind::LParen && !call_site_wrapping.lambda_frames[index] {
            paren_depth = paren_depth.saturating_add(1);
        } else if token.kind == Kind::LBracket {
            bracket_depth = bracket_depth.saturating_add(1);
        }
        if generic_angles[index] > 0 {
            generic_depth = generic_depth.saturating_add(1);
        } else if generic_angles[index] < 0 {
            generic_depth = generic_depth.saturating_sub(1);
        }
        if token.kind == Kind::BlockComment && contains_line_ending(token.text(source)) {
            line_start = ends_with_line_ending(token.text(source));
            previous = (!line_start).then_some(index);
        } else {
            previous = Some(index);
        }

        if token.kind == Kind::LBrace
            && brace_style_stack.last() == Some(&BraceStyle::Expanded)
            && tokens.get(index + 1).is_some_and(|next| {
                !matches!(
                    next.kind,
                    Kind::Newline | Kind::RBrace | Kind::LineComment | Kind::BlockComment
                )
            })
        {
            push(&mut output, line_ending)?;
            line_start = true;
            previous = None;
        }
        if token.kind == Kind::Comma
            && (signature_wrapping.commas[index] || call_site_wrapping.commas[index])
            && tokens
                .get(index + 1)
                .is_some_and(|next| next.kind != Kind::Newline)
        {
            push(&mut output, line_ending)?;
            line_start = true;
            previous = None;
        }
        if token.kind == Kind::LParen
            && expanded_parens[index]
            && tokens
                .get(index + 1)
                .is_some_and(|next| next.kind != Kind::Newline)
        {
            push(&mut output, line_ending)?;
            line_start = true;
            previous = None;
        }
        // ktlint `when-entry-bracing`: a rewritten bare body opens with an inserted
        // ` {` on the arrow line; the body follows on the next line (its own newline,
        // if any, is dropped).
        if when_bracing.brace_after_arrow[index] {
            push(&mut output, " ")?;
            let brace_lo = output.len() as u32;
            push(&mut output, "{")?;
            inserted_spans.push((brace_lo, output.len() as u32));
            entry_levels.push(line_level);
            push(&mut output, line_ending)?;
            line_start = true;
            previous = None;
        }

        if first_on_line && token.kind == Kind::RBrace && brace_depth == 0 {
            // A body that closes a top-level property statement (an object expression,
            // `if`/`when`, or a lambda on the right-hand side) keeps the property as the
            // previous declaration line — ktlint draws no blank line between it and a
            // following property. Any other top-level body close is a declaration
            // boundary.
            previous_top_line = match saved_top_line {
                Some(TopLine::Property) => Some(TopLine::Property),
                _ => Some(TopLine::Closure),
            };
        }
        let _ = closing_style;
    }

    for marker in &source_scan.trailing_markers {
        push(
            &mut output,
            &source[marker.span.lo as usize..marker.span.hi as usize],
        )?;
    }
    if !options.trim_trailing_whitespace {
        preserve_eof_whitespace(&mut output, source, tokens)?;
    } else {
        preserve_ktlint_eof_tab(&mut output, source, tokens)?;
    }
    apply_final_newline_options(&mut output, line_ending, options)?;
    let dropped_spans: Vec<(u32, u32)> = tokens
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            dropped_semicolons[*index]
                || dropped_braces[*index]
                || dropped_parens[*index]
                || expression_bodies.dropped[*index]
                || enum_entries.drop_semicolon[*index]
        })
        .map(|(_, token)| (token.lo, token.hi))
        .collect();
    let formatted_scan = formatting_tokens(&output)?;
    (output.len() <= MAX_FORMATTING_OUTPUT_BYTES
        && same_non_whitespace_tokens(
            source,
            &source_scan.lexical,
            &output,
            &formatted_scan.lexical,
            &dropped_spans,
            &inserted_spans,
        ))
    .then_some(output)
}

fn push(output: &mut String, text: &str) -> Option<()> {
    if text.len() > MAX_FORMATTING_OUTPUT_BYTES.saturating_sub(output.len()) {
        return None;
    }
    output.push_str(text);
    Some(())
}

fn push_markers(
    output: &mut String,
    source: &str,
    markers: &[AttachedMarker],
    marker_index: &mut usize,
    token_index: usize,
) -> Option<()> {
    while let Some(marker) = markers
        .get(*marker_index)
        .filter(|marker| marker.token_index == token_index)
    {
        push(output, &source[marker.lo as usize..marker.hi as usize])?;
        *marker_index += 1;
    }
    Some(())
}

fn push_indentation(output: &mut String, indent: &str, depth: usize) -> Option<()> {
    if indent.is_empty() {
        return Some(());
    }
    let bytes = indent.len().checked_mul(depth)?;
    if bytes > MAX_FORMATTING_OUTPUT_BYTES.saturating_sub(output.len()) {
        return None;
    }
    for _ in 0..depth {
        output.push_str(indent);
    }
    Some(())
}

fn indentation(options: FormattingOptions) -> Option<String> {
    if !options.insert_spaces {
        return Some("\t".to_string());
    }
    let width = usize::try_from(options.tab_size).ok()?;
    (width <= MAX_FORMATTING_OUTPUT_BYTES).then(|| " ".repeat(width))
}

fn preserve_trailing_whitespace(
    output: &mut String,
    source: &str,
    tokens: &[Token],
    newline: usize,
) -> Option<()> {
    let start = newline
        .checked_sub(1)
        .map_or(0, |previous| tokens[previous].hi as usize);
    let end = tokens[newline].lo as usize;
    push(output, horizontal_suffix(&source[start..end]))
}

fn preserve_ktlint_trailing_tab(
    output: &mut String,
    source: &str,
    tokens: &[Token],
    newline: usize,
) -> Option<()> {
    let start = newline
        .checked_sub(1)
        .map_or(0, |previous| tokens[previous].hi as usize);
    let end = tokens[newline].lo as usize;
    if &source[start..end] == "\t" {
        push(output, "\t")?;
    }
    Some(())
}

fn preserve_eof_whitespace(output: &mut String, source: &str, tokens: &[Token]) -> Option<()> {
    let start = tokens.last().map_or(0, |token| token.hi as usize);
    push(output, horizontal_suffix(&source[start..]))
}

fn preserve_ktlint_eof_tab(output: &mut String, source: &str, tokens: &[Token]) -> Option<()> {
    let start = tokens.last().map_or(0, |token| token.hi as usize);
    if &source[start..] == "\t" {
        push(output, "\t")?;
    }
    Some(())
}

fn horizontal_suffix(text: &str) -> &str {
    let prefix = text.trim_end_matches([' ', '\t']);
    &text[prefix.len()..]
}

fn apply_final_newline_options(
    output: &mut String,
    line_ending: &str,
    options: FormattingOptions,
) -> Option<()> {
    if options.trim_final_newlines {
        let mut had_final_newline = false;
        while output.ends_with(line_ending) {
            output.truncate(output.len() - line_ending.len());
            had_final_newline = true;
        }
        if had_final_newline {
            push(output, line_ending)?;
        }
    }
    if options.insert_final_newline && !output.ends_with(line_ending) {
        push(output, line_ending)?;
    }
    Some(())
}

fn source_line_ending<'a>(source: &'a str, tokens: &[Token]) -> &'a str {
    if let Some(newline) = tokens.iter().find(|token| token.kind == Kind::Newline) {
        return newline.text(source);
    }
    let bytes = source.as_bytes();
    for (index, byte) in bytes.iter().copied().enumerate() {
        match byte {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => return &source[index..index + 2],
            b'\r' | b'\n' => return &source[index..index + 1],
            _ => {}
        }
    }
    "\n"
}

fn contains_line_ending(text: &str) -> bool {
    text.bytes().any(|byte| matches!(byte, b'\r' | b'\n'))
}

fn ends_with_line_ending(text: &str) -> bool {
    text.as_bytes()
        .last()
        .is_some_and(|byte| matches!(byte, b'\r' | b'\n'))
}

fn ends_with_blank_line(output: &str, line_ending: &str) -> bool {
    output
        .strip_suffix(line_ending)
        .is_some_and(|prefix| prefix.ends_with(line_ending))
}

fn ensure_blank_line(output: &mut String, line_ending: &str) -> Option<()> {
    if output.is_empty() || ends_with_blank_line(output, line_ending) {
        return Some(());
    }
    if output.ends_with(line_ending) {
        push(output, line_ending)
    } else {
        push(output, line_ending)?;
        push(output, line_ending)
    }
}

/// ktlint `no-semi` rule: marks semicolons that separate nothing — the next code starts on
/// a later line, or the statement is the last before a closing brace or EOF. Semicolons in
/// an enum class body are always kept: between enum entries and member declarations they
/// are required by the Kotlin grammar.
fn redundant_semicolons(tokens: &[Token], source: &str) -> Vec<bool> {
    let mut dropped = vec![false; tokens.len()];
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::Semicolon {
            continue;
        }
        let mut next = index + 1;
        while let Some(candidate) = tokens.get(next) {
            match candidate.kind {
                Kind::Semicolon | Kind::LineComment => next += 1,
                Kind::BlockComment if !contains_line_ending(candidate.text(source)) => next += 1,
                _ => break,
            }
        }
        let redundant = match tokens.get(next) {
            None => true,
            Some(candidate) => matches!(candidate.kind, Kind::Newline | Kind::RBrace),
        };
        if redundant && !enum_entry_semicolon(tokens, source, index) {
            dropped[index] = true;
        }
    }
    dropped
}

/// Whether the semicolon at `index` sits in the body of an enum class, where a semicolon
/// after the last enum entry may be required before member declarations.
fn enum_entry_semicolon(tokens: &[Token], source: &str, index: usize) -> bool {
    let mut depth = 0usize;
    let mut i = index;
    while i > 0 {
        i -= 1;
        match tokens[i].kind {
            Kind::RBrace => depth += 1,
            Kind::LBrace => {
                if depth == 0 {
                    return class_header_is_enum(tokens, source, i);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    false
}

/// Walks backward from a class-body `{` over the declaration header looking for the
/// `enum class` keyword pair. Stops at any construct that ends the header.
fn class_header_is_enum(tokens: &[Token], source: &str, brace: usize) -> bool {
    let mut saw_class = false;
    let mut saw_enum = false;
    let mut remaining = 64usize;
    let mut j = brace;
    while j > 0 && remaining > 0 {
        j -= 1;
        remaining -= 1;
        let token = tokens[j];
        match token.kind {
            Kind::Word => match token.text(source) {
                "class" => saw_class = true,
                "enum" => saw_enum = true,
                "interface" | "object" | "fun" | "val" | "var" | "typealias" => break,
                _ => {}
            },
            Kind::LBrace | Kind::RBrace | Kind::Semicolon => break,
            _ => {}
        }
        if saw_class && saw_enum {
            return true;
        }
    }
    false
}

/// ktlint `comment-spacing` rule: a line comment's text is separated from `//` by one
/// space. IntelliJ folding markers (`//region`, `//endregion`) are exempt, matching
/// ktlint; `//` alone and comments already followed by whitespace pass through.
fn spaced_line_comment(text: &str) -> std::borrow::Cow<'_, str> {
    let Some(rest) = text.strip_prefix("//") else {
        return std::borrow::Cow::Borrowed(text);
    };
    let exempt = rest.is_empty()
        || rest.starts_with(char::is_whitespace)
        || rest == "region"
        || rest.starts_with("region ")
        || rest == "endregion"
        || rest.starts_with("endregion ");
    if exempt {
        std::borrow::Cow::Borrowed(text)
    } else {
        std::borrow::Cow::Owned(format!("// {rest}"))
    }
}

/// ktlint `no-empty-class-body` rule: marks the `{}` pairs of empty classifier bodies for
/// removal (`class A {}` -> `class A`, also interfaces, objects, enum classes). Bodies
/// containing comments are not empty. Anonymous objects (`val x = object : T {}`) keep
/// their braces — those are required by the grammar — as do enum entry bodies, function
/// bodies, and lambdas.
fn empty_classifier_body_braces(tokens: &[Token], source: &str) -> Vec<bool> {
    let mut dropped = vec![false; tokens.len()];
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::LBrace {
            continue;
        }
        let mut next = index + 1;
        while tokens.get(next).is_some_and(|t| t.kind == Kind::Newline) {
            next += 1;
        }
        if tokens.get(next).is_some_and(|t| t.kind == Kind::RBrace)
            && classifier_body_brace(tokens, source, index)
        {
            dropped[index] = true;
            dropped[next] = true;
        }
    }
    dropped
}

/// Whether the `{` at `brace` opens the body of a named classifier declaration, by walking
/// backward over its header. Stops at statement boundaries so expression-position braces
/// (lambdas, blocks, anonymous objects) never qualify.
fn classifier_body_brace(tokens: &[Token], source: &str, brace: usize) -> bool {
    let mut remaining = 96usize;
    let mut paren_depth = 0usize;
    let mut j = brace;
    while j > 0 && remaining > 0 {
        j -= 1;
        remaining -= 1;
        let token = tokens[j];
        match token.kind {
            Kind::RParen | Kind::RBracket => paren_depth += 1,
            Kind::LParen | Kind::LBracket => {
                paren_depth = paren_depth.saturating_sub(1);
            }
            _ if paren_depth > 0 => {}
            Kind::Word => match token.text(source) {
                "class" | "interface" => return true,
                "object" => return named_object(tokens, source, j),
                "fun" | "val" | "var" | "typealias" => return false,
                _ => {}
            },
            Kind::Operator if token.text(source) == "=" => return false,
            Kind::Semicolon | Kind::LBrace | Kind::RBrace => return false,
            _ => {}
        }
    }
    false
}

/// Whether `object` at `index` starts a named object declaration rather than an anonymous
/// object expression: named objects follow a statement boundary or the `companion`
/// keyword; expressions follow `=` or `:`.
fn named_object(tokens: &[Token], source: &str, index: usize) -> bool {
    let mut j = index;
    while j > 0 {
        j -= 1;
        match tokens[j].kind {
            Kind::Newline => continue,
            Kind::Word => return tokens[j].text(source) == "companion",
            Kind::Operator | Kind::Colon => return false,
            _ => return true,
        }
    }
    true
}

/// ktlint `no-empty-primary-constructor` rule: marks the `()` of an empty primary
/// constructor for removal (`class E()` -> `class E`). Only the bare `class <Name>()`
/// form qualifies — `class F constructor()`, annotations, and type parameters keep theirs.
fn empty_primary_constructor_parens(tokens: &[Token], source: &str) -> Vec<bool> {
    let mut dropped = vec![false; tokens.len()];
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::LParen {
            continue;
        }
        if !tokens
            .get(index + 1)
            .is_some_and(|next| next.kind == Kind::RParen)
        {
            continue;
        }
        let Some(name) = index.checked_sub(1) else {
            continue;
        };
        if tokens[name].kind != Kind::Word {
            continue;
        }
        let mut before = name;
        while before > 0 && tokens[before - 1].kind == Kind::Newline {
            before -= 1;
        }
        let is_class_name = before > 0
            && tokens[before - 1].kind == Kind::Word
            && tokens[before - 1].text(source) == "class";
        if is_class_name {
            dropped[index] = true;
            dropped[index + 1] = true;
        }
    }
    dropped
}

/// ktlint `function-signature` / `class-signature` rules (`ktlint_official`): a function
/// or class header with two or more parameters is rewritten to one parameter per line with
/// a trailing comma. A single-parameter header written on one line stays; one spanning
/// multiple lines collapses to a single line when the parameter list holds no commas at
/// all, and otherwise keeps its lines but still gains the trailing comma. Calls and
/// lambdas are untouched.
struct SignatureWrapping {
    /// LParen and matching RParen of headers wrapped to one parameter per line.
    parens: Vec<bool>,
    /// Top-level commas of wrapped headers, after which a line break is emitted.
    commas: Vec<bool>,
    /// RParens that need a trailing comma: wrapped headers plus multiline headers that
    /// were left expanded.
    trailing_rparens: Vec<bool>,
    /// Newlines inside a collapsing single-parameter header, removed from the output.
    collapsed_newlines: Vec<bool>,
}

fn signature_wrapping(
    tokens: &[Token],
    source: &str,
    previous_significant: &[Option<usize>],
    generic_angles: &[i8],
) -> SignatureWrapping {
    let mut wrapping = SignatureWrapping {
        parens: vec![false; tokens.len()],
        commas: vec![false; tokens.len()],
        trailing_rparens: vec![false; tokens.len()],
        collapsed_newlines: vec![false; tokens.len()],
    };
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::LParen {
            continue;
        }
        let Some(keyword) =
            signature_keyword(tokens, source, previous_significant, generic_angles, index)
        else {
            continue;
        };
        let mut depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut brace_depth = 0usize;
        let mut angle_depth = 0i32;
        let mut top_commas = Vec::new();
        let mut any_comma = false;
        let mut multiline = false;
        let mut close = None;
        for (candidate_index, candidate) in tokens.iter().enumerate().skip(index) {
            match candidate.kind {
                Kind::LParen => depth += 1,
                Kind::RParen => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        close = Some(candidate_index);
                        break;
                    }
                }
                Kind::LBracket => bracket_depth += 1,
                Kind::RBracket => bracket_depth = bracket_depth.saturating_sub(1),
                Kind::LBrace => brace_depth += 1,
                Kind::RBrace => brace_depth = brace_depth.saturating_sub(1),
                Kind::Newline => multiline = true,
                Kind::Comma => {
                    any_comma = true;
                    if depth == 1 && bracket_depth == 0 && brace_depth == 0 && angle_depth == 0 {
                        top_commas.push(candidate_index);
                    }
                }
                _ => {}
            }
            angle_depth += i32::from(generic_angles[candidate_index]);
        }
        let Some(close) = close else { continue };
        // ktlint `class-signature`: a class header with type parameters wraps even a
        // single-parameter list one parameter per line with a trailing comma.
        let class_with_type_params = tokens[keyword].text(source) == "class"
            && (keyword..index).any(|candidate| generic_angles[candidate] > 0)
            && (index + 1..close).any(|candidate| tokens[candidate].kind != Kind::Newline);
        if !top_commas.is_empty() {
            wrapping.parens[index] = true;
            wrapping.parens[close] = true;
            wrapping.trailing_rparens[close] = true;
            for comma in top_commas {
                wrapping.commas[comma] = true;
            }
        } else if multiline && !any_comma {
            for (newline_index, newline) in tokens.iter().enumerate().take(close).skip(index + 1) {
                if newline.kind == Kind::Newline {
                    wrapping.collapsed_newlines[newline_index] = true;
                }
            }
        } else if multiline {
            wrapping.trailing_rparens[close] = true;
            wrapping.parens[close] = true;
        } else if class_with_type_params {
            wrapping.parens[index] = true;
            wrapping.parens[close] = true;
            wrapping.trailing_rparens[close] = true;
        }
    }
    wrapping
}

/// The `fun`/`class` keyword whose parameter list the `(` at `index` opens: the declared
/// name immediately before the `(`, possibly behind a type-parameter list
/// (`class A<T>(`) or a receiver chain (`fun String.g(`), with the `fun`/`class` keyword
/// before that.
fn signature_keyword(
    tokens: &[Token],
    source: &str,
    previous_significant: &[Option<usize>],
    generic_angles: &[i8],
    index: usize,
) -> Option<usize> {
    let mut name = previous_significant[index]?;
    if tokens[name].kind == Kind::Operator && generic_angles[name] < 0 {
        name = previous_significant[matching_opener(tokens, generic_angles, name)?]?;
    }
    if tokens[name].kind != Kind::Word {
        return None;
    }
    let mut cursor = name;
    while let Some(previous) = previous_significant[cursor] {
        if tokens[previous].kind == Kind::Dot {
            match previous_significant[previous] {
                Some(receiver) if tokens[receiver].kind == Kind::Word => {
                    cursor = receiver;
                    continue;
                }
                _ => return None,
            }
        }
        break;
    }
    previous_significant[cursor].filter(|&keyword| {
        tokens[keyword].kind == Kind::Word
            && matches!(tokens[keyword].text(source), "fun" | "class")
    })
}

/// Index of the opener matching the closer at `close`: `()` and `[]` pair by kind, and a
/// generic closing `>` (role < 0 in `generic_angles`) pairs with its `<`.
fn matching_opener(tokens: &[Token], generic_angles: &[i8], close: usize) -> Option<usize> {
    if tokens[close].kind == Kind::Operator && generic_angles[close] < 0 {
        let mut depth = 1i32;
        for candidate in (0..close).rev() {
            depth -= i32::from(generic_angles[candidate]);
            if depth == 0 {
                return Some(candidate);
            }
        }
        return None;
    }
    let (opener, closer) = match tokens[close].kind {
        Kind::RParen => (Kind::LParen, Kind::RParen),
        Kind::RBracket => (Kind::LBracket, Kind::RBracket),
        _ => return None,
    };
    let mut depth = 1usize;
    for candidate in (0..close).rev() {
        if tokens[candidate].kind == closer {
            depth += 1;
        } else if tokens[candidate].kind == opener {
            depth -= 1;
            if depth == 0 {
                return Some(candidate);
            }
        }
    }
    None
}

/// ktlint `spacing-around-colon` / `type-parameter-list-spacing`: a colon that introduces
/// a supertype list (`class C : Super`) or bounds a type parameter (`T : Comparable<T>`)
/// is preceded by a space, while a property, parameter, or return-type colon stays tight
/// (`val x: Int`). The space after any colon is handled by `needs_space`.
fn wide_colons(
    tokens: &[Token],
    source: &str,
    previous_significant: &[Option<usize>],
    generic_angles: &[i8],
) -> Vec<bool> {
    let mut angle_depth = vec![0i32; tokens.len()];
    let mut depth = 0i32;
    for (index, role) in generic_angles.iter().enumerate() {
        angle_depth[index] = depth;
        depth = (depth + i32::from(*role)).max(0);
    }
    let mut wide = vec![false; tokens.len()];
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::Colon {
            continue;
        }
        if angle_depth[index] > 0 {
            wide[index] = true;
            continue;
        }
        // Skip back over a balanced closer (primary constructor or type-parameter list)
        // to the name or keyword anchoring the colon.
        let mut cursor = index;
        let anchor = loop {
            let Some(previous) = previous_significant[cursor] else {
                break None;
            };
            match tokens[previous].kind {
                Kind::RParen | Kind::RBracket => {
                    match matching_opener(tokens, generic_angles, previous) {
                        Some(opener) => cursor = opener,
                        None => break None,
                    }
                }
                Kind::Operator if generic_angles[previous] < 0 => {
                    match matching_opener(tokens, generic_angles, previous) {
                        Some(opener) => cursor = opener,
                        None => break None,
                    }
                }
                _ => break Some(previous),
            }
        };
        let Some(anchor) = anchor else { continue };
        if tokens[anchor].kind != Kind::Word {
            continue;
        }
        let text = tokens[anchor].text(source);
        if matches!(text, "object" | "constructor") {
            wide[index] = true;
            continue;
        }
        if previous_significant[anchor].is_some_and(|before| {
            tokens[before].kind == Kind::Word
                && matches!(
                    tokens[before].text(source),
                    "class" | "interface" | "object"
                )
        }) {
            wide[index] = true;
        }
    }
    wide
}

/// ktlint `multiline-expression-wrapping` (`ktlint_official`): a right-hand side of `=`
/// that spans multiple lines in the output moves to the line after the `=` and carries one
/// extra indent level through its whole extent (`val b =\n    when (1) {\n ...`). A lambda
/// right-hand side keeps its opening brace on the `=` line.
struct ExpressionWraps {
    /// Extra indentation level per token index.
    extra: Vec<u8>,
    /// First token of a wrapped right-hand side: a line break goes before it.
    starts: Vec<bool>,
}

struct ElvisWraps {
    /// A trailing `?:` that ktlint relocates to the start of the following line.
    relocate: Vec<bool>,
    /// The source newline after a relocated `?:`; the operator now precedes its operand.
    drop_newline: Vec<bool>,
}

fn elvis_wraps(tokens: &[Token], source: &str) -> ElvisWraps {
    let mut wraps = ElvisWraps {
        relocate: vec![false; tokens.len()],
        drop_newline: vec![false; tokens.len()],
    };
    for question in 0..tokens.len() {
        if !is_elvis_question(tokens, source, question) {
            continue;
        }
        let colon = question + 1;
        let newline = colon + 1;
        if tokens
            .get(newline)
            .is_some_and(|token| token.kind == Kind::Newline)
            && tokens.get(newline + 1).is_some()
        {
            wraps.relocate[question] = true;
            wraps.drop_newline[newline] = true;
        }
    }
    wraps
}

fn is_elvis_question(tokens: &[Token], source: &str, index: usize) -> bool {
    tokens.get(index).is_some_and(|token| {
        token.kind == Kind::Operator
            && token.text(source) == "?"
            && tokens.get(index + 1).is_some_and(|colon| {
                colon.kind == Kind::Colon && token.logical_hi == colon.logical_lo
            })
    })
}

fn is_elvis_colon(tokens: &[Token], source: &str, index: usize) -> bool {
    index
        .checked_sub(1)
        .is_some_and(|question| is_elvis_question(tokens, source, question))
}

fn expression_wraps(
    tokens: &[Token],
    source: &str,
    next_significant: &[Option<usize>],
    previous_significant: &[Option<usize>],
    token_brace_styles: &[BraceStyle],
    generic_angles: &[i8],
    wrapped_signature_parens: &[bool],
) -> ExpressionWraps {
    let mut wraps = ExpressionWraps {
        extra: vec![0; tokens.len()],
        starts: vec![false; tokens.len()],
    };
    // Paren/brace/bracket/angle depths before each token.
    let mut depths = vec![[0i32; 4]; tokens.len()];
    let mut running = [0i32; 4];
    for (index, token) in tokens.iter().enumerate() {
        depths[index] = running;
        match token.kind {
            Kind::LParen => running[0] += 1,
            Kind::RParen => running[0] -= 1,
            Kind::LBrace => running[1] += 1,
            Kind::RBrace => running[1] -= 1,
            Kind::LBracket => running[2] += 1,
            Kind::RBracket => running[2] -= 1,
            _ => {}
        }
        running[3] += i32::from(generic_angles[index]);
    }
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::Operator || token.text(source) != "=" {
            continue;
        }
        let Some(start) = next_significant[index] else {
            continue;
        };
        // A `=` closing a wrapped function signature keeps its right-hand side on the
        // closing-paren line (`) = when {`).
        if previous_significant[index]
            .is_some_and(|prev| tokens[prev].kind == Kind::RParen && wrapped_signature_parens[prev])
        {
            continue;
        }
        if tokens[start].kind == Kind::LBrace {
            // A lambda right-hand side keeps its `{` on the `=` line.
            continue;
        }
        let base = depths[index];
        // The right-hand side ends at the first token at base depth that cannot continue
        // the expression: a statement newline (not behind an operator or dot), a
        // separator, or a closer of an enclosing pair.
        let mut end = tokens.len();
        for cursor in start..tokens.len() {
            let depth = depths[cursor];
            if depth[0] < base[0] || depth[1] < base[1] || depth[2] < base[2] || depth[3] < base[3]
            {
                end = cursor;
                break;
            }
            if depth == base {
                match tokens[cursor].kind {
                    Kind::Semicolon | Kind::Comma => {
                        end = cursor;
                        break;
                    }
                    Kind::Newline => {
                        // The expression continues across the newline when the line ends
                        // with an operator or dot, or the next line leads with a dot (a
                        // wrapped call chain).
                        let continues = previous_significant[cursor].is_some_and(|prev| {
                            matches!(tokens[prev].kind, Kind::Operator | Kind::Dot)
                                || is_elvis_colon(tokens, source, prev)
                        }) || next_significant[cursor].is_some_and(|next| {
                            tokens[next].kind == Kind::Dot
                                || is_elvis_question(tokens, source, next)
                        });
                        if !continues {
                            end = cursor;
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        // Wrap only when the right-hand side is multiline in the output: it already
        // spans lines or holds a block that expands. A raw string alone stays on the
        // `=` line; as part of a larger expression (`"""\n...\n""".trimIndent()`) the
        // whole expression wraps.
        let mut multiline = false;
        let mut multiline_literal = false;
        let mut string_only = true;
        let mut template_depth = 0usize;
        for cursor in start..end {
            let token = tokens[cursor];
            match token.kind {
                Kind::Newline => {
                    multiline = true;
                    break;
                }
                Kind::TemplateExpressionStart => template_depth += 1,
                Kind::TemplateExpressionEnd => template_depth = template_depth.saturating_sub(1),
                Kind::Literal => {
                    if contains_line_ending(token.text(source)) {
                        multiline_literal = true;
                    }
                }
                _ => {
                    if template_depth == 0 {
                        string_only = false;
                    }
                    if token.kind == Kind::LBrace
                        && token_brace_styles[cursor] == BraceStyle::Expanded
                        && brace_contains_code(tokens, cursor)
                    {
                        multiline = true;
                        break;
                    }
                }
            }
        }
        let multiline = multiline || multiline_literal && !string_only;
        if !multiline {
            continue;
        }
        wraps.starts[start] = true;
        // ktlint binary-expression-wrapping: an operand that spans lines starts on the
        // line after its operator (`"prefix" + """..."""` breaks before the string).
        let mut cursor = start;
        while cursor < end {
            if depths[cursor] == base
                && continuation_operator(
                    tokens,
                    source,
                    generic_angles,
                    previous_significant,
                    cursor,
                )
            {
                if let Some(operand) = next_significant[cursor] {
                    if operand < end {
                        let operand_end = operand_extent(
                            tokens,
                            depths.as_slice(),
                            next_significant,
                            previous_significant,
                            operand,
                            end,
                            base,
                        );
                        if region_multiline(
                            tokens,
                            source,
                            token_brace_styles,
                            operand,
                            operand_end,
                        ) {
                            wraps.starts[operand] = true;
                        }
                    }
                }
            }
            cursor += 1;
        }
        for extra in wraps.extra.iter_mut().take(end).skip(start) {
            *extra = extra.saturating_add(1);
        }
    }
    // ktlint `indent`: a line whose previous line ends with a binary operator, or that
    // leads with a `.`, is a continuation line and gains one indent level. The level
    // persists while following lines stay deeper than the operator or dot — the
    // interior of a multiline operand, including its closing delimiters, keeps it.
    for (newline, token) in tokens.iter().enumerate() {
        if token.kind != Kind::Newline {
            continue;
        }
        let anchor = previous_significant[newline]
            .filter(|&previous| {
                continuation_operator(
                    tokens,
                    source,
                    generic_angles,
                    previous_significant,
                    previous,
                )
            })
            .or_else(|| {
                next_significant[newline].filter(|&next| {
                    tokens[next].kind == Kind::Dot || is_elvis_question(tokens, source, next)
                })
            });
        let Some(anchor) = anchor else { continue };
        let base = depths[anchor];
        let mut first_line = true;
        let mut line = newline + 1;
        while line < tokens.len() {
            let mut line_end = line;
            while line_end < tokens.len() && tokens[line_end].kind != Kind::Newline {
                line_end += 1;
            }
            let first = (line..line_end).find(|&index| {
                !matches!(tokens[index].kind, Kind::LineComment | Kind::BlockComment)
            });
            let covered = match first {
                // Comment-only and blank lines keep (and share) the continuation.
                None => true,
                Some(index) => {
                    first_line || {
                        let depth = depths[index];
                        depth
                            .iter()
                            .zip(base.iter())
                            .all(|(depth, base)| depth >= base)
                            && depth
                                .iter()
                                .zip(base.iter())
                                .any(|(depth, base)| depth > base)
                    }
                }
            };
            if !covered {
                break;
            }
            for extra in wraps.extra.iter_mut().take(line_end).skip(line) {
                *extra = extra.saturating_add(1);
            }
            first_line = false;
            line = line_end + 1;
        }
    }
    // ktlint `chain-wrapping`: a dotted chain whose segments span lines puts each segment
    // on its own line — a break goes before every dot of the chain, not only the ones
    // already wrapped in the input. Safe-call dots (`?.`, two tokens) do not chain here.
    for (index, token) in tokens.iter().enumerate() {
        // A chain starts at an atom (word or literal) that is not itself a selector.
        if !matches!(token.kind, Kind::Word | Kind::Literal) {
            continue;
        }
        if previous_significant[index].is_some_and(|previous| tokens[previous].kind == Kind::Dot) {
            continue;
        }
        let mut dots = Vec::new();
        let mut multiline = false;
        let mut position = index;
        loop {
            // Skip call, index, and type-argument trailers of the atom or selector.
            while let Some(next) = next_significant[position] {
                if matches!(tokens[next].kind, Kind::LParen | Kind::LBracket) {
                    let close_kind = if tokens[next].kind == Kind::LParen {
                        Kind::RParen
                    } else {
                        Kind::RBracket
                    };
                    let mut depth = 0usize;
                    let mut close = None;
                    for (cursor, trailer) in tokens.iter().enumerate().skip(next) {
                        if trailer.kind == tokens[next].kind {
                            depth += 1;
                        } else if trailer.kind == close_kind {
                            depth = depth.saturating_sub(1);
                            if depth == 0 {
                                close = Some(cursor);
                                break;
                            }
                        }
                    }
                    let Some(close) = close else { break };
                    position = close;
                    continue;
                }
                if tokens[next].kind == Kind::Operator && generic_angles[next] > 0 {
                    // Type arguments between the selector and its call: `a.add<Int>(1)`.
                    let mut angle = 0i32;
                    let mut close = None;
                    for (cursor, role) in generic_angles.iter().enumerate().skip(next) {
                        angle += i32::from(*role);
                        if angle == 0 {
                            close = Some(cursor);
                            break;
                        }
                    }
                    let Some(close) = close else { break };
                    position = close;
                    continue;
                }
                break;
            }
            let Some(dot) = next_significant[position] else {
                break;
            };
            if tokens[dot].kind != Kind::Dot {
                break;
            }
            if (position + 1..dot).any(|cursor| tokens[cursor].kind == Kind::Newline) {
                multiline = true;
            }
            let Some(selector) = next_significant[dot] else {
                break;
            };
            if tokens[selector].kind != Kind::Word {
                break;
            }
            if (dot + 1..selector).any(|cursor| tokens[cursor].kind == Kind::Newline) {
                multiline = true;
            }
            dots.push(dot);
            position = selector;
        }
        if multiline {
            for dot in dots {
                wraps.starts[dot] = true;
            }
        }
    }
    wraps
}

/// Whether the operator at `index` is a binary operator whose operand may continue on
/// the next line: assignments (`=`), arrows (`->`), tight or unary operators, and
/// generic angle brackets never trail a line in ktlint's continuation style.
fn continuation_operator(
    tokens: &[Token],
    source: &str,
    generic_angles: &[i8],
    previous_significant: &[Option<usize>],
    index: usize,
) -> bool {
    if tokens[index].kind != Kind::Operator || generic_angles[index] != 0 {
        return false;
    }
    let text = tokens[index].text(source);
    if text == "=" || text == "->" || text == "!" || tight_operator(text) {
        return false;
    }
    !unary_operator(tokens, source, index, previous_significant[index])
}

/// The end (exclusive) of the operand starting at `operand`: the first token back at
/// the operator's own depth that cannot continue the operand — a base-depth operator
/// or separator, or a statement newline not glued to a dot or operator.
fn operand_extent(
    tokens: &[Token],
    depths: &[[i32; 4]],
    next_significant: &[Option<usize>],
    previous_significant: &[Option<usize>],
    operand: usize,
    end: usize,
    base: [i32; 4],
) -> usize {
    for cursor in operand..end {
        if depths[cursor] != base {
            continue;
        }
        match tokens[cursor].kind {
            Kind::Operator | Kind::Comma | Kind::Semicolon => return cursor,
            Kind::Newline => {
                let continues = previous_significant[cursor].is_some_and(|previous| {
                    matches!(tokens[previous].kind, Kind::Operator | Kind::Dot)
                }) || next_significant[cursor]
                    .is_some_and(|next| tokens[next].kind == Kind::Dot);
                if !continues {
                    return cursor;
                }
            }
            _ => {}
        }
    }
    end
}

/// Whether the token region spans lines in the output: it holds a newline, a multiline
/// literal, or a block brace that expands around code.
fn region_multiline(
    tokens: &[Token],
    source: &str,
    token_brace_styles: &[BraceStyle],
    from: usize,
    to: usize,
) -> bool {
    for cursor in from..to {
        let token = tokens[cursor];
        match token.kind {
            Kind::Newline => return true,
            Kind::Literal if contains_line_ending(token.text(source)) => return true,
            Kind::LBrace
                if token_brace_styles[cursor] == BraceStyle::Expanded
                    && brace_contains_code(tokens, cursor) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// Whether the brace opened at `open` holds any code token (not only whitespace or
/// comments) before its matching close.
fn brace_contains_code(tokens: &[Token], open: usize) -> bool {
    let mut depth = 1usize;
    for token in tokens.iter().skip(open + 1) {
        match token.kind {
            Kind::LBrace => depth += 1,
            Kind::RBrace => {
                depth -= 1;
                if depth == 0 {
                    return false;
                }
            }
            Kind::Newline | Kind::LineComment | Kind::BlockComment => {}
            _ => return true,
        }
    }
    false
}

/// ktlint enum-entry formatting (`ktlint_official`): an enum body without member
/// declarations keeps the layout it was written in; once the entries span lines or
/// members follow, each entry goes on its own line with a trailing comma, the entries
/// `;` sits on its own line, and a blank line separates it from the members. A `;` with
/// no members after it is dropped when the body is rewritten.
struct EnumEntries {
    /// Enum body open braces that keep their input layout (no members, single line).
    keep_inline: Vec<bool>,
    /// Tokens that start on a fresh line (entry after a comma, the entries `;`).
    break_before: Vec<bool>,
    /// Tokens preceded by a blank line (first member after the entries `;`).
    blank_before: Vec<bool>,
    /// Tokens before which a trailing comma is inserted (the entries `;` or body `}`).
    comma_before: Vec<bool>,
    /// Entries `;` dropped when no members follow in a rewritten (multiline) body.
    drop_semicolon: Vec<bool>,
    /// Entry body braces (`A(1) { override fun f() = 1 }`) expand like class bodies.
    force_expanded: Vec<bool>,
}

fn enum_entries(
    tokens: &[Token],
    source: &str,
    next_significant: &[Option<usize>],
    generic_angles: &[i8],
) -> EnumEntries {
    let mut entries = EnumEntries {
        keep_inline: vec![false; tokens.len()],
        break_before: vec![false; tokens.len()],
        blank_before: vec![false; tokens.len()],
        comma_before: vec![false; tokens.len()],
        drop_semicolon: vec![false; tokens.len()],
        force_expanded: vec![false; tokens.len()],
    };
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::Word || token.text(source) != "enum" {
            continue;
        }
        let Some(class_keyword) = next_significant[index] else {
            continue;
        };
        if tokens[class_keyword].kind != Kind::Word || tokens[class_keyword].text(source) != "class"
        {
            continue;
        }
        // The body opens at the first `{` outside the header's parens, brackets, and
        // type parameters.
        let mut paren = 0i32;
        let mut bracket = 0i32;
        let mut angle = 0i32;
        let mut open = None;
        for (cursor, cursor_token) in tokens.iter().enumerate().skip(class_keyword + 1) {
            match cursor_token.kind {
                Kind::LParen => paren += 1,
                Kind::RParen => paren -= 1,
                Kind::LBracket => bracket += 1,
                Kind::RBracket => bracket -= 1,
                Kind::LBrace if paren == 0 && bracket == 0 && angle == 0 => {
                    open = Some(cursor);
                    break;
                }
                Kind::LBrace | Kind::Semicolon => break,
                _ => {}
            }
            angle += i32::from(generic_angles[cursor]);
        }
        let Some(open) = open else { continue };
        let mut depth = 1usize;
        let mut close = None;
        for (cursor, cursor_token) in tokens.iter().enumerate().skip(open + 1) {
            match cursor_token.kind {
                Kind::LBrace => depth += 1,
                Kind::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(cursor);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else { continue };
        // Scan the entry section: tokens at body depth 1 up to the entries `;`.
        let mut depth = 1i32;
        let mut paren = 0i32;
        let mut bracket = 0i32;
        let mut angle = 0i32;
        let mut multiline = false;
        let mut entry_commas = Vec::new();
        let mut semicolon = None;
        let mut cursor = open + 1;
        while cursor < close {
            let cursor_token = tokens[cursor];
            match cursor_token.kind {
                Kind::LBrace => {
                    if depth == 1 && paren == 0 && bracket == 0 && angle == 0 {
                        // An enum entry body is an anonymous class body, not a lambda.
                        entries.force_expanded[cursor] = true;
                    }
                    depth += 1;
                }
                Kind::RBrace => depth -= 1,
                Kind::LParen => paren += 1,
                Kind::RParen => paren -= 1,
                Kind::LBracket => bracket += 1,
                Kind::RBracket => bracket -= 1,
                Kind::Newline => multiline = true,
                Kind::Comma if depth == 1 && paren == 0 && bracket == 0 && angle == 0 => {
                    entry_commas.push(cursor);
                }
                Kind::Semicolon if depth == 1 && paren == 0 && bracket == 0 && angle == 0 => {
                    semicolon = Some(cursor);
                    break;
                }
                _ => {}
            }
            angle += i32::from(generic_angles[cursor]);
            cursor += 1;
        }
        let has_members = semicolon.is_some_and(|semi| {
            (semi + 1..close).any(|member| {
                !matches!(
                    tokens[member].kind,
                    Kind::Newline | Kind::LineComment | Kind::BlockComment
                )
            })
        });
        if !multiline && (semicolon.is_none() || !has_members) {
            // `enum class Color { RED, GREEN, BLUE }`: no members, one line — untouched.
            entries.keep_inline[open] = true;
            continue;
        }
        match semicolon {
            Some(semi) if has_members => {
                entries.comma_before[semi] = true;
                entries.break_before[semi] = true;
                if let Some(member) = next_significant[semi] {
                    if member < close {
                        entries.blank_before[member] = true;
                    }
                }
            }
            Some(semi) => entries.drop_semicolon[semi] = true,
            None => {}
        }
        for comma in entry_commas {
            if let Some(next) = next_significant[comma] {
                entries.break_before[next] = true;
            }
        }
        if !has_members {
            // Without members the trailing comma goes before the body close.
            entries.comma_before[close] = true;
        }
    }
    entries
}

/// ktlint `function-expression-body` rule: a function body consisting of a single `return`
/// statement is rewritten as an expression body (`fun f() { return x }` -> `fun f() = x`).
/// A `when` or `if` expression may span lines and still converts. Marks the braces, the
/// `return` keyword, and the wrapper newlines for removal, and the first expression token
/// as the point where ` = ` is inserted.
struct ExpressionBodies {
    dropped: Vec<bool>,
    starts: Vec<bool>,
}

fn expression_bodies(
    tokens: &[Token],
    source: &str,
    previous_significant: &[Option<usize>],
) -> ExpressionBodies {
    let mut bodies = ExpressionBodies {
        dropped: vec![false; tokens.len()],
        starts: vec![false; tokens.len()],
    };
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::LBrace {
            continue;
        }
        let mut depth = 0usize;
        let mut close = None;
        for (candidate_index, candidate) in tokens.iter().enumerate().skip(index) {
            match candidate.kind {
                Kind::LBrace => depth += 1,
                Kind::RBrace => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        close = Some(candidate_index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else { continue };
        let content: Vec<usize> = (index + 1..close)
            .filter(|&candidate| tokens[candidate].kind != Kind::Newline)
            .collect();
        // Content must be exactly `return <expr>` with no comments or separators.
        if content.len() < 2
            || tokens[content[0]].kind != Kind::Word
            || tokens[content[0]].text(source) != "return"
            || content.iter().any(|&candidate| {
                matches!(
                    tokens[candidate].kind,
                    Kind::LineComment | Kind::BlockComment | Kind::Semicolon
                )
            })
        {
            continue;
        }
        // Newlines may only wrap the statement, not split the expression — unless the
        // expression is a `when` or `if`, whose branches legitimately span lines.
        let first = content[0];
        let last = *content.last().expect("non-empty content");
        let multiline_head = matches!(tokens[content[1]].text(source), "when" | "if");
        if !multiline_head
            && (index + 1..close).any(|candidate| {
                tokens[candidate].kind == Kind::Newline && candidate > first && candidate < last
            })
        {
            continue;
        }
        if !function_body_brace(tokens, source, previous_significant, index) {
            continue;
        }
        bodies.dropped[index] = true;
        bodies.dropped[first] = true;
        bodies.dropped[close] = true;
        for (candidate, candidate_token) in tokens.iter().enumerate().take(close).skip(index + 1) {
            if candidate_token.kind == Kind::Newline && (candidate < first || candidate > last) {
                bodies.dropped[candidate] = true;
            }
        }
        bodies.starts[content[1]] = true;
    }
    bodies
}

/// Whether the `{` at `brace` opens a function body, by walking backward over its header.
/// Blocks (`if`, `try`, lambdas, ...), class bodies, and property getters do not qualify —
/// ktlint leaves getter block bodies alone.
fn function_body_brace(
    tokens: &[Token],
    source: &str,
    previous_significant: &[Option<usize>],
    brace: usize,
) -> bool {
    let mut cursor = brace;
    let mut remaining = 96usize;
    while remaining > 0 {
        remaining -= 1;
        let Some(previous) = previous_significant[cursor] else {
            return false;
        };
        cursor = previous;
        let token = tokens[cursor];
        match token.kind {
            Kind::Word => match token.text(source) {
                "fun" => return true,
                "class" | "interface" | "object" | "val" | "var" | "typealias" | "constructor"
                | "init" | "set" | "get" => return false,
                _ => {}
            },
            Kind::Operator if token.text(source) == "=" => return false,
            Kind::Semicolon | Kind::LBrace | Kind::RBrace => return false,
            _ => {}
        }
    }
    false
}

/// ktlint `when-entry-bracing` and blank separation (`ktlint_official`): once any entry
/// of a `when` has a block body or puts its body on the line after the `->`, every bare
/// entry body becomes a block (`1 -> "one"` -> `1 -> {\n"one"\n}`) and blank lines
/// separate the entries. A `when` whose bodies are all bare and same-line, or that has a
/// multi-statement body, keeps its layout.
struct WhenEntryBracing {
    /// `->` tokens after which ` {` is inserted.
    brace_after_arrow: Vec<bool>,
    /// Newlines between an arrow and its separate-line body, dropped by the rewrite.
    drop_newline: Vec<bool>,
    /// Body tokens indented one extra level: the inserted `{` is not a token, so the
    /// emitter's brace depth never sees it.
    body_extra: Vec<bool>,
    /// Tokens before which the inserted `}` line goes: the next entry's condition start
    /// or the `when`'s closing brace.
    close_before: Vec<bool>,
    /// Entry conditions preceded by a blank line once the `when` is rewritten.
    blank_before: Vec<bool>,
}

fn when_entry_bracing(
    tokens: &[Token],
    source: &str,
    next_significant: &[Option<usize>],
) -> WhenEntryBracing {
    let mut bracing = WhenEntryBracing {
        brace_after_arrow: vec![false; tokens.len()],
        drop_newline: vec![false; tokens.len()],
        body_extra: vec![false; tokens.len()],
        close_before: vec![false; tokens.len()],
        blank_before: vec![false; tokens.len()],
    };
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::Word || token.text(source) != "when" {
            continue;
        }
        // The subject and the body brace: `when (x) {` or `when {`.
        let mut paren = 0i32;
        let mut open = None;
        for (cursor, candidate) in tokens.iter().enumerate().skip(index + 1) {
            match candidate.kind {
                Kind::LParen => paren += 1,
                Kind::RParen => paren -= 1,
                Kind::LBrace if paren == 0 => {
                    open = Some(cursor);
                    break;
                }
                Kind::Semicolon => break,
                _ => {}
            }
        }
        let Some(open) = open else { continue };
        let mut depth = 0usize;
        let mut close = None;
        for (cursor, candidate) in tokens.iter().enumerate().skip(open) {
            match candidate.kind {
                Kind::LBrace => depth += 1,
                Kind::RBrace => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        close = Some(cursor);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else { continue };
        // Entries: arrows at body depth 1 outside any nested parens.
        let mut arrows = Vec::new();
        let mut depth = 1i32;
        let mut paren = 0i32;
        for (cursor, candidate) in tokens.iter().enumerate().take(close).skip(open + 1) {
            match candidate.kind {
                Kind::LBrace => depth += 1,
                Kind::RBrace => depth -= 1,
                Kind::LParen => paren += 1,
                Kind::RParen => paren -= 1,
                Kind::Operator if depth == 1 && paren == 0 && candidate.text(source) == "->" => {
                    arrows.push(cursor);
                }
                _ => {}
            }
        }
        if arrows.is_empty() {
            continue;
        }
        // Each entry: condition tokens up to its arrow, then the body. A body ends at
        // the next depth-1 newline, the depth-1 `else` of the following entry, or the
        // `when`'s closing brace.
        struct Entry {
            arrow: usize,
            cond_start: usize,
            body_start: usize,
            body_last: usize,
            block: bool,
            separate_line: bool,
            /// First token of the next entry's condition, or the `when`'s close.
            anchor: usize,
        }
        let mut entries: Vec<Entry> = Vec::with_capacity(arrows.len());
        for (entry, &arrow) in arrows.iter().enumerate() {
            let Some(body_start) = next_significant[arrow].filter(|&start| start < close) else {
                break;
            };
            let mut body_last = body_start;
            let mut depth = 1i32;
            let mut paren = 0i32;
            let mut bracket = 0i32;
            let mut cursor = body_start;
            while cursor < close {
                let candidate = tokens[cursor];
                match candidate.kind {
                    Kind::LBrace => depth += 1,
                    Kind::RBrace => depth -= 1,
                    Kind::LParen => paren += 1,
                    Kind::RParen => paren -= 1,
                    Kind::LBracket => bracket += 1,
                    Kind::RBracket => bracket -= 1,
                    _ => {}
                }
                let at_body_level = depth == 1 && paren == 0 && bracket == 0;
                if at_body_level
                    && cursor > body_start
                    && (candidate.kind == Kind::Newline
                        || arrows.get(entry + 1) == Some(&cursor)
                        || next_significant[cursor] == arrows.get(entry + 1).copied()
                        || (candidate.kind == Kind::Word && candidate.text(source) == "else"))
                {
                    break;
                }
                if candidate.kind != Kind::Newline {
                    body_last = cursor;
                }
                cursor += 1;
            }
            // The next entry's condition starts right after this body's last token.
            let cond_start = if entry == 0 {
                next_significant[open].unwrap_or(body_start)
            } else {
                next_significant[entries[entries.len() - 1].body_last].unwrap_or(body_start)
            };
            entries.push(Entry {
                arrow,
                cond_start,
                body_start,
                body_last,
                block: tokens[body_start].kind == Kind::LBrace,
                separate_line: (arrow + 1..body_start)
                    .any(|between| tokens[between].kind == Kind::Newline),
                anchor: close,
            });
        }
        for entry in 0..entries.len() {
            if let Some(next) = entries.get(entry + 1) {
                let anchor = next.cond_start;
                entries[entry].anchor = anchor;
            }
        }
        if !entries
            .iter()
            .any(|entry| entry.block || entry.separate_line)
        {
            continue;
        }
        // Multi-statement bodies (`1 ->\nstmt()\nstmt()`) make entry boundaries
        // ambiguous; ktlint leaves such a `when` untouched. They surface as statement
        // newlines inside a condition region or as statements trailing the last body.
        let mut ambiguous = false;
        for current in &entries {
            let code_start = (current.cond_start..current.arrow).find(|&between| {
                !matches!(
                    tokens[between].kind,
                    Kind::Newline | Kind::LineComment | Kind::BlockComment
                )
            });
            if let Some(code_start) = code_start {
                let mut paren = 0i32;
                let mut bracket = 0i32;
                for candidate in &tokens[code_start..current.arrow] {
                    match candidate.kind {
                        Kind::LParen => paren += 1,
                        Kind::RParen => paren -= 1,
                        Kind::LBracket => bracket += 1,
                        Kind::RBracket => bracket -= 1,
                        Kind::Newline if paren == 0 && bracket == 0 => ambiguous = true,
                        _ => {}
                    }
                }
            }
            let next_anchor = current.anchor;
            let mut probe = next_significant[current.body_last];
            while let Some(between) = probe.filter(|&between| between < next_anchor) {
                if !matches!(tokens[between].kind, Kind::LineComment | Kind::BlockComment) {
                    ambiguous = true;
                    break;
                }
                probe = next_significant[between];
            }
        }
        if ambiguous {
            // No rewrite, but a separate-line body still takes a continuation indent.
            for current in &entries {
                if current.separate_line && !current.block {
                    for body in (current.arrow + 1..current.anchor)
                        .filter(|&body| tokens[body].kind != Kind::Newline)
                    {
                        bracing.body_extra[body] = true;
                    }
                }
            }
            continue;
        }
        for (entry, current) in entries.iter().enumerate() {
            if entry > 0 {
                bracing.blank_before[current.cond_start] = true;
            }
            if current.block {
                continue;
            }
            bracing.brace_after_arrow[current.arrow] = true;
            if current.separate_line {
                for between in arrow_newlines(tokens, current.arrow, current.body_start) {
                    bracing.drop_newline[between] = true;
                }
            }
            for extra in (current.arrow + 1..current.anchor)
                .filter(|&body| tokens[body].kind != Kind::Newline)
            {
                bracing.body_extra[extra] = true;
            }
            bracing.close_before[current.anchor] = true;
        }
    }
    bracing
}

/// Newline tokens between `arrow` and `body_start`.
fn arrow_newlines(tokens: &[Token], arrow: usize, body_start: usize) -> Vec<usize> {
    (arrow + 1..body_start)
        .filter(|&between| tokens[between].kind == Kind::Newline)
        .collect()
}

/// Whether a line-starting `get`/`set` word is a property accessor: not a call (no `.` or
/// operator before it) and the previous content line opens a `val`/`var` declaration.
fn property_accessor(
    tokens: &[Token],
    source: &str,
    previous_significant: &[Option<usize>],
    index: usize,
) -> bool {
    let Some(previous) = previous_significant[index] else {
        return false;
    };
    if matches!(tokens[previous].kind, Kind::Dot | Kind::Operator) {
        return false;
    }
    let mut line_start = previous;
    while line_start > 0 && tokens[line_start - 1].kind != Kind::Newline {
        line_start -= 1;
    }
    tokens[line_start].kind == Kind::Word
        && matches!(tokens[line_start].text(source), "val" | "var")
}

/// ktlint `parameter-list-wrapping` + `trailing-comma-on-call-site` (`ktlint_official`):
/// marks the parens of call argument lists that already span multiple lines, plus their
/// top-level argument commas. The caller treats the parens as expanded, breaks after each
/// argument comma, and inserts the trailing comma. A call with a lambda argument keeps
/// its frame untouched instead (`consume({ a ->\n...\n})` stays glued, no trailing
/// comma). Grouping parens (`val y = (1 +\n2)`) and control-flow conditions
/// (`if (\ntrue\n)`) are not call sites and stay untouched either.
struct CallSiteWrapping {
    /// Open and close parens of multiline call argument lists.
    parens: Vec<bool>,
    /// Top-level argument commas inside those lists.
    commas: Vec<bool>,
    /// Parens of multiline calls exempted for holding a lambda argument. The frame stays
    /// glued to the lambda and contributes no indent level of its own.
    lambda_frames: Vec<bool>,
}

fn multiline_call_parens(
    tokens: &[Token],
    source: &str,
    previous_significant: &[Option<usize>],
    generic_angles: &[i8],
) -> CallSiteWrapping {
    let mut wrapping = CallSiteWrapping {
        parens: vec![false; tokens.len()],
        commas: vec![false; tokens.len()],
        lambda_frames: vec![false; tokens.len()],
    };
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::LParen {
            continue;
        }
        // Signature parens are handled (wrapped or collapsed) by signature_wrapping.
        if signature_keyword(tokens, source, previous_significant, generic_angles, index).is_some()
        {
            continue;
        }
        let callee =
            previous_significant[index].is_some_and(|previous| match tokens[previous].kind {
                Kind::Word => !matches!(
                    tokens[previous].text(source),
                    "if" | "for"
                        | "while"
                        | "when"
                        | "catch"
                        | "synchronized"
                        | "return"
                        | "throw"
                        | "fun"
                        | "class"
                ),
                Kind::RParen | Kind::RBracket => true,
                _ => false,
            });
        if !callee {
            continue;
        }
        let mut paren = 0usize;
        let mut brace = 0i32;
        let mut bracket = 0i32;
        let mut angle = 0i32;
        let mut multiline = false;
        let mut lambda_argument = false;
        let mut argument_start: Option<usize> = None;
        let mut commas = Vec::new();
        let mut close = None;
        for (cursor, candidate) in tokens.iter().enumerate().skip(index) {
            let top_level = paren == 1 && brace == 0 && bracket == 0 && angle == 0;
            match candidate.kind {
                Kind::LParen => paren += 1,
                Kind::RParen => {
                    paren = paren.saturating_sub(1);
                    if paren == 0 {
                        close = Some(cursor);
                        break;
                    }
                }
                Kind::LBrace => {
                    if top_level {
                        // A `{` that opens the argument itself is a lambda. A brace
                        // after an `object`/`when`/`if` keyword is a body brace and
                        // does not exempt the call frame.
                        lambda_argument = lambda_argument || argument_start.is_none();
                    }
                    brace += 1;
                }
                Kind::RBrace => brace -= 1,
                Kind::LBracket => bracket += 1,
                Kind::RBracket => bracket -= 1,
                Kind::Comma if top_level => {
                    commas.push(cursor);
                    argument_start = None;
                }
                Kind::Newline => multiline = true,
                _ => {}
            }
            if top_level
                && argument_start.is_none()
                && !matches!(
                    candidate.kind,
                    Kind::Newline | Kind::LineComment | Kind::BlockComment | Kind::Comma
                )
            {
                argument_start = Some(cursor);
            }
            angle += i32::from(generic_angles[cursor]);
        }
        if multiline {
            if let Some(close) = close {
                if lambda_argument {
                    wrapping.lambda_frames[index] = true;
                    wrapping.lambda_frames[close] = true;
                } else {
                    // Both ends expand: the first argument leaves the `(` line and the
                    // `)` drops to its own line (with the trailing comma the caller adds).
                    wrapping.parens[index] = true;
                    wrapping.parens[close] = true;
                    for comma in commas {
                        wrapping.commas[comma] = true;
                    }
                }
            }
        }
    }
    wrapping
}

/// Marks the closing `>`/`>>` tokens of type-argument lists that span multiple lines;
/// ktlint adds a trailing comma ahead of them.
fn multiline_angle_brackets(tokens: &[Token], generic_angles: &[i8]) -> Vec<bool> {
    let mut marked = vec![false; tokens.len()];
    let mut stack: Vec<bool> = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if token.kind == Kind::Newline {
            for open in stack.iter_mut() {
                *open = true;
            }
        }
        let role = generic_angles[index];
        if role > 0 {
            stack.resize(stack.len() + usize::from(role as u8), false);
        }
        for _ in 0..-role {
            if stack.pop().unwrap_or(false) {
                marked[index] = true;
            }
        }
    }
    marked
}

/// ktlint `string-template-indent`: rewrites the whitespace before the closing `"""` of a
/// multiline raw string to `line_level` levels of `indent`. Returns `None` when the token
/// is not a multiline raw string whose last line holds only the closing delimiter.
fn reindent_raw_string_close(text: &str, indent: &str, line_level: usize) -> Option<String> {
    let (head, _) = raw_string_close_parts(text)?;
    let mut rewritten = String::with_capacity(head.len() + indent.len() * line_level + 3);
    rewritten.push_str(head);
    for _ in 0..line_level {
        rewritten.push_str(indent);
    }
    rewritten.push_str("\"\"\"");
    Some(rewritten)
}

/// Splits a multiline raw string into everything through its last line ending and the
/// whitespace before the closing `"""`; `None` when the token is some other literal or
/// the closing delimiter shares its line with content.
fn raw_string_close_parts(text: &str) -> Option<(&str, &str)> {
    let body = text.strip_suffix("\"\"\"")?;
    let newline = body.rfind('\n')?;
    let (head, spaces) = body.split_at(newline + 1);
    if !spaces.bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
        return None;
    }
    Some((head, spaces))
}

fn same_non_whitespace_tokens(
    source: &str,
    source_tokens: &[LexicalToken],
    formatted: &str,
    formatted_tokens: &[LexicalToken],
    dropped_source_spans: &[(u32, u32)],
    inserted_formatted_spans: &[(u32, u32)],
) -> bool {
    let mut source_tokens = source_tokens
        .iter()
        .copied()
        .filter(|token| !matches!(token.kind, LexicalKind::Whitespace | LexicalKind::Newline))
        .filter(|token| !dropped_source_spans.contains(&(token.span.lo, token.span.hi)));
    let mut formatted_tokens = formatted_tokens
        .iter()
        .copied()
        .filter(|token| !matches!(token.kind, LexicalKind::Whitespace | LexicalKind::Newline))
        // Braces inserted by the when-entry rewrite are intended additions.
        .filter(|token| !inserted_formatted_spans.contains(&(token.span.lo, token.span.hi)))
        .peekable();
    loop {
        match (source_tokens.next(), formatted_tokens.next()) {
            // An `=` inserted by expression-body rewriting, directly before the
            // expression's first token.
            (Some(source_token), Some(formatted_token))
                if formatted_token.text(formatted) == "="
                    && source_token.text(source) != "="
                    && formatted_tokens.peek().is_some_and(|next| {
                        next.kind == source_token.kind
                            && next.text(formatted) == source_token.text(source)
                    }) =>
            {
                formatted_tokens.next();
            }
            // A trailing comma inserted before `)` (signature or call site), `>`
            // (type-argument list), or `;`/`}` (enum entries): formatted `,` directly
            // before the closer, where the source already has the closer at this
            // position.
            (Some(source_token), Some(formatted_token))
                if formatted_token.text(formatted) == ","
                    && matches!(source_token.text(source), ")" | ">" | ";" | "}")
                    && formatted_tokens
                        .peek()
                        .is_some_and(|next| next.text(formatted) == source_token.text(source)) =>
            {
                match formatted_tokens.next() {
                    Some(closing)
                        if closing.kind == source_token.kind
                            && closing.text(formatted) == source_token.text(source) => {}
                    _ => return false,
                }
            }
            (Some(source_token), Some(formatted_token))
                if source_token.kind == formatted_token.kind
                    && (source_token.text(source) == formatted_token.text(formatted)
                        // The comment-spacing rewrite is an intended token-text change.
                        || source_token.kind == LexicalKind::LineComment
                            && spaced_line_comment(source_token.text(source))
                                == formatted_token.text(formatted)
                        // So is the raw-string closing-delimiter reindent, which touches
                        // only the whitespace before the final `"""`.
                        || matches!(
                            source_token.kind,
                            LexicalKind::Opaque | LexicalKind::Code(CoreKind::StringLit)
                        ) && raw_string_close_parts(source_token.text(source))
                            .is_some_and(|(head, _)| {
                                raw_string_close_parts(formatted_token.text(formatted))
                                    .is_some_and(|(formatted_head, _)| head == formatted_head)
                            })) => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

fn top_line(tokens: &[Token], index: usize, source: &str) -> Option<TopLine> {
    let token = tokens[index];
    match token.kind {
        Kind::AnnotationAt => Some(TopLine::Attached),
        Kind::LineComment | Kind::BlockComment => Some(TopLine::Attached),
        Kind::RBrace => Some(TopLine::Closure),
        Kind::Word => match token.text(source) {
            "package" => Some(TopLine::Package),
            "import" => Some(TopLine::Import),
            _ => tokens[index..]
                .iter()
                .take_while(|candidate| candidate.kind != Kind::Newline)
                .filter(|candidate| candidate.kind == Kind::Word)
                .find_map(|candidate| match candidate.text(source) {
                    "class" | "interface" | "object" => Some(TopLine::TypeDeclaration),
                    "typealias" => Some(TopLine::Typealias),
                    "fun" | "init" => Some(TopLine::CallableDeclaration),
                    "constructor" => Some(TopLine::Constructor),
                    "val" | "var" => Some(TopLine::Property),
                    _ => None,
                }),
        },
        _ => None,
    }
}

/// Whether the next code line after the line containing `index` is an import: `Some(true)`
/// for an import, `Some(false)` for any other code, `None` when only comments or the end
/// of the file follow.
fn next_code_line_is_import(tokens: &[Token], index: usize, source: &str) -> Option<bool> {
    let mut line_start = false;
    for token in tokens.iter().skip(index + 1) {
        match token.kind {
            Kind::Newline => line_start = true,
            Kind::LineComment | Kind::BlockComment if line_start => line_start = false,
            Kind::Word if line_start => return Some(token.text(source) == "import"),
            _ if line_start => return Some(false),
            _ => {}
        }
    }
    None
}

fn needs_top_level_blank(previous: Option<TopLine>, current: TopLine, script_style: bool) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    if script_style
        && matches!(
            (previous, current),
            (
                TopLine::TypeDeclaration | TopLine::CallableDeclaration | TopLine::Property,
                TopLine::TypeDeclaration | TopLine::CallableDeclaration | TopLine::Property
            )
        )
    {
        return false;
    }
    !matches!(
        (previous, current),
        // ktlint does not separate the package header from the import block, imports from
        // each other, consecutive property declarations, or the declaration a secondary
        // constructor or type alias attaches to; every other declaration boundary gets a
        // blank line.
        (TopLine::Package, TopLine::Import)
            | (TopLine::Import, TopLine::Import)
            | (TopLine::Import, TopLine::Attached)
            | (TopLine::Attached, TopLine::Import)
            | (TopLine::Attached, TopLine::Attached)
            | (TopLine::Property, TopLine::Property)
            | (_, TopLine::Constructor | TopLine::Typealias)
            | (
                TopLine::Attached,
                TopLine::TypeDeclaration | TopLine::CallableDeclaration | TopLine::Property
            )
            | (_, TopLine::Closure)
    )
}

fn needs_space(
    tokens: &[Token],
    source: &str,
    previous_index: usize,
    current_index: usize,
    generic_angles: &[i8],
    previous_significant: &[Option<usize>],
    enclosing_brace: Option<BraceStyle>,
) -> bool {
    let previous = tokens[previous_index];
    let current = tokens[current_index];
    let previous_text = previous.text(source);
    let current_text = current.text(source);

    if previous.kind == Kind::TemplateExpressionStart || current.kind == Kind::TemplateExpressionEnd
    {
        return false;
    }
    if previous.logical_hi != current.logical_lo
        && matches!(
            (previous_text, current_text),
            ("!", "!") | ("?", "." | ":") | (".", ".") | (":", ":")
        )
    {
        return true;
    }
    if previous.kind == Kind::LBrace {
        return enclosing_brace == Some(BraceStyle::Lambda)
            || matches!(current.kind, Kind::LineComment | Kind::BlockComment);
    }
    if matches!(
        current.kind,
        Kind::RParen | Kind::RBracket | Kind::Comma | Kind::Colon | Kind::Dot | Kind::Semicolon
    ) || matches!(
        previous.kind,
        Kind::LParen | Kind::LBracket | Kind::Dot | Kind::AnnotationAt | Kind::JumpAt
    ) {
        if previous.kind == Kind::LBrace && enclosing_brace == Some(BraceStyle::Lambda) {
            return true;
        }
        return false;
    }
    if current.kind == Kind::RBrace {
        return previous.kind != Kind::LBrace;
    }
    if current.kind == Kind::LParen {
        return previous.kind == Kind::Colon
            || previous.kind == Kind::Word
                && matches!(
                    previous_text,
                    "if" | "for" | "while" | "when" | "catch" | "synchronized"
                )
            || previous.kind == Kind::Operator
                && generic_angles[previous_index] == 0
                && !tight_operator(previous_text)
                && !unary_operator(
                    tokens,
                    source,
                    previous_index,
                    previous_significant[previous_index],
                );
    }
    if current.kind == Kind::LBracket {
        return false;
    }
    if current.kind == Kind::LBrace {
        return !matches!(
            previous.kind,
            Kind::LParen
                | Kind::LBrace
                | Kind::LBracket
                | Kind::Dot
                | Kind::AnnotationAt
                | Kind::LabelAt
                | Kind::JumpAt
        );
    }
    if matches!(previous.kind, Kind::Comma | Kind::Colon | Kind::Semicolon) {
        if previous.kind == Kind::Colon && annotation_target_colon(tokens, previous_index) {
            return false;
        }
        return true;
    }
    if previous.kind == Kind::Operator
        && current.kind == Kind::Operator
        && operators_merge(previous_text, current_text)
    {
        return true;
    }
    if previous.kind == Kind::Operator {
        if generic_angles[previous_index] != 0 {
            return generic_angles[previous_index] < 0
                && (current.kind == Kind::Word
                    && (current_text == "where" || generic_angles[previous_index] == -2)
                    || current.kind == Kind::Operator
                        && generic_angles[current_index] == 0
                        && !tight_operator(current_text));
        }
        // An assignment `=` is spaced even behind a tight postfix operator (`T? = x`).
        if current.kind == Kind::Operator && current_text == "=" {
            return true;
        }
        if tight_operator(previous_text)
            || unary_operator(
                tokens,
                source,
                previous_index,
                previous_significant[previous_index],
            )
        {
            return false;
        }
        return true;
    }
    if current.kind == Kind::Operator {
        if generic_angles[current_index] > 0 {
            return previous_text == "fun";
        }
        if generic_angles[current_index] < 0
            || tight_operator(current_text)
            || unary_operator(
                tokens,
                source,
                current_index,
                previous_significant[current_index],
            )
        {
            return false;
        }
        return true;
    }
    if matches!(current.kind, Kind::LabelAt | Kind::JumpAt) {
        return false;
    }
    if current.kind == Kind::AnnotationAt {
        return !matches!(previous.kind, Kind::LParen | Kind::LBracket | Kind::LBrace);
    }
    if previous.kind == Kind::LabelAt {
        return true;
    }
    if matches!(previous.kind, Kind::LineComment) {
        return false;
    }
    if matches!(previous.kind, Kind::BlockComment)
        || matches!(current.kind, Kind::LineComment | Kind::BlockComment)
    {
        return source[previous.hi as usize..current.lo as usize]
            .bytes()
            .any(|byte| matches!(byte, b' ' | b'\t'));
    }
    is_atom(previous.kind) && is_atom(current.kind)
        || matches!(previous.kind, Kind::RParen | Kind::RBracket | Kind::RBrace)
            && is_atom(current.kind)
}

fn annotation_target_colon(tokens: &[Token], index: usize) -> bool {
    let Some(word_index) = previous_on_line(tokens, index) else {
        return false;
    };
    let Some(at_index) = previous_on_line(tokens, word_index) else {
        return false;
    };
    tokens[word_index].kind == Kind::Word && tokens[at_index].kind == Kind::AnnotationAt
}

fn classify_braces(tokens: &[Token], source: &str) -> BraceStyles {
    let mut styles = vec![BraceStyle::Normal; tokens.len()];
    let mut class_body = vec![false; tokens.len()];
    let mut paren_stack = Vec::new();
    let mut paren_open = vec![None; tokens.len()];
    let mut declaration_header = false;
    let mut classifier_header = false;
    let mut header_before = vec![false; tokens.len()];
    let mut classifier_before = vec![false; tokens.len()];

    for (index, token) in tokens.iter().copied().enumerate() {
        header_before[index] = declaration_header && paren_stack.is_empty();
        classifier_before[index] = classifier_header && paren_stack.is_empty();
        match token.kind {
            Kind::LParen => paren_stack.push(index),
            Kind::RParen => {
                if let Some(open) = paren_stack.pop() {
                    paren_open[index] = Some(open);
                }
            }
            Kind::LBrace | Kind::RBrace | Kind::Semicolon if paren_stack.is_empty() => {
                declaration_header = false;
                classifier_header = false;
            }
            Kind::Operator if token.text(source) == "=" && paren_stack.is_empty() => {
                declaration_header = false;
                classifier_header = false;
            }
            Kind::Word if matches!(token.text(source), "class" | "interface" | "object") => {
                declaration_header = true;
                classifier_header = true;
            }
            Kind::Word
                if matches!(
                    token.text(source),
                    "fun" | "constructor" | "get" | "set" | "init"
                ) =>
            {
                declaration_header = true;
                classifier_header = false;
            }
            _ => {}
        }
    }

    for (index, token) in tokens.iter().copied().enumerate() {
        if token.kind != Kind::LBrace {
            continue;
        }
        let Some(previous) = previous_on_line(tokens, index) else {
            styles[index] = BraceStyle::Expanded;
            continue;
        };
        let previous_token = tokens[previous];
        let word_starts_block = previous_token.kind == Kind::Word
            && matches!(
                previous_token.text(source),
                "else"
                    | "try"
                    | "finally"
                    | "do"
                    | "when"
                    | "class"
                    | "interface"
                    | "object"
                    | "init"
                    | "companion"
            );
        let paren_starts_block = (previous_token.kind == Kind::RParen)
            .then(|| paren_open[previous])
            .flatten()
            .and_then(|open| previous_on_line(tokens, open))
            .is_some_and(|before| {
                tokens[before].kind == Kind::Word
                    && matches!(
                        tokens[before].text(source),
                        "if" | "for" | "while" | "when" | "catch" | "constructor" | "get" | "set"
                    )
            });
        let lambda_context = !header_before[index]
            && !word_starts_block
            && !paren_starts_block
            && (matches!(
                previous_token.kind,
                Kind::Word
                    | Kind::RParen
                    | Kind::RBracket
                    | Kind::Comma
                    | Kind::LParen
                    | Kind::LabelAt
            ) || previous_token.kind == Kind::Operator && previous_token.text(source) == "=");
        styles[index] = if lambda_context {
            BraceStyle::Lambda
        } else {
            BraceStyle::Expanded
        };
        class_body[index] = header_before[index] && classifier_before[index];
    }
    BraceStyles { styles, class_body }
}

fn classify_expanded_parens(tokens: &[Token], generic_angles: &[i8]) -> Vec<bool> {
    let mut expanded = vec![false; tokens.len()];
    let mut stack = Vec::new();
    let mut newlines = 0usize;
    let mut generic_opens = 0usize;
    for (index, token) in tokens.iter().copied().enumerate() {
        match token.kind {
            Kind::LParen => stack.push((index, newlines, generic_opens)),
            Kind::RParen => {
                let Some((open, open_newlines, open_generics)) = stack.pop() else {
                    continue;
                };
                if newlines > open_newlines && generic_opens > open_generics {
                    expanded[open] = true;
                    expanded[index] = true;
                }
            }
            Kind::Newline => newlines = newlines.saturating_add(1),
            _ => {}
        }
        if generic_angles[index] > 0 {
            generic_opens = generic_opens.saturating_add(1);
        }
    }
    expanded
}

fn previous_on_line(tokens: &[Token], index: usize) -> Option<usize> {
    let previous = index.checked_sub(1)?;
    (tokens[previous].kind != Kind::Newline).then_some(previous)
}

fn is_atom(kind: Kind) -> bool {
    matches!(kind, Kind::Word | Kind::Literal)
}

fn tight_operator(operator: &str) -> bool {
    matches!(
        operator,
        "." | "?." | "::" | "?" | "!!" | "++" | "--" | ".." | "..<"
    )
}

fn unary_operator(
    tokens: &[Token],
    source: &str,
    index: usize,
    previous_significant: Option<usize>,
) -> bool {
    let operator = tokens[index].text(source);
    if operator == "!" {
        return true;
    }
    if !matches!(operator, "+" | "-" | "*") {
        return false;
    }
    let previous = previous_significant.map(|previous| tokens[previous]);
    previous.is_none_or(|previous| {
        matches!(
            previous.kind,
            Kind::LParen
                | Kind::LBracket
                | Kind::LBrace
                | Kind::Comma
                | Kind::Colon
                | Kind::Semicolon
                | Kind::Operator
        )
    })
}

fn previous_significant_tokens(tokens: &[Token]) -> Vec<Option<usize>> {
    let mut result = Vec::with_capacity(tokens.len());
    let mut previous = None;
    for (index, token) in tokens.iter().enumerate() {
        result.push(previous);
        if token.kind != Kind::Newline {
            previous = Some(index);
        }
    }
    result
}

fn next_significant_tokens(tokens: &[Token]) -> Vec<Option<usize>> {
    let mut result = vec![None; tokens.len()];
    let mut next = None;
    for (index, token) in tokens.iter().enumerate().rev() {
        result[index] = next;
        if token.kind != Kind::Newline {
            next = Some(index);
        }
    }
    result
}

fn generic_angle_roles(
    tokens: &[Token],
    source: &str,
    next_significant: &[Option<usize>],
) -> Vec<i8> {
    let mut disallowed = Vec::with_capacity(tokens.len() + 1);
    disallowed.push(0u32);
    for token in tokens {
        let count = disallowed.last().copied().unwrap_or(0).saturating_add(
            (token.kind == Kind::Operator
                && !matches!(token.text(source), "<" | ">" | "?" | "*" | "&" | "->"))
                as u32,
        );
        disallowed.push(count);
    }

    let mut roles = vec![0i8; tokens.len()];
    let mut stack = Vec::new();
    for (index, token) in tokens.iter().copied().enumerate() {
        if token.kind != Kind::Operator {
            continue;
        }
        match token.text(source) {
            "<" => stack.push(index),
            ">" => {
                let Some(open) = stack.pop() else {
                    continue;
                };
                if let Some(role) =
                    generic_angle_pair(tokens, source, &disallowed, next_significant, open, index)
                {
                    roles[open] = role;
                    roles[index] = -role;
                }
            }
            _ => {}
        }
    }
    roles
}

fn generic_angle_pair(
    tokens: &[Token],
    source: &str,
    disallowed: &[u32],
    next_significant: &[Option<usize>],
    open: usize,
    close: usize,
) -> Option<i8> {
    let previous = previous_on_line(tokens, open)?;
    let next = tokens[next_significant.get(open).copied().flatten()?];
    if tokens[previous].kind != Kind::Word
        || !matches!(
            next.kind,
            Kind::Word | Kind::Operator | Kind::LParen | Kind::AnnotationAt
        )
        || next.kind == Kind::Operator && next.text(source) != "*"
        || disallowed[close] != disallowed[open + 1]
    {
        return None;
    }

    let declaration_or_type_context = tokens[previous].text(source) == "fun"
        || previous_on_line(tokens, previous).is_some_and(|before| {
            tokens[before].kind == Kind::Colon
                || tokens[before].kind == Kind::Word
                    && matches!(
                        tokens[before].text(source),
                        "class" | "interface" | "typealias" | "fun" | "is" | "as"
                    )
        });
    let adjacent = tokens[previous].logical_hi == tokens[open].logical_lo;
    if !adjacent && !declaration_or_type_context && tokens[previous].text(source) != "fun" {
        return None;
    }

    let after = tokens.get(close + 1);
    if after.is_some_and(|after| {
        after.kind == Kind::Word && after.text(source) != "where" && !declaration_or_type_context
    }) {
        return None;
    }
    Some(if tokens[previous].text(source) == "fun" {
        2
    } else {
        1
    })
}

struct FormattingTokens {
    tokens: Vec<Token>,
    lexical: Vec<LexicalToken>,
    prefix_markers: Vec<AttachedMarker>,
    suffix_markers: Vec<AttachedMarker>,
    trailing_markers: Vec<LexicalToken>,
}

fn formatting_tokens(source: &str) -> Option<FormattingTokens> {
    let mut diagnostics = DiagSink::new();
    let lexical = lex_formatting_tokens(source, &mut diagnostics, MAX_FORMATTING_TOKENS)?;
    let mut tokens = Vec::with_capacity(lexical.len());
    let mut prefix_markers = Vec::new();
    let mut suffix_markers = Vec::new();
    let mut pending_prefix_markers = Vec::new();
    let mut marker_bytes = 0u32;
    let mut nesting = 0usize;
    for token in &lexical {
        let kind = match token.kind {
            LexicalKind::Whitespace => continue,
            LexicalKind::MarkerOpen => {
                pending_prefix_markers.push(*token);
                marker_bytes = marker_bytes.checked_add(token.span.hi - token.span.lo)?;
                continue;
            }
            LexicalKind::MarkerClose => {
                if !pending_prefix_markers.is_empty() {
                    pending_prefix_markers.push(*token);
                } else if let Some(token_index) = tokens.len().checked_sub(1) {
                    suffix_markers.push(AttachedMarker {
                        token_index,
                        lo: token.span.lo,
                        hi: token.span.hi,
                    });
                } else {
                    pending_prefix_markers.push(*token);
                }
                marker_bytes = marker_bytes.checked_add(token.span.hi - token.span.lo)?;
                continue;
            }
            LexicalKind::Newline => Kind::Newline,
            LexicalKind::LineComment => Kind::LineComment,
            LexicalKind::BlockComment => Kind::BlockComment,
            LexicalKind::Shebang => Kind::Shebang,
            LexicalKind::Opaque => Kind::Literal,
            LexicalKind::TemplateExpressionStart => Kind::TemplateExpressionStart,
            LexicalKind::TemplateExpressionEnd => Kind::TemplateExpressionEnd,
            LexicalKind::Code(kind) => formatting_kind(
                kind,
                &tokens,
                source,
                token.span.lo.checked_sub(marker_bytes)?,
            )?,
        };
        let token_index = tokens.len();
        prefix_markers.extend(
            pending_prefix_markers
                .drain(..)
                .map(|marker| AttachedMarker {
                    token_index,
                    lo: marker.span.lo,
                    hi: marker.span.hi,
                }),
        );
        match kind {
            Kind::LParen | Kind::LBrace | Kind::LBracket => {
                nesting = nesting.checked_add(1)?;
                if nesting > MAX_FORMATTING_NESTING {
                    return None;
                }
            }
            Kind::RParen | Kind::RBrace | Kind::RBracket => {
                nesting = nesting.saturating_sub(1);
            }
            _ => {}
        }
        tokens.push(Token {
            kind,
            lo: token.span.lo,
            hi: token.span.hi,
            logical_lo: token.span.lo.checked_sub(marker_bytes)?,
            logical_hi: token.span.hi.checked_sub(marker_bytes)?,
        });
    }
    Some(FormattingTokens {
        tokens,
        lexical,
        prefix_markers,
        suffix_markers,
        trailing_markers: pending_prefix_markers,
    })
}

fn formatting_kind(
    kind: CoreKind,
    tokens: &[Token],
    source: &str,
    logical_lo: u32,
) -> Option<Kind> {
    use CoreKind::*;
    Some(match kind {
        Ident | KwFun | KwClass | KwVal | KwVar | KwReturn | KwIf | KwElse | KwWhen | KwWhile
        | KwDo | KwFor | KwIn | KwTrue | KwFalse | KwNull | KwPackage | KwImport | Unknown => {
            Kind::Word
        }
        IntLit | LongLit | UIntLit | ULongLit | DoubleLit | FloatLit | StringLit | CharLit
        | TemplateStart | RawTemplateStart | TemplateEnd | StrChunk | Dollar => Kind::Literal,
        LParen => Kind::LParen,
        RParen => Kind::RParen,
        LBrace => Kind::LBrace,
        RBrace => Kind::RBrace,
        LBracket => Kind::LBracket,
        RBracket => Kind::RBracket,
        Comma => Kind::Comma,
        Colon => Kind::Colon,
        Dot => Kind::Dot,
        At => at_kind(tokens, source, logical_lo),
        Newline => Kind::Semicolon,
        Eof => return None,
        Eq | Plus | Minus | Star | Slash | Percent | EqEq | NotEq | RefEq | RefNe | Lt | LtEq
        | Gt | GtEq | Amp | AndAnd | OrOr | Not | Arrow | DotDot | DotDotLt | PlusPlus
        | MinusMinus | PlusEq | MinusEq | StarEq | SlashEq | PercentEq | ColonColon | Question => {
            Kind::Operator
        }
    })
}

fn at_kind(tokens: &[Token], source: &str, logical_start: u32) -> Kind {
    let Some(previous) = tokens
        .last()
        .copied()
        .filter(|token| token.kind != Kind::Newline && token.logical_hi == logical_start)
    else {
        return Kind::AnnotationAt;
    };
    if previous.kind != Kind::Word {
        return Kind::AnnotationAt;
    }
    if matches!(
        previous.text(source),
        "return" | "break" | "continue" | "this" | "super"
    ) {
        Kind::JumpAt
    } else {
        Kind::LabelAt
    }
}

fn operators_merge(left: &str, right: &str) -> bool {
    matches!(
        (left, right),
        ("/", "/" | "*" | "=")
            | ("+", "+" | "=")
            | ("-", "-" | ">" | "=")
            | ("*", "=")
            | ("%", "=")
            | ("=", "=")
            | ("==", "=")
            | ("!", "=")
            | ("!=", "=")
            | ("<" | ">", "=")
            | ("&", "&")
            | ("|", "|")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(source: &str) -> Option<String> {
        format_kotlin(
            source,
            FormattingOptions {
                tab_size: 4,
                insert_spaces: true,
                ..FormattingOptions::default()
            },
        )
    }

    #[test]
    fn drops_redundant_semicolons_but_keeps_statement_separators() {
        let source = "fun f() {\nval x = 1;\nval y = 2; val z = 3;\n}\n";
        let once = format(source).unwrap();
        assert_eq!(
            once,
            "fun f() {\n    val x = 1\n    val y = 2; val z = 3\n}\n"
        );
        assert_eq!(format(&once).as_deref(), Some(once.as_str()));
    }

    #[test]
    fn keeps_enum_entry_semicolon_inside_enum_body() {
        // Byte-identical to ktlint 1.8.0 (ktlint_official): entries split one per line
        // with a trailing comma, the entries `;` sits on its own line, and a blank line
        // separates it from the member declarations.
        let source = "enum class E {\nA;\nfun f() {}\n}\n";
        let once = format(source).unwrap();
        assert_eq!(once, "enum class E {\n    A,\n    ;\n\n    fun f() {}\n}\n");
        assert_eq!(format(&once).as_deref(), Some(once.as_str()));
    }

    #[test]
    fn formats_common_kotlin_spacing_indentation_and_top_level_separation() {
        let source = "package formattingparity\nclass Box{\nfun sum(left:Int,right:Int):Int{\nreturn left+right\n}\n}\nfun use( ){\nval box=Box( )\nprintln(box.sum(1,2))\n}\n";
        // Byte-identical to ktlint 1.8.0 (ktlint_official): the two-parameter signature is
        // wrapped with a trailing comma and the single-return block body becomes an
        // expression body.
        assert_eq!(
            format(source).as_deref(),
            Some(
                "package formattingparity\n\nclass Box {\n    fun sum(\n        left: Int,\n        right: Int,\n    ): Int = left + right\n}\n\nfun use() {\n    val box = Box()\n    println(box.sum(1, 2))\n}\n"
            )
        );
    }

    #[test]
    fn keeps_comment_and_literal_contents_opaque() {
        let source = "fun sample( ){\nval text=\"{a+b}\" // keep  a+b  \n/* keep  x+y /* nested */ */println(text)\nval raw=\"\"\"a\"\"\"\"\n}\n";
        assert_eq!(
            format(source).as_deref(),
            Some(
                "fun sample() {\n    val text = \"{a+b}\" // keep  a+b  \n    /* keep  x+y /* nested */ */println(text)\n    val raw = \"\"\"a\"\"\"\"\n}\n"
            )
        );
    }

    #[test]
    fn formatting_is_idempotent_and_keeps_generic_angles_tight() {
        let source =
            "class box<t> {\n    fun value(input: list<string>): list<string> = input\n}\n";
        let once = format(source).unwrap();
        assert_eq!(format(&once).as_deref(), Some(once.as_str()));
        assert_eq!(once, source);
    }

    #[test]
    fn nested_object_calls_converge_before_the_public_result_is_returned() {
        let source = "fun nested(){consume(object{fun value()=call(object{fun inner()=1})})}\n";
        let formatted = format(source).unwrap();
        // ktlint parameter-list-wrapping: a multiline call breaks after its `(` and the
        // object argument drops to its own line.
        assert!(formatted.contains("consume(\n"), "{formatted:?}");
        assert!(formatted.contains("call(\n"), "{formatted:?}");
        assert!(formatted.contains("object {"), "{formatted:?}");
        assert_eq!(format(&formatted).as_deref(), Some(formatted.as_str()));
    }

    #[test]
    fn formats_authoritative_template_expression_tokens_without_touching_template_text() {
        let source = "fun template(a:Int,b:Int){val text=\"sum=${a+b}; raw=$a\"}\n";
        let formatted = format(source).unwrap();
        assert!(formatted.contains("\"sum=${a + b}; raw=$a\""));
        assert_eq!(format(&formatted).as_deref(), Some(formatted.as_str()));
    }

    #[test]
    fn keeps_comma_and_nested_diagnostic_marker_boundaries_exact() {
        let source = "fun marked(){val comma=listOf(1<!COMMA!>,<!>2);val nested=<!OUTER!>call(<!INNER!>object{fun value()=1}<!>)<!>}\n";
        let formatted = format(source).unwrap();
        assert!(formatted.contains("listOf(1<!COMMA!>,<!> 2)"));
        // ktlint parameter-list-wrapping: the multiline call breaks after its `(` and the
        // object argument drops to its own line, marker glued.
        assert!(formatted.contains("<!OUTER!>call(\n"), "{formatted:?}");
        assert!(formatted.contains("<!INNER!>object {"), "{formatted:?}");
        // ktlint expands the anonymous-object body and adds a call-site trailing comma;
        // the markers must stay glued to their tokens across that rewrite.
        assert!(formatted.contains("}<!>,"), "{formatted:?}");
        assert!(formatted.contains(")<!>"), "{formatted:?}");
        assert_eq!(
            formatted.matches("<!COMMA!>").count(),
            source.matches("<!COMMA!>").count()
        );
        assert_eq!(
            formatted.matches("<!>").count(),
            source.matches("<!>").count()
        );
        assert_eq!(format(&formatted).as_deref(), Some(formatted.as_str()));
    }

    #[test]
    fn keeps_adjacent_binary_and_unary_operators_lexically_stable() {
        let source =
            "fun operators(a:Int,b:Int){\nval x=a - -b\nval y=a + +b\nval z=(- -b)+(+ +b)\n}\n";
        let once = format(source).unwrap();
        assert!(once.contains("a - -b"));
        assert!(once.contains("a + +b"));
        assert!(once.contains("(- -b) + (+ +b)"), "{once:?}");
        assert_eq!(format(&once).as_deref(), Some(once.as_str()));
        let source_tokens = formatting_tokens(source).unwrap();
        let formatted_tokens = formatting_tokens(&once).unwrap();
        assert!(same_non_whitespace_tokens(
            source,
            &source_tokens.lexical,
            &once,
            &formatted_tokens.lexical,
            &[],
            &[]
        ));
    }

    #[test]
    fn honors_indentation_options_without_unbounded_zero_width_work() {
        let source = "fun options(){\nval value=1\n}\n";
        let spaces = format_kotlin(
            source,
            FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                ..FormattingOptions::default()
            },
        )
        .unwrap();
        let tabs = format_kotlin(
            source,
            FormattingOptions {
                tab_size: 8,
                insert_spaces: false,
                ..FormattingOptions::default()
            },
        )
        .unwrap();
        let zero = format_kotlin(
            source,
            FormattingOptions {
                tab_size: 0,
                insert_spaces: true,
                ..FormattingOptions::default()
            },
        )
        .unwrap();
        assert!(spaces.contains("\n  val value"));
        assert!(tabs.contains("\n\tval value"));
        assert!(zero.contains("\nval value"));
        assert_eq!(
            format_kotlin(
                &zero,
                FormattingOptions {
                    tab_size: 0,
                    insert_spaces: true,
                    ..FormattingOptions::default()
                }
            )
            .as_deref(),
            Some(zero.as_str())
        );
    }

    #[test]
    fn honors_optional_whitespace_and_final_newline_options() {
        let source = "fun options(){  \nval value=1 // note  \n}\n\n\n";
        let preserved = format_kotlin(
            source,
            FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                ..FormattingOptions::default()
            },
        )
        .unwrap();
        let trimmed = format_kotlin(
            source,
            FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                trim_trailing_whitespace: true,
                insert_final_newline: true,
                trim_final_newlines: true,
            },
        )
        .unwrap();
        assert!(preserved.contains("{  \n"));
        assert!(preserved.contains("value = 1 // note  \n"));
        assert!(preserved.ends_with("\n\n"));
        assert_eq!(trimmed, "fun options() {\n  val value = 1 // note\n}\n");

        let inserted = format_kotlin(
            "val value=1",
            FormattingOptions {
                tab_size: 4,
                insert_spaces: true,
                insert_final_newline: true,
                ..FormattingOptions::default()
            },
        )
        .unwrap();
        assert_eq!(inserted, "val value = 1\n");
    }

    #[test]
    fn preserves_ktlint_single_trailing_tab_quirk() {
        let options = FormattingOptions {
            tab_size: 4,
            insert_spaces: true,
            trim_trailing_whitespace: true,
            insert_final_newline: true,
            trim_final_newlines: true,
        };
        assert_eq!(
            format_kotlin("val value=1\t", options).as_deref(),
            Some("val value = 1\t\n")
        );
    }

    #[test]
    fn preserves_crlf_for_existing_and_inserted_line_endings() {
        let source = "fun sample( ){\r\nval raw=\"\"\"a\r\nb\"\"\"\r\nprintln(raw)\r\n}\r\n";
        let formatted = format(source).unwrap();
        assert_eq!(
            formatted,
            "fun sample() {\r\n    val raw = \"\"\"a\r\nb\"\"\"\r\n    println(raw)\r\n}\r\n"
        );
        assert!(!formatted
            .replace("\r\n", "")
            .bytes()
            .any(|byte| matches!(byte, b'\r' | b'\n')));
    }

    #[test]
    fn keeps_unterminated_literals_opaque() {
        let source = "fun sample( ){val text=\"a+b";
        assert_eq!(
            format(source).as_deref(),
            Some("fun sample() {\n    val text = \"a+b")
        );
    }

    #[test]
    fn preserves_shebangs_exponents_labels_and_annotation_use_sites() {
        let source = "#!/usr/bin/env kotlin\n@get:Deprecated(\"old\")\nval rate:Double=1e-3\nfun labels( ){\nloop@ for(value in listOf(1)){\nif(value>0)break@loop\n}\n}\n";
        let formatted = format(source).unwrap();
        assert!(formatted.starts_with("#!/usr/bin/env kotlin\n"));
        assert!(formatted.contains("@get:Deprecated(\"old\")"));
        assert!(formatted.contains("val rate: Double = 1e-3"));
        assert!(formatted.contains("loop@ for (value in listOf(1)) {"));
        assert!(formatted.contains("break@loop"));
    }

    #[test]
    fn expands_blocks_but_keeps_trailing_lambdas_inline() {
        let source = "fun block(){println(\"x\")}\nfun trailing(values:List<Int>)=values.map{value->value+1}\nfun choose(value:Int)=when{value>0->1}\n";
        assert_eq!(
            format(source).as_deref(),
            Some(
                "fun block() {\n    println(\"x\")\n}\n\nfun trailing(values: List<Int>) = values.map { value -> value + 1 }\n\nfun choose(value: Int) =\n    when {\n        value > 0 -> 1\n    }\n"
            )
        );
    }

    #[test]
    fn keeps_default_values_in_function_headers_and_indents_multiline_generics() {
        let source = "fun defaulted(x:Int=1){println(x)}\nfun defaultLambda(block:()->Unit={println(\"default\")}){block()}\nfun generic(x:Map<\nString,\nInt\n>){println(x)}\n";
        // Byte-identical to ktlint 1.8.0 (ktlint_official): the multiline type-argument
        // list gains a trailing comma, dedents its `>`, and keeps the header expanded.
        assert_eq!(
            format(source).as_deref(),
            Some(
                "fun defaulted(x: Int = 1) {\n    println(x)\n}\n\nfun defaultLambda(block: () -> Unit = { println(\"default\") }) {\n    block()\n}\n\nfun generic(\n    x: Map<\n        String,\n        Int,\n    >,\n) {\n    println(x)\n}\n"
            )
        );
    }

    #[test]
    fn rejects_inputs_over_the_retained_text_budget() {
        let source = " ".repeat(MAX_FORMATTING_INPUT_BYTES + 1);
        assert!(format(&source).is_none());
    }

    #[test]
    fn rejects_template_expansion_over_token_or_nesting_bounds() {
        let interpolations = format!(
            "val text=\"{}\"",
            "${value}".repeat(MAX_FORMATTING_TOKENS / 4)
        );
        assert!(format(&interpolations).is_none());

        let mut nested = "value".to_string();
        for _ in 0..=128 {
            nested = format!("\"${{{nested}}}\"");
        }
        assert!(format(&nested).is_none());
    }

    #[test]
    fn rejects_structural_nesting_that_would_make_layout_scans_quadratic() {
        let source = format!(
            "val value = {}0{}\n",
            "f(".repeat(MAX_FORMATTING_NESTING + 1),
            ")".repeat(MAX_FORMATTING_NESTING + 1)
        );
        assert!(format(&source).is_none());
    }
}

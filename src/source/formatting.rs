use crate::diag::DiagSink;
use crate::lexer::{
    lex_formatting_tokens, FormattingToken as LexicalToken, FormattingTokenKind as LexicalKind,
};
use crate::token::TokenKind as CoreKind;

const MAX_FORMATTING_INPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_FORMATTING_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_FORMATTING_TOKENS: usize = 256 * 1024;
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
    Attached,
    Closure,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BraceStyle {
    Normal,
    Lambda,
    Expanded,
}

pub fn format_kotlin(source: &str, options: FormattingOptions) -> Option<String> {
    if source.len() > MAX_FORMATTING_INPUT_BYTES {
        return None;
    }
    let mut formatted = source.to_string();
    for _ in 0..MAX_LAYOUT_PASSES {
        let next = format_once(&formatted, options)?;
        if next == formatted {
            return Some(next);
        }
        formatted = next;
    }
    None
}

fn format_once(source: &str, options: FormattingOptions) -> Option<String> {
    let source_scan = formatting_tokens(source)?;
    let tokens = &source_scan.tokens;
    let indent = indentation(options)?;
    let line_ending = source_line_ending(source, tokens);
    let previous_significant = previous_significant_tokens(tokens);
    let next_significant = next_significant_tokens(tokens);
    let generic_angles = generic_angle_roles(tokens, source, &next_significant);
    let token_brace_styles = classify_braces(tokens, source);
    let expanded_parens = classify_expanded_parens(tokens, &generic_angles);
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
    let mut prefix_marker_index = 0usize;
    let mut suffix_marker_index = 0usize;

    for (index, token) in tokens.iter().copied().enumerate() {
        if token.kind == Kind::Newline {
            if !options.trim_trailing_whitespace {
                preserve_trailing_whitespace(&mut output, source, tokens, index)?;
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

        let closing_style = if token.kind == Kind::RBrace {
            let style = brace_style_stack.pop().unwrap_or(BraceStyle::Normal);
            if style == BraceStyle::Expanded && !line_start {
                push(&mut output, line_ending)?;
                line_start = true;
                previous = None;
            }
            brace_depth = brace_depth.saturating_sub(1);
            Some(style)
        } else {
            None
        };
        if token.kind == Kind::RParen && expanded_parens[index] && !line_start {
            push(&mut output, line_ending)?;
            line_start = true;
            previous = None;
        }
        if token.kind == Kind::RParen {
            paren_depth = paren_depth.saturating_sub(1);
        } else if token.kind == Kind::RBracket {
            bracket_depth = bracket_depth.saturating_sub(1);
        }

        let first_on_line = line_start;
        if first_on_line {
            if brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 {
                if let Some(current) = top_line(tokens, index, source) {
                    if needs_top_level_blank(previous_top_line, current, script_style) {
                        ensure_blank_line(&mut output, line_ending)?;
                    }
                    previous_top_line = Some(current);
                }
            }
            push_indentation(
                &mut output,
                &indent,
                brace_depth
                    .saturating_add(paren_depth)
                    .saturating_add(bracket_depth)
                    .saturating_add(generic_depth.saturating_mul(2)),
            )?;
            line_start = false;
        } else if let Some(previous_index) = previous {
            let previous_token = tokens[previous_index];
            if needs_space(
                tokens,
                source,
                previous_index,
                index,
                &generic_angles,
                &previous_significant,
                brace_style_stack.last().copied(),
            ) {
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
        push(&mut output, token.text(source))?;
        push_markers(
            &mut output,
            source,
            &source_scan.suffix_markers,
            &mut suffix_marker_index,
            index,
        )?;

        if token.kind == Kind::LBrace {
            brace_style_stack.push(token_brace_styles[index]);
            brace_depth = brace_depth.saturating_add(1);
        } else if token.kind == Kind::LParen {
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
            && tokens
                .get(index + 1)
                .is_some_and(|next| !matches!(next.kind, Kind::Newline | Kind::RBrace))
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

        if first_on_line && token.kind == Kind::RBrace && brace_depth == 0 {
            previous_top_line = Some(TopLine::Closure);
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
    }
    apply_final_newline_options(&mut output, line_ending, options)?;
    let formatted_scan = formatting_tokens(&output)?;
    (output.len() <= MAX_FORMATTING_OUTPUT_BYTES
        && same_non_whitespace_tokens(
            source,
            &source_scan.lexical,
            &output,
            &formatted_scan.lexical,
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

fn preserve_eof_whitespace(output: &mut String, source: &str, tokens: &[Token]) -> Option<()> {
    let start = tokens.last().map_or(0, |token| token.hi as usize);
    push(output, horizontal_suffix(&source[start..]))
}

fn horizontal_suffix(text: &str) -> &str {
    let prefix = text.trim_end_matches(|character| matches!(character, ' ' | '\t'));
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

fn same_non_whitespace_tokens(
    source: &str,
    source_tokens: &[LexicalToken],
    formatted: &str,
    formatted_tokens: &[LexicalToken],
) -> bool {
    let mut source_tokens = source_tokens
        .iter()
        .copied()
        .filter(|token| !matches!(token.kind, LexicalKind::Whitespace | LexicalKind::Newline));
    let mut formatted_tokens = formatted_tokens
        .iter()
        .copied()
        .filter(|token| !matches!(token.kind, LexicalKind::Whitespace | LexicalKind::Newline));
    loop {
        match (source_tokens.next(), formatted_tokens.next()) {
            (Some(source_token), Some(formatted_token))
                if source_token.kind == formatted_token.kind
                    && source_token.text(source) == formatted_token.text(formatted) => {}
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
                    "class" | "interface" | "object" | "typealias" => {
                        Some(TopLine::TypeDeclaration)
                    }
                    "fun" | "val" | "var" => Some(TopLine::CallableDeclaration),
                    _ => None,
                }),
        },
        _ => None,
    }
}

fn needs_top_level_blank(previous: Option<TopLine>, current: TopLine, script_style: bool) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    if script_style
        && matches!(
            (previous, current),
            (
                TopLine::TypeDeclaration | TopLine::CallableDeclaration,
                TopLine::TypeDeclaration | TopLine::CallableDeclaration
            )
        )
    {
        return false;
    }
    !matches!(
        (previous, current),
        (TopLine::Import, TopLine::Import)
            | (TopLine::TypeDeclaration, TopLine::TypeDeclaration)
            | (TopLine::Attached, TopLine::Attached)
            | (
                TopLine::Attached,
                TopLine::TypeDeclaration | TopLine::CallableDeclaration
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
        return enclosing_brace == Some(BraceStyle::Lambda);
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

fn classify_braces(tokens: &[Token], source: &str) -> Vec<BraceStyle> {
    let mut styles = vec![BraceStyle::Normal; tokens.len()];
    let mut paren_stack = Vec::new();
    let mut paren_open = vec![None; tokens.len()];
    let mut declaration_header = false;
    let mut header_before = vec![false; tokens.len()];

    for (index, token) in tokens.iter().copied().enumerate() {
        header_before[index] = declaration_header && paren_stack.is_empty();
        match token.kind {
            Kind::LParen => paren_stack.push(index),
            Kind::RParen => {
                if let Some(open) = paren_stack.pop() {
                    paren_open[index] = Some(open);
                }
            }
            Kind::LBrace | Kind::RBrace | Kind::Semicolon if paren_stack.is_empty() => {
                declaration_header = false;
            }
            Kind::Operator if token.text(source) == "=" && paren_stack.is_empty() => {
                declaration_header = false;
            }
            Kind::Word
                if matches!(
                    token.text(source),
                    "fun"
                        | "class"
                        | "interface"
                        | "object"
                        | "constructor"
                        | "get"
                        | "set"
                        | "init"
                ) =>
            {
                declaration_header = true;
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
    }
    styles
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
    fn formats_common_kotlin_spacing_indentation_and_top_level_separation() {
        let source = "package formattingparity\nclass Box{\nfun sum(left:Int,right:Int):Int{\nreturn left+right\n}\n}\nfun use( ){\nval box=Box( )\nprintln(box.sum(1,2))\n}\n";
        assert_eq!(
            format(source).as_deref(),
            Some(
                "package formattingparity\n\nclass Box {\n    fun sum(left: Int, right: Int): Int {\n        return left + right\n    }\n}\n\nfun use() {\n    val box = Box()\n    println(box.sum(1, 2))\n}\n"
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
        assert!(formatted.contains("consume(object {"));
        assert!(formatted.contains("call(object {"));
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
        assert!(formatted.contains("<!OUTER!>call(<!INNER!>object {"));
        assert!(formatted.contains("}<!>)<!>"));
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
            &formatted_tokens.lexical
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
                "fun block() {\n    println(\"x\")\n}\n\nfun trailing(values: List<Int>) = values.map { value -> value + 1 }\n\nfun choose(value: Int) = when {\n    value > 0 -> 1\n}\n"
            )
        );
    }

    #[test]
    fn keeps_default_values_in_function_headers_and_indents_multiline_generics() {
        let source = "fun defaulted(x:Int=1){println(x)}\nfun defaultLambda(block:()->Unit={println(\"default\")}){block()}\nfun generic(x:Map<\nString,\nInt\n>){println(x)}\n";
        assert_eq!(
            format(source).as_deref(),
            Some(
                "fun defaulted(x: Int = 1) {\n    println(x)\n}\n\nfun defaultLambda(block: () -> Unit = { println(\"default\") }) {\n    block()\n}\n\nfun generic(\n    x: Map<\n            String,\n            Int\n            >\n) {\n    println(x)\n}\n"
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
}

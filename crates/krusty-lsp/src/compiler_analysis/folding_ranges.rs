//! Foldable source regions extracted while the compiler AST is short-lived.

use std::collections::HashSet;

use krusty::ast::{ClassDecl, Decl, FunBody, FunDecl, Stmt};
use krusty::diag::Span;

use super::{
    companion_class,
    source_scan::{
        matching_delimiter, skip_block_comment, skip_line_comment, skip_quoted, skip_trivia,
        utf8_char_len,
    },
    FileAnalysis,
};

const MAX_DELIMITER_DEPTH: usize = 256;
const MAX_DYNAMIC_PLACEHOLDER_BYTES: usize = 4 * 1024;

pub(crate) const FOLDING_KIND_COMMENT: u8 = 0;
pub(crate) const FOLDING_KIND_IMPORTS: u8 = 1;
pub(crate) const FOLDING_KIND_REGION: u8 = 2;

pub(crate) const TEXT_IMPORTS: u8 = 0;
pub(crate) const TEXT_PARENTHESES: u8 = 1;
pub(crate) const TEXT_BRACES: u8 = 2;
pub(crate) const TEXT_KDOC: u8 = 3;
pub(crate) const TEXT_BLOCK_COMMENT: u8 = 4;
pub(crate) const TEXT_RAW_STRING: u8 = 5;
pub(crate) const TEXT_REGION_LABEL: u8 = 6;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FoldingRangeText {
    Imports,
    Parentheses,
    Braces,
    KDoc(Span),
    BlockComment(Span),
    RawString(Span),
    RegionLabel(Span),
}

impl FoldingRangeText {
    pub(crate) fn style(&self) -> u8 {
        match self {
            Self::Imports => TEXT_IMPORTS,
            Self::Parentheses => TEXT_PARENTHESES,
            Self::Braces => TEXT_BRACES,
            Self::KDoc(_) => TEXT_KDOC,
            Self::BlockComment(_) => TEXT_BLOCK_COMMENT,
            Self::RawString(_) => TEXT_RAW_STRING,
            Self::RegionLabel(_) => TEXT_REGION_LABEL,
        }
    }

    pub(crate) fn summary(&self) -> Span {
        match self {
            Self::KDoc(summary)
            | Self::BlockComment(summary)
            | Self::RawString(summary)
            | Self::RegionLabel(summary) => *summary,
            Self::Imports | Self::Parentheses | Self::Braces => Span::new(0, 0),
        }
    }

    pub(crate) fn collapsed_text_bytes(&self) -> usize {
        let summary = self.summary();
        let summary_bytes = summary.hi.saturating_sub(summary.lo) as usize;
        summary_bytes.saturating_add(match self {
            Self::Imports => 3,
            Self::Parentheses | Self::Braces => 5,
            Self::KDoc(_) => 10,
            Self::BlockComment(_) => 7,
            Self::RawString(_) => 10,
            Self::RegionLabel(_) => 0,
        })
    }

    fn sort_key(&self) -> (u8, u32, u32) {
        let summary = self.summary();
        (self.style(), summary.lo, summary.hi)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FoldingRangeOccurrence {
    pub(crate) span: Span,
    pub(crate) kind: u8,
    pub(crate) text: FoldingRangeText,
}

#[derive(Clone, Copy)]
struct Delimiter {
    open: u8,
    byte: usize,
    line: u32,
}

struct ImportGroup {
    start: usize,
    start_line: u32,
    end: usize,
    end_line: u32,
}

#[derive(Clone, Copy)]
enum ScanState {
    Code,
    BacktickedIdentifier,
    Quoted { quote: u8, escaped: bool },
    RawString { start: usize, line: u32 },
    BlockComment { start: usize, line: u32, depth: u32 },
}

struct RegionMarker {
    start: usize,
    line: u32,
    label: Span,
}

pub(crate) fn folding_range_occurrences(
    source: &str,
    analysis: &FileAnalysis,
    max_entries: usize,
) -> Vec<FoldingRangeOccurrence> {
    if max_entries == 0 {
        return Vec::new();
    }

    let candidate_limit = max_entries.saturating_mul(2).clamp(256, 64 * 1024);
    let mut occurrences = scan_source(source, candidate_limit);
    let function_limit = occurrences.len().saturating_add(candidate_limit);
    let mut suppressed_parentheses = HashSet::new();
    append_expression_function_ranges(
        source,
        analysis,
        &mut occurrences,
        &mut suppressed_parentheses,
        function_limit,
    );
    occurrences.retain(|occurrence| {
        occurrence.text != FoldingRangeText::Parentheses
            || !suppressed_parentheses.contains(&occurrence.span.lo)
    });
    occurrences.sort_by(|left, right| {
        (left.span.lo, left.span.hi, left.kind)
            .cmp(&(right.span.lo, right.span.hi, right.kind))
            .then_with(|| left.text.sort_key().cmp(&right.text.sort_key()))
    });
    occurrences.dedup_by(|left, right| {
        left.span == right.span && left.kind == right.kind && left.text == right.text
    });
    occurrences.truncate(max_entries);
    occurrences
}

fn scan_source(source: &str, max_entries: usize) -> Vec<FoldingRangeOccurrence> {
    let bytes = source.as_bytes();
    let mut occurrences = Vec::new();
    let mut delimiters = Vec::<Delimiter>::new();
    let mut delimiter_folding_disabled = false;
    let mut regions = Vec::<RegionMarker>::new();
    let mut imports = None::<ImportGroup>;
    let mut state = ScanState::Code;
    let mut line = 0u32;
    let mut line_start = 0usize;
    let mut line_started_in_code = true;
    let mut index = 0usize;

    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\n' || byte == b'\r' {
            finish_import_line(
                source,
                (line_start, index),
                line,
                line_started_in_code,
                &mut imports,
                &mut occurrences,
                max_entries,
            );
            if byte == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                index += 1;
            }
            line = line.saturating_add(1);
            index += 1;
            line_start = index;
            line_started_in_code = matches!(state, ScanState::Code);
            if matches!(
                state,
                ScanState::Quoted { .. } | ScanState::BacktickedIdentifier
            ) {
                state = ScanState::Code;
            }
            continue;
        }

        match state {
            ScanState::Code => match byte {
                b'/' if bytes.get(index + 1) == Some(&b'/') => {
                    let line_end = find_line_end(bytes, index + 2);
                    handle_region_marker(
                        source,
                        index,
                        line_end,
                        line,
                        &mut regions,
                        &mut occurrences,
                        max_entries,
                    );
                    index = line_end;
                }
                b'/' if bytes.get(index + 1) == Some(&b'*') => {
                    state = ScanState::BlockComment {
                        start: index,
                        line,
                        depth: 1,
                    };
                    index += 2;
                }
                b'"' if bytes.get(index + 1) == Some(&b'"')
                    && bytes.get(index + 2) == Some(&b'"') =>
                {
                    state = ScanState::RawString { start: index, line };
                    index += 3;
                }
                b'"' | b'\'' => {
                    state = ScanState::Quoted {
                        quote: byte,
                        escaped: false,
                    };
                    index += 1;
                }
                b'`' => {
                    state = ScanState::BacktickedIdentifier;
                    index += 1;
                }
                b'(' | b'{' => {
                    if !delimiter_folding_disabled {
                        if delimiters.len() == MAX_DELIMITER_DEPTH {
                            delimiters.clear();
                            delimiter_folding_disabled = true;
                        } else {
                            delimiters.push(Delimiter {
                                open: byte,
                                byte: index,
                                line,
                            });
                        }
                    }
                    index += 1;
                }
                b')' | b'}' => {
                    if !delimiter_folding_disabled {
                        let expected = if byte == b')' { b'(' } else { b'{' };
                        if let Some(open) = delimiters.pop_if(|open| open.open == expected) {
                            if open.line < line && occurrences.len() < max_entries {
                                occurrences.push(FoldingRangeOccurrence {
                                    span: Span::new(open.byte as u32, (index + 1) as u32),
                                    kind: FOLDING_KIND_REGION,
                                    text: if expected == b'(' {
                                        FoldingRangeText::Parentheses
                                    } else {
                                        FoldingRangeText::Braces
                                    },
                                });
                            }
                        }
                    }
                    index += 1;
                }
                _ => index += 1,
            },
            ScanState::Quoted { quote, mut escaped } => {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == quote {
                    state = ScanState::Code;
                    index += 1;
                    continue;
                }
                state = ScanState::Quoted { quote, escaped };
                index += 1;
            }
            ScanState::BacktickedIdentifier => {
                if byte == b'`' {
                    state = ScanState::Code;
                }
                index += 1;
            }
            ScanState::RawString {
                start,
                line: start_line,
            } => {
                if byte == b'"'
                    && bytes.get(index + 1) == Some(&b'"')
                    && bytes.get(index + 2) == Some(&b'"')
                {
                    let end = index + 3;
                    if start_line < line && occurrences.len() < max_entries {
                        if let Some(summary) =
                            first_meaningful_line_span(source, start + 3, index, false).filter(
                                |summary| {
                                    placeholder_bytes(*summary, TEXT_RAW_STRING)
                                        <= MAX_DYNAMIC_PLACEHOLDER_BYTES
                                },
                            )
                        {
                            occurrences.push(FoldingRangeOccurrence {
                                span: Span::new(start as u32, end as u32),
                                kind: FOLDING_KIND_REGION,
                                text: FoldingRangeText::RawString(summary),
                            });
                        }
                    }
                    state = ScanState::Code;
                    index = end;
                } else {
                    index += 1;
                }
            }
            ScanState::BlockComment {
                start,
                line: start_line,
                mut depth,
            } => {
                if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
                    depth = depth.saturating_add(1);
                    state = ScanState::BlockComment {
                        start,
                        line: start_line,
                        depth,
                    };
                    index += 2;
                } else if byte == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    depth -= 1;
                    let end = index + 2;
                    if depth == 0 {
                        if start_line < line && occurrences.len() < max_entries {
                            if let Some(text) = block_comment_text(source, start, end) {
                                occurrences.push(FoldingRangeOccurrence {
                                    span: Span::new(start as u32, end as u32),
                                    kind: FOLDING_KIND_COMMENT,
                                    text,
                                });
                            }
                        }
                        state = ScanState::Code;
                    } else {
                        state = ScanState::BlockComment {
                            start,
                            line: start_line,
                            depth,
                        };
                    }
                    index = end;
                } else {
                    index += 1;
                }
            }
        }
    }

    finish_import_line(
        source,
        (line_start, bytes.len()),
        line,
        line_started_in_code,
        &mut imports,
        &mut occurrences,
        max_entries,
    );
    finish_import_group(&mut imports, &mut occurrences, max_entries);
    occurrences
}

fn finish_import_line(
    source: &str,
    line_bytes: (usize, usize),
    line: u32,
    line_started_in_code: bool,
    imports: &mut Option<ImportGroup>,
    occurrences: &mut Vec<FoldingRangeOccurrence>,
    max_entries: usize,
) {
    let (line_start, mut line_end) = line_bytes;
    if source.as_bytes().get(line_end.wrapping_sub(1)) == Some(&b'\r') {
        line_end -= 1;
    }
    let line_text = &source[line_start..line_end];
    let leading = line_text.len() - line_text.trim_start_matches([' ', '\t']).len();
    let trimmed = &line_text[leading..];
    let import_name = trimmed
        .strip_prefix("import")
        .and_then(|suffix| {
            suffix
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_whitespace)
                .then_some(suffix)
        })
        .map(|suffix| suffix.len() - suffix.trim_start_matches([' ', '\t']).len());
    if line_started_in_code {
        if let Some(spaces) = import_name {
            let name = line_start + leading + "import".len() + spaces;
            match imports {
                Some(group) => {
                    group.end = line_end;
                    group.end_line = line;
                }
                None => {
                    *imports = Some(ImportGroup {
                        start: name,
                        start_line: line,
                        end: line_end,
                        end_line: line,
                    });
                }
            }
            return;
        }
    }
    finish_import_group(imports, occurrences, max_entries);
}

fn finish_import_group(
    imports: &mut Option<ImportGroup>,
    occurrences: &mut Vec<FoldingRangeOccurrence>,
    max_entries: usize,
) {
    let Some(group) = imports.take() else {
        return;
    };
    if group.start_line < group.end_line && occurrences.len() < max_entries {
        occurrences.push(FoldingRangeOccurrence {
            span: Span::new(group.start as u32, group.end as u32),
            kind: FOLDING_KIND_IMPORTS,
            text: FoldingRangeText::Imports,
        });
    }
}

fn find_line_end(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index] != b'\n' && bytes[index] != b'\r' {
        index += 1;
    }
    index
}

fn handle_region_marker(
    source: &str,
    start: usize,
    end: usize,
    line: u32,
    regions: &mut Vec<RegionMarker>,
    occurrences: &mut Vec<FoldingRangeOccurrence>,
    max_entries: usize,
) {
    let Some(comment) = trimmed_span(source, start + 2, end) else {
        return;
    };
    let comment_text = &source[comment.lo as usize..comment.hi as usize];
    let region_suffix = comment_text.strip_prefix("region").filter(|suffix| {
        suffix
            .as_bytes()
            .first()
            .is_none_or(u8::is_ascii_whitespace)
    });
    if let Some(suffix) = region_suffix {
        let label_start = comment.hi as usize - suffix.len();
        let label = trimmed_span(source, label_start, comment.hi as usize)
            .unwrap_or(Span::new(label_start as u32, label_start as u32));
        if placeholder_bytes(label, TEXT_REGION_LABEL) <= MAX_DYNAMIC_PLACEHOLDER_BYTES
            && regions.len() < MAX_DELIMITER_DEPTH
        {
            regions.push(RegionMarker { start, line, label });
        }
        return;
    }
    let is_end = comment_text == "endregion";
    if !is_end {
        return;
    }
    let Some(marker) = regions.pop() else {
        return;
    };
    if marker.line < line && occurrences.len() < max_entries {
        occurrences.push(FoldingRangeOccurrence {
            span: Span::new(marker.start as u32, end as u32),
            kind: FOLDING_KIND_COMMENT,
            text: FoldingRangeText::RegionLabel(marker.label),
        });
    }
}

fn trimmed_span(source: &str, start: usize, end: usize) -> Option<Span> {
    let value = source.get(start..end)?;
    let start_trimmed = value.trim_start();
    let lo = start + value.len().saturating_sub(start_trimmed.len());
    let trimmed = start_trimmed.trim_end();
    Some(Span::new(lo as u32, (lo + trimmed.len()) as u32))
}

fn first_meaningful_line_span(
    source: &str,
    start: usize,
    end: usize,
    strip_comment_stars: bool,
) -> Option<Span> {
    let mut line_start = start;
    while line_start <= end {
        let remaining = source.get(line_start..end)?;
        let line_length = remaining
            .bytes()
            .position(|byte| byte == b'\n' || byte == b'\r')
            .unwrap_or(remaining.len());
        let mut line = trimmed_span(source, line_start, line_start + line_length)?;
        if strip_comment_stars {
            let value = &source[line.lo as usize..line.hi as usize];
            let stars = value.bytes().take_while(|byte| *byte == b'*').count();
            line = trimmed_span(source, line.lo as usize + stars, line.hi as usize)?;
        }
        if line.lo < line.hi {
            return Some(line);
        }
        if line_length == remaining.len() {
            break;
        }
        line_start += line_length + 1;
        if source.as_bytes().get(line_start - 1) == Some(&b'\r')
            && source.as_bytes().get(line_start) == Some(&b'\n')
        {
            line_start += 1;
        }
    }
    Some(Span::new(start as u32, start as u32))
}

fn placeholder_bytes(summary: Span, style: u8) -> usize {
    let summary_bytes = summary.hi.saturating_sub(summary.lo) as usize;
    summary_bytes.saturating_add(match style {
        TEXT_KDOC => 10,
        TEXT_BLOCK_COMMENT => 7,
        TEXT_RAW_STRING => 10,
        _ => 0,
    })
}

fn block_comment_text(source: &str, start: usize, end: usize) -> Option<FoldingRangeText> {
    let kdoc = source.as_bytes().get(start + 2) == Some(&b'*');
    let inner_start = start + if kdoc { 3 } else { 2 };
    let inner_end = end.checked_sub(2)?;
    let summary = first_meaningful_line_span(source, inner_start, inner_end, true)?;
    let style = if kdoc { TEXT_KDOC } else { TEXT_BLOCK_COMMENT };
    if placeholder_bytes(summary, style) > MAX_DYNAMIC_PLACEHOLDER_BYTES {
        return None;
    }
    Some(if kdoc {
        FoldingRangeText::KDoc(summary)
    } else {
        FoldingRangeText::BlockComment(summary)
    })
}

fn append_expression_function_ranges(
    source: &str,
    analysis: &FileAnalysis,
    occurrences: &mut Vec<FoldingRangeOccurrence>,
    suppressed_parentheses: &mut HashSet<u32>,
    max_entries: usize,
) {
    for declaration in &analysis.file.decl_arena {
        match declaration {
            Decl::Fun(function) => append_function_range(
                source,
                analysis,
                function,
                occurrences,
                suppressed_parentheses,
                max_entries,
            ),
            Decl::Class(class) => append_class_function_ranges(
                source,
                analysis,
                class,
                occurrences,
                suppressed_parentheses,
                max_entries,
            ),
            Decl::Property(_) => {}
        }
    }
    for statement in &analysis.file.stmt_arena {
        match statement {
            Stmt::LocalFun(function) => append_function_range(
                source,
                analysis,
                function,
                occurrences,
                suppressed_parentheses,
                max_entries,
            ),
            Stmt::LocalClass(class) => append_class_function_ranges(
                source,
                analysis,
                class,
                occurrences,
                suppressed_parentheses,
                max_entries,
            ),
            _ => {}
        }
    }
}

fn append_class_function_ranges(
    source: &str,
    analysis: &FileAnalysis,
    class: &ClassDecl,
    occurrences: &mut Vec<FoldingRangeOccurrence>,
    suppressed_parentheses: &mut HashSet<u32>,
    max_entries: usize,
) {
    let companion = companion_class(&analysis.file, class);
    for function in class
        .methods
        .iter()
        .chain(companion.into_iter().flat_map(|class| &class.methods))
        .chain(
            class
                .enum_entries
                .iter()
                .flat_map(|entry| entry.methods.iter()),
        )
    {
        append_function_range(
            source,
            analysis,
            function,
            occurrences,
            suppressed_parentheses,
            max_entries,
        );
    }
}

fn append_function_range(
    source: &str,
    analysis: &FileAnalysis,
    function: &FunDecl,
    occurrences: &mut Vec<FoldingRangeOccurrence>,
    suppressed_parentheses: &mut HashSet<u32>,
    max_entries: usize,
) {
    if occurrences.len() >= max_entries {
        return;
    }
    let FunBody::Expr(body) = function.body else {
        return;
    };
    let Some(&body_span) = analysis.file.expr_spans.get(body.0 as usize) else {
        return;
    };
    let source_len = source.len() as u32;
    if body_span.lo >= body_span.hi || body_span.hi > source_len {
        return;
    }
    let mut start = body_span.lo;
    if let Some((open, close)) = function_parameter_span(source, function, body_span.lo) {
        let parameters = &source[open as usize..close as usize];
        if parameters
            .bytes()
            .any(|byte| byte == b'\n' || byte == b'\r')
        {
            start = open;
            suppressed_parentheses.insert(open);
        }
    }
    if !source[start as usize..body_span.hi as usize]
        .bytes()
        .any(|byte| byte == b'\n' || byte == b'\r')
    {
        return;
    }
    occurrences.push(FoldingRangeOccurrence {
        span: Span::new(start, body_span.hi),
        kind: FOLDING_KIND_REGION,
        text: FoldingRangeText::Braces,
    });
}

fn function_parameter_span(
    source: &str,
    function: &FunDecl,
    body_start: u32,
) -> Option<(u32, u32)> {
    let start = function.span.lo as usize;
    let end = (body_start as usize).min(source.len());
    let open = find_function_parameter_open(source.as_bytes(), &function.name, start, end)?;
    let close = matching_parenthesis(source.as_bytes(), open, end)?;
    Some((open as u32, close as u32))
}

fn find_function_parameter_open(
    bytes: &[u8],
    function_name: &str,
    start: usize,
    limit: usize,
) -> Option<usize> {
    let name = function_name.as_bytes();
    if name.is_empty() || start > limit || limit > bytes.len() {
        return None;
    }
    let mut index = start;
    let mut seen_fun = false;
    while index < limit {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = skip_line_comment(bytes, index, limit);
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index, limit);
            }
            b'`' => {
                let name_end = skip_quoted(bytes, index, limit);
                if seen_fun
                    && name_end > index + 1
                    && bytes.get(name_end - 1) == Some(&b'`')
                    && bytes[index + 1..name_end - 1] == *name
                {
                    if let Some(open) = next_parameter_open(bytes, name_end, limit) {
                        return Some(open);
                    }
                }
                index = name_end;
            }
            b'"' | b'\'' => {
                index = skip_quoted(bytes, index, limit);
            }
            _ if token_at(bytes, index, b"fun", start, limit) => {
                seen_fun = true;
                index += 3;
            }
            _ if seen_fun && token_starts_at(bytes, index, name, start, limit) => {
                if let Some(open) = next_parameter_open(bytes, index + name.len(), limit) {
                    return Some(open);
                }
                index += utf8_char_len(bytes[index]);
            }
            _ => index += utf8_char_len(bytes[index]),
        }
    }
    None
}

fn token_at(bytes: &[u8], index: usize, token: &[u8], start: usize, limit: usize) -> bool {
    index + token.len() <= limit
        && token_starts_at(bytes, index, token, start, limit)
        && !bytes
            .get(index + token.len())
            .is_some_and(|byte| is_identifier_byte(*byte))
}

fn token_starts_at(bytes: &[u8], index: usize, token: &[u8], start: usize, limit: usize) -> bool {
    index + token.len() <= limit
        && (index == start
            || !bytes
                .get(index.wrapping_sub(1))
                .is_some_and(|byte| is_identifier_byte(*byte)))
        && bytes[index..].starts_with(token)
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || !byte.is_ascii()
}

fn next_parameter_open(bytes: &[u8], mut index: usize, limit: usize) -> Option<usize> {
    index = skip_trivia(bytes, index, limit);
    (bytes.get(index) == Some(&b'(')).then_some(index)
}

fn matching_parenthesis(bytes: &[u8], open: usize, limit: usize) -> Option<usize> {
    matching_delimiter(bytes, open, limit, b'(', b')').map(|close| close + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler_analysis::analyze_standalone_source_set;

    #[test]
    fn scanner_ignores_delimiters_inside_literals_and_comments() {
        let source = "val quoted = \"{ not a block }\"\n\
                      val character = '{'\n\
                      /*\n\
                       * { comment body }\n\
                       */\n\
                      val raw = \"\"\"\n\
                      { raw body }\n\
                      \"\"\"\n\
                      fun actual() {\n\
                        val escaped = \"\\\"} still quoted\\\"\"\n\
                        /* outer /* } nested */ still outer */\n\
                      }\n";
        let comment_start = source.find("/*\n").unwrap();
        let comment_end = comment_start + source[comment_start..].find("*/").unwrap() + "*/".len();
        let comment_summary = source.find("{ comment body }").unwrap();
        let raw_start = source.find("\"\"\"").unwrap();
        let raw_end =
            raw_start + 3 + source[raw_start + 3..].find("\"\"\"").unwrap() + "\"\"\"".len();
        let raw_summary = source.find("{ raw body }").unwrap();
        let brace_start = source.find("fun actual() {").unwrap() + "fun actual() ".len();
        let brace_end = source.rfind('}').unwrap() + 1;
        let analysis = analyze_standalone_source_set(&[source]);

        assert_eq!(
            folding_range_occurrences(source, &analysis.files[0], 128),
            vec![
                FoldingRangeOccurrence {
                    span: Span::new(comment_start as u32, comment_end as u32),
                    kind: FOLDING_KIND_COMMENT,
                    text: FoldingRangeText::BlockComment(Span::new(
                        comment_summary as u32,
                        (comment_summary + "{ comment body }".len()) as u32,
                    )),
                },
                FoldingRangeOccurrence {
                    span: Span::new(raw_start as u32, raw_end as u32),
                    kind: FOLDING_KIND_REGION,
                    text: FoldingRangeText::RawString(Span::new(
                        raw_summary as u32,
                        (raw_summary + "{ raw body }".len()) as u32,
                    )),
                },
                FoldingRangeOccurrence {
                    span: Span::new(brace_start as u32, brace_end as u32),
                    kind: FOLDING_KIND_REGION,
                    text: FoldingRangeText::Braces,
                },
            ]
        );
    }

    #[test]
    fn scanners_ignore_delimiters_inside_backticked_identifiers() {
        let source = "fun escaped(\n  `)`: Int,\n): Int = listOf(\n  `)`,\n  1,\n).first()\n";
        let open = source.find('(').unwrap();
        let close = source.find("\n)").unwrap() + 2;
        let call_open = source.find("listOf(").unwrap() + "listOf".len();
        let call_close = source.rfind("\n)").unwrap() + 2;

        assert_eq!(
            matching_parenthesis(source.as_bytes(), open, source.len()),
            Some(close)
        );
        assert_eq!(
            scan_source(source, 8)
                .into_iter()
                .filter(|range| range.text == FoldingRangeText::Parentheses)
                .map(|range| range.span)
                .collect::<Vec<_>>(),
            vec![
                Span::new(open as u32, close as u32),
                Span::new(call_open as u32, call_close as u32)
            ]
        );
    }

    #[test]
    fn function_header_scanner_skips_comment_and_raw_string_delimiters() {
        let source = "\"\"\" f() \"\"\"\n\
                      /* f() */\n\
                      fun f(\n\
                      \u{20}\u{20}// ) f()\n\
                      \u{20}\u{20}/* nested /* ) */ f() */\n\
                      \u{20}\u{20}value: String = \"\"\"\n\
                      \u{20}\u{20}\u{20}\u{20}) f()\n\
                      \u{20}\u{20}\"\"\",\n\
                      ) = value\n";
        let open = source.find("fun f(").unwrap() + "fun f".len();
        let close = source.rfind("\n)").unwrap() + 2;

        assert_eq!(
            find_function_parameter_open(source.as_bytes(), "f", 0, source.len()),
            Some(open)
        );
        assert_eq!(
            matching_parenthesis(source.as_bytes(), open, source.len()),
            Some(close)
        );
    }

    #[test]
    fn function_name_scan_is_linear_across_a_long_shared_prefix_receiver() {
        let name = "a".repeat(64 * 1024);
        let receiver = format!("{name}b");
        let source = format!("fun {receiver}.{name}() = 1");
        let open = source.rfind('(').unwrap();

        assert_eq!(
            find_function_parameter_open(source.as_bytes(), &name, 0, source.len()),
            Some(open)
        );
    }

    #[test]
    fn scanner_budget_preserves_the_first_exact_range_and_skips_later_candidates() {
        let source = "\"\"\"\nraw\n\"\"\"\n\
                      /*\ncomment\n*/\n\
                      {\nbody\n}\n";
        let analysis = analyze_standalone_source_set(&[source]);

        assert_eq!(
            folding_range_occurrences(source, &analysis.files[0], 1),
            vec![FoldingRangeOccurrence {
                span: Span::new(0, 11),
                kind: FOLDING_KIND_REGION,
                text: FoldingRangeText::RawString(Span::new(4, 7)),
            }]
        );
        assert!(folding_range_occurrences(source, &analysis.files[0], 0).is_empty());
    }

    #[test]
    fn scanner_tracks_crlf_import_locations_exactly() {
        let source = "import first.name\r\nimport second.name\r\n\r\n";
        let analysis = analyze_standalone_source_set(&[source]);

        assert_eq!(
            folding_range_occurrences(source, &analysis.files[0], 8),
            vec![FoldingRangeOccurrence {
                span: Span::new(7, 37),
                kind: FOLDING_KIND_IMPORTS,
                text: FoldingRangeText::Imports,
            }]
        );
    }

    #[test]
    fn scanner_bounds_malformed_delimiter_and_region_nesting() {
        let source = format!(
            "{}\n{}",
            "(".repeat(MAX_DELIMITER_DEPTH + 1),
            ")".repeat(MAX_DELIMITER_DEPTH + 1)
        );
        let analysis = analyze_standalone_source_set(&[&source]);
        let ranges = folding_range_occurrences(&source, &analysis.files[0], 8);

        assert!(ranges.is_empty());

        let regions = format!(
            "{}\n{}",
            "//region x\n".repeat(MAX_DELIMITER_DEPTH + 1),
            "//endregion\n".repeat(MAX_DELIMITER_DEPTH + 1)
        );
        let analysis = analyze_standalone_source_set(&[&regions]);
        let ranges = folding_range_occurrences(&regions, &analysis.files[0], 8);
        assert!(ranges.len() <= 8);
    }

    #[test]
    fn oversized_dynamic_placeholders_are_skipped_before_allocation() {
        let oversized = "x".repeat(MAX_DYNAMIC_PLACEHOLDER_BYTES + 1);
        for source in [
            format!("/*\n{oversized}\n*/"),
            format!("//region {oversized}\nbody\n//endregion\n"),
            format!("\"\"\"\n{oversized}\n\"\"\""),
        ] {
            let analysis = analyze_standalone_source_set(&[&source]);
            assert!(folding_range_occurrences(&source, &analysis.files[0], 8).is_empty());
        }
    }
}

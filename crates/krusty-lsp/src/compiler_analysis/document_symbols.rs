//! Hierarchical source declarations reduced to names, kinds, and byte spans.

use std::collections::{HashMap, HashSet};

use krusty::ast::{ClassDecl, ClassKind, Decl, File, FunDecl, PropDecl};
use krusty::diag::{DiagSink, Span};
use krusty::frontend::lex_name_tokens;

use super::navigation::definition_name_span;
use super::source_scan::{
    bounded_utf8_advance, matching_delimiter, normalized_scan_end, skip_block_comment, skip_quoted,
    skip_trivia, utf8_char_len,
};

const SYMBOL_KIND_CLASS: u8 = 5;
const SYMBOL_KIND_METHOD: u8 = 6;
const SYMBOL_KIND_PROPERTY: u8 = 7;
const SYMBOL_KIND_CONSTRUCTOR: u8 = 9;
const SYMBOL_KIND_ENUM: u8 = 10;
const SYMBOL_KIND_INTERFACE: u8 = 11;
const SYMBOL_KIND_FUNCTION: u8 = 12;
const SYMBOL_KIND_VARIABLE: u8 = 13;
const SYMBOL_KIND_OBJECT: u8 = 19;
const SYMBOL_KIND_ENUM_MEMBER: u8 = 22;
const SYMBOL_KIND_STRUCT: u8 = 23;
const MAX_DOCUMENT_SYMBOL_DEPTH: usize = 128;

pub(crate) struct DocumentSymbolOccurrence {
    pub name: String,
    pub kind: u8,
    pub deprecated: bool,
    pub range: Span,
    pub selection: Span,
    pub parent: Option<usize>,
}

struct SymbolNode {
    name: String,
    kind: u8,
    deprecated: bool,
    range: Span,
    selection: Span,
    children: Vec<SymbolNode>,
}

struct ExtractionBudget {
    remaining: usize,
}

impl ExtractionBudget {
    fn take(&mut self) -> bool {
        let Some(remaining) = self.remaining.checked_sub(1) else {
            return false;
        };
        self.remaining = remaining;
        true
    }
}

/// Extract the declaration hierarchy from a parsed file.
///
/// Takes the parsed `File` rather than a `FileAnalysis`: nothing here reads resolved types, so a
/// file that was only parsed -- which is all an unopened workspace file gets -- produces the same
/// symbols as one that was fully analyzed.
pub(crate) fn document_symbol_occurrences(
    source: &str,
    file: &File,
    max_entries: usize,
) -> Vec<DocumentSymbolOccurrence> {
    if max_entries == 0 {
        return Vec::new();
    }
    let mut budget = ExtractionBudget {
        remaining: max_entries,
    };
    let mut diagnostics = DiagSink::new();
    let tokens = lex_name_tokens(source, &mut diagnostics);
    let classes = file
        .decls
        .iter()
        .filter_map(|&declaration| {
            if file.is_local_declaration(declaration) {
                return None;
            }
            match file.decl(declaration) {
                Decl::Class(class) => Some(class),
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    let class_names = classes
        .iter()
        .map(|class| class.name.as_str())
        .collect::<HashSet<_>>();
    let mut nested = HashMap::<&str, Vec<&ClassDecl>>::new();
    for class in &classes {
        if let Some((parent, _)) = class.name.rsplit_once('.') {
            if class_names.contains(parent) {
                nested.entry(parent).or_default().push(class);
            }
        }
    }

    let mut roots = Vec::new();
    for &declaration in &file.decls {
        if file.is_local_declaration(declaration) {
            continue;
        }
        match file.decl(declaration) {
            Decl::Fun(function) => {
                if let Some(node) = function_node(source, &tokens, function, false, &mut budget) {
                    roots.push(node);
                }
            }
            Decl::Property(property) => {
                if let Some(node) = property_node(source, &tokens, property, &mut budget) {
                    roots.push(node);
                }
            }
            Decl::Class(class) => {
                let has_parent = class
                    .name
                    .rsplit_once('.')
                    .is_some_and(|(parent, _)| class_names.contains(parent));
                if !has_parent {
                    if let Some(node) =
                        class_node(source, &tokens, file, class, &nested, 0, &mut budget)
                    {
                        roots.push(node);
                    }
                }
            }
        }
    }
    roots.extend(type_alias_nodes(source, &tokens, file, &mut budget));
    roots.sort_by_key(|node| (node.range.lo, node.range.hi));

    let mut flattened = Vec::new();
    let mut pending = roots
        .into_iter()
        .rev()
        .map(|node| (node, None))
        .collect::<Vec<_>>();
    while let Some((node, parent)) = pending.pop() {
        let index = flattened.len();
        let SymbolNode {
            name,
            kind,
            deprecated,
            range,
            selection,
            children,
        } = node;
        flattened.push(DocumentSymbolOccurrence {
            name,
            kind,
            deprecated,
            range,
            selection,
            parent,
        });
        pending.extend(children.into_iter().rev().map(|child| (child, Some(index))));
    }
    flattened
}

fn class_node(
    source: &str,
    tokens: &[krusty::frontend::FrontendNameToken],
    file: &krusty::ast::File,
    class: &ClassDecl,
    nested: &HashMap<&str, Vec<&ClassDecl>>,
    depth: usize,
    budget: &mut ExtractionBudget,
) -> Option<SymbolNode> {
    if depth >= MAX_DOCUMENT_SYMBOL_DEPTH {
        return None;
    }
    let name = class.name.rsplit('.').next().unwrap_or(&class.name);
    let selection = declaration_name_span_bounded(tokens, source, class.span, name)?;
    if !budget.take() {
        return None;
    }
    let mut children = Vec::new();
    if let Some((parameters, constructor)) = primary_constructor_spans(source, class, selection) {
        children.extend(constructor_parameter_nodes(
            source, class, parameters, budget,
        ));
        if budget.take() {
            children.push(SymbolNode {
                name: name.to_string(),
                kind: SYMBOL_KIND_CONSTRUCTOR,
                deprecated: source_prefix_is_deprecated(source, constructor.lo, parameters.lo),
                range: constructor,
                selection: constructor,
                children: Vec::new(),
            });
        }
    }

    let mut body_children = Vec::new();
    for property in &class.body_props {
        if let Some(node) = property_node(source, tokens, property, budget) {
            body_children.push(node);
        }
    }
    for function in &class.methods {
        if let Some(node) = function_node(source, tokens, function, true, budget) {
            body_children.push(node);
        }
    }
    for constructor in &class.secondary_ctors {
        if let Some(range) = secondary_constructor_range(source, file, constructor) {
            if budget.take() {
                body_children.push(SymbolNode {
                    name: name.to_string(),
                    kind: SYMBOL_KIND_CONSTRUCTOR,
                    deprecated: source_prefix_is_deprecated(source, range.lo, constructor.span.lo),
                    range,
                    selection: range,
                    children: Vec::new(),
                });
            }
        }
    }
    if let Some(node) = companion_object_node(source, tokens, class, budget) {
        body_children.push(node);
    }
    let mut enum_boundary = outer_braces(source.as_bytes(), class.span)
        .map_or(class.span.lo as usize, |(open, _)| open + 1);
    for entry in &class.enum_entries {
        let selection = definition_name_span(source, entry.span);
        let range = enum_entry_range(source, selection, class.span.hi, enum_boundary);
        enum_boundary = range.hi as usize;
        if budget.take() {
            body_children.push(SymbolNode {
                name: entry.name.clone(),
                kind: SYMBOL_KIND_ENUM_MEMBER,
                deprecated: source_prefix_is_deprecated(source, range.lo, selection.lo),
                range,
                selection,
                children: Vec::new(),
            });
        }
    }
    if let Some(nested_classes) = nested.get(class.name.as_str()) {
        for nested_class in nested_classes {
            if let Some(node) = class_node(
                source,
                tokens,
                file,
                nested_class,
                nested,
                depth + 1,
                budget,
            ) {
                body_children.push(node);
            }
        }
    }
    body_children.sort_by_key(|node| (node.range.lo, node.range.hi));
    children.extend(body_children);

    Some(SymbolNode {
        name: name.to_string(),
        kind: match class.kind {
            ClassKind::Class if class.is_data => SYMBOL_KIND_STRUCT,
            ClassKind::Class | ClassKind::Annotation => SYMBOL_KIND_CLASS,
            ClassKind::Interface => SYMBOL_KIND_INTERFACE,
            ClassKind::Object => SYMBOL_KIND_OBJECT,
            ClassKind::Enum => SYMBOL_KIND_ENUM,
        },
        deprecated: source_prefix_is_deprecated(
            source,
            declaration_prefix_start(source, class.span.lo),
            class.span.lo,
        ),
        range: declaration_range(source, class.span),
        selection,
        children,
    })
}

fn secondary_constructor_range(
    source: &str,
    file: &krusty::ast::File,
    constructor: &krusty::ast::SecondaryCtor,
) -> Option<Span> {
    let lo = declaration_prefix_start(source, constructor.span.lo);
    if let Some(body) = constructor.body {
        let body_span = *file.expr_spans.get(body.0 as usize)?;
        return Some(Span::new(lo, declaration_range(source, body_span).hi));
    }

    let bytes = source.as_bytes();
    let mut hi = constructor.span.hi as usize;
    let mut parens = 0usize;
    let mut quote = None;
    let mut escaped = false;
    while hi < bytes.len() {
        let byte = bytes[hi];
        if let Some(end_quote) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' && end_quote != b'`' {
                escaped = true;
            } else if byte == end_quote {
                quote = None;
            }
            hi += utf8_char_len(byte);
            continue;
        }
        match byte {
            b'"' | b'\'' | b'`' => quote = Some(byte),
            b'(' => parens += 1,
            b')' => parens = parens.saturating_sub(1),
            b';' if parens == 0 => {
                hi += 1;
                break;
            }
            b'\r' | b'\n' | b'}' if parens == 0 => break,
            _ => {}
        }
        hi += utf8_char_len(byte);
    }
    while hi > lo as usize && bytes.get(hi - 1).is_some_and(u8::is_ascii_whitespace) {
        hi -= 1;
    }
    Some(Span::new(lo, hi as u32))
}

fn type_alias_nodes(
    source: &str,
    tokens: &[krusty::frontend::FrontendNameToken],
    file: &File,
    budget: &mut ExtractionBudget,
) -> Vec<SymbolNode> {
    let aliases = file
        .type_aliases
        .iter()
        .map(|(alias, _)| alias.as_str())
        .chain(
            file.type_alias_fun
                .iter()
                .map(|(alias, _, _)| alias.as_str()),
        )
        .take(budget.remaining)
        .collect::<HashSet<_>>();
    if aliases.is_empty() {
        return Vec::new();
    }

    let mut nodes = Vec::with_capacity(aliases.len().min(budget.remaining));
    let mut emitted = HashSet::with_capacity(aliases.len().min(budget.remaining));
    for (index, token) in tokens.iter().enumerate() {
        if token.text(source) != "typealias" {
            continue;
        }
        let Some(name_token) = tokens[index + 1..]
            .iter()
            .take_while(|next| {
                !matches!(next.kind, krusty::frontend::FrontendNameTokenKind::Newline)
            })
            .find(|next| matches!(next.kind, krusty::frontend::FrontendNameTokenKind::Ident))
        else {
            continue;
        };
        let raw_name = name_token.text(source);
        let name = raw_name.trim_matches('`');
        if !aliases.contains(name) || !emitted.insert(name) {
            continue;
        }
        if !budget.take() {
            break;
        }
        let range = type_alias_range(source, token.span);
        nodes.push(SymbolNode {
            name: name.to_string(),
            kind: SYMBOL_KIND_CLASS,
            deprecated: source_prefix_is_deprecated(source, range.lo, token.span.lo),
            range,
            selection: name_token.span,
            children: Vec::new(),
        });
    }
    nodes
}

fn type_alias_range(source: &str, keyword: Span) -> Span {
    let bytes = source.as_bytes();
    let mut hi = keyword.hi as usize;
    let mut quote = None;
    let mut escaped = false;
    while hi < bytes.len() {
        let byte = bytes[hi];
        if let Some(end_quote) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' && end_quote != b'`' {
                escaped = true;
            } else if byte == end_quote {
                quote = None;
            }
            hi += utf8_char_len(byte);
            continue;
        }
        match byte {
            b'"' | b'\'' | b'`' => quote = Some(byte),
            b';' => {
                hi += 1;
                break;
            }
            b'\r' | b'\n' => break,
            b'/' if bytes.get(hi + 1) == Some(&b'/') => break,
            _ => {}
        }
        hi += utf8_char_len(byte);
    }
    while hi > keyword.lo as usize && bytes.get(hi - 1).is_some_and(u8::is_ascii_whitespace) {
        hi -= 1;
    }
    Span::new(declaration_prefix_start(source, keyword.lo), hi as u32)
}

fn companion_object_node(
    source: &str,
    tokens: &[krusty::frontend::FrontendNameToken],
    class: &ClassDecl,
    budget: &mut ExtractionBudget,
) -> Option<SymbolNode> {
    let (keyword, _object_keyword, selection, name, body) =
        companion_object_source(source, class.span)?;
    if !budget.take() {
        return None;
    }
    let mut children = Vec::with_capacity(
        (class.companion_props.len() + class.companion_methods.len()).min(budget.remaining),
    );
    for property in &class.companion_props {
        if let Some(node) = property_node(source, tokens, property, budget) {
            children.push(node);
        }
    }
    for function in &class.companion_methods {
        if let Some(node) = function_node(source, tokens, function, true, budget) {
            children.push(node);
        }
    }
    children.sort_by_key(|node| (node.range.lo, node.range.hi));

    let range = Span::new(declaration_prefix_start(source, keyword.lo), body.hi);
    Some(SymbolNode {
        name,
        kind: SYMBOL_KIND_OBJECT,
        deprecated: source_prefix_is_deprecated(source, range.lo, keyword.lo),
        range,
        selection: selection.unwrap_or(range),
        children,
    })
}

fn companion_object_source(
    source: &str,
    class_span: Span,
) -> Option<(Span, Span, Option<Span>, String, Span)> {
    let bytes = source.as_bytes();
    let (class_open, class_close) = outer_braces(bytes, class_span)?;
    let mut index = class_open + 1;
    let mut depth = 0usize;
    while index < class_close {
        index = skip_trivia(bytes, index, class_close);
        if index >= class_close {
            break;
        }
        match bytes[index] {
            b'{' => {
                depth += 1;
                index += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                index += 1;
            }
            b'"' | b'\'' | b'`' => {
                index = skip_quoted(bytes, index, class_close);
            }
            _ if depth == 0 && word_at(bytes, index, b"companion") => {
                let companion = Span::new(index as u32, (index + 9) as u32);
                let object_lo = skip_trivia(bytes, index + 9, class_close);
                if !word_at(bytes, object_lo, b"object") {
                    index += 9;
                    continue;
                }
                let object = Span::new(object_lo as u32, (object_lo + 6) as u32);
                let after_object = skip_trivia(bytes, object_lo + 6, class_close);
                let selection = source_identifier_span(bytes, after_object, class_close);
                let name = selection
                    .and_then(|span| source.get(span.lo as usize..span.hi as usize))
                    .map(|name| name.trim_matches('`').to_string())
                    .unwrap_or_else(|| "Companion".to_string());
                let header_end = selection.map_or(object_lo + 6, |span| span.hi as usize);
                let body_open = find_declaration_body(bytes, header_end, class_close)?;
                let body_close = matching_delimiter(bytes, body_open, class_close, b'{', b'}')?;
                if body_close > class_close {
                    return None;
                }
                return Some((
                    companion,
                    object,
                    selection,
                    name,
                    Span::new(body_open as u32, (body_close + 1) as u32),
                ));
            }
            _ => index += utf8_char_len(bytes[index]),
        }
    }
    None
}

fn outer_braces(bytes: &[u8], span: Span) -> Option<(usize, usize)> {
    let lo = span.lo as usize;
    let mut hi = (span.hi as usize).min(bytes.len());
    while hi > lo && bytes.get(hi - 1).is_some_and(u8::is_ascii_whitespace) {
        hi -= 1;
    }
    if bytes.get(hi.checked_sub(1)?) != Some(&b'}') {
        return None;
    }
    let close = hi - 1;
    let mut stack = Vec::new();
    let mut index = lo;
    while index <= close {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index <= close && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index, close + 1);
            }
            b'"' | b'\'' | b'`' => index = skip_quoted(bytes, index, close + 1),
            b'{' => {
                stack.push(index);
                index += 1;
            }
            b'}' => {
                let open = stack.pop()?;
                if index == close {
                    return Some((open, close));
                }
                index += 1;
            }
            _ => index += utf8_char_len(bytes[index]),
        }
    }
    None
}

fn find_declaration_body(bytes: &[u8], mut index: usize, end: usize) -> Option<usize> {
    let mut parens = 0usize;
    let mut brackets = 0usize;
    while index < end {
        index = skip_trivia(bytes, index, end);
        match *bytes.get(index)? {
            b'"' | b'\'' | b'`' => index = skip_quoted(bytes, index, end),
            b'(' => {
                parens += 1;
                index += 1;
            }
            b')' => {
                parens = parens.saturating_sub(1);
                index += 1;
            }
            b'[' => {
                brackets += 1;
                index += 1;
            }
            b']' => {
                brackets = brackets.saturating_sub(1);
                index += 1;
            }
            b'{' if parens == 0 && brackets == 0 => return Some(index),
            _ => index += utf8_char_len(bytes[index]),
        }
    }
    None
}

fn source_identifier_span(bytes: &[u8], start: usize, end: usize) -> Option<Span> {
    if start >= end || matches!(bytes[start], b':' | b'{') {
        return None;
    }
    if bytes[start] == b'`' {
        let relative = bytes
            .get(start + 1..end)?
            .iter()
            .position(|byte| *byte == b'`')?;
        return Some(Span::new(start as u32, (start + relative + 2) as u32));
    }
    let mut index = start;
    while index < end {
        let byte = bytes[index];
        if byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80 {
            index += utf8_char_len(byte);
        } else {
            break;
        }
    }
    (index > start).then(|| Span::new(start as u32, index as u32))
}

fn word_at(bytes: &[u8], index: usize, word: &[u8]) -> bool {
    bytes.get(index..index + word.len()) == Some(word)
        && !bytes
            .get(index.wrapping_sub(1))
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        && !bytes
            .get(index + word.len())
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

fn function_node(
    source: &str,
    tokens: &[krusty::frontend::FrontendNameToken],
    function: &FunDecl,
    member: bool,
    budget: &mut ExtractionBudget,
) -> Option<SymbolNode> {
    if budget.remaining == 0 {
        return None;
    }
    let selection = declaration_name_span_bounded(tokens, source, function.span, &function.name)?;
    if !budget.take() {
        return None;
    }
    let range = declaration_range(source, function.span);
    Some(SymbolNode {
        name: function.name.clone(),
        kind: if member {
            SYMBOL_KIND_METHOD
        } else {
            SYMBOL_KIND_FUNCTION
        },
        deprecated: source_prefix_is_deprecated(source, range.lo, function.span.lo),
        range,
        selection,
        children: Vec::new(),
    })
}

fn property_node(
    source: &str,
    tokens: &[krusty::frontend::FrontendNameToken],
    property: &PropDecl,
    budget: &mut ExtractionBudget,
) -> Option<SymbolNode> {
    if budget.remaining == 0 {
        return None;
    }
    let selection = declaration_name_span_bounded(tokens, source, property.span, &property.name)?;
    if !budget.take() {
        return None;
    }
    let range = declaration_range(source, property.span);
    Some(SymbolNode {
        name: property.name.clone(),
        kind: SYMBOL_KIND_PROPERTY,
        deprecated: source_prefix_is_deprecated(source, range.lo, property.span.lo),
        range,
        selection,
        children: Vec::new(),
    })
}

fn declaration_name_span_bounded(
    tokens: &[krusty::frontend::FrontendNameToken],
    source: &str,
    owner: Span,
    name: &str,
) -> Option<Span> {
    let first = tokens.partition_point(|token| token.span.hi <= owner.lo);
    let span = tokens[first..]
        .iter()
        .take_while(|token| token.span.lo < owner.hi)
        .find(|token| {
            token.kind == krusty::frontend::FrontendNameTokenKind::Ident
                && token.span.hi <= owner.hi
                && token.text(source) == name
        })?
        .span;
    Some(definition_name_span(source, span))
}

fn source_prefix_is_deprecated(source: &str, prefix_lo: u32, declaration_lo: u32) -> bool {
    let bytes = source.as_bytes();
    let end = (declaration_lo as usize).min(bytes.len());
    let mut index = (prefix_lo as usize).min(end);
    while index < end {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < end && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index, end);
            }
            b'"' | b'\'' | b'`' => index = skip_quoted(bytes, index, end),
            b'@' => {
                let mut cursor = skip_trivia(bytes, index + 1, end);
                let Some(mut name_span) = source_identifier_span(bytes, cursor, end) else {
                    index += 1;
                    continue;
                };
                cursor = name_span.hi as usize;
                if bytes.get(cursor) == Some(&b':') {
                    cursor = skip_trivia(bytes, cursor + 1, end);
                    let Some(target_span) = source_identifier_span(bytes, cursor, end) else {
                        index += 1;
                        continue;
                    };
                    name_span = target_span;
                    cursor = name_span.hi as usize;
                }
                loop {
                    cursor = skip_trivia(bytes, cursor, end);
                    if bytes.get(cursor) != Some(&b'.') {
                        break;
                    }
                    cursor = skip_trivia(bytes, cursor + 1, end);
                    let Some(segment) = source_identifier_span(bytes, cursor, end) else {
                        break;
                    };
                    name_span = segment;
                    cursor = segment.hi as usize;
                }
                if source
                    .get(name_span.lo as usize..name_span.hi as usize)
                    .is_some_and(|name| name.trim_matches('`') == "Deprecated")
                {
                    return true;
                }
                index = cursor.max(index + 1);
            }
            _ => index += utf8_char_len(bytes[index]),
        }
    }
    false
}

fn primary_constructor_spans(
    source: &str,
    class: &ClassDecl,
    class_name: Span,
) -> Option<(Span, Span)> {
    if matches!(class.kind, ClassKind::Interface | ClassKind::Object) {
        return None;
    }
    let bytes = source.as_bytes();
    let end = normalized_scan_end(bytes, declaration_range(source, class.span).hi as usize);
    let mut index = class_name.hi as usize;
    if index >= end {
        return None;
    }
    let line_end = bytes[index..end]
        .iter()
        .position(|byte| matches!(byte, b'\n' | b'\r'))
        .map_or(end, |relative| index + relative);
    if bytes[index..line_end].iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    let after_name = skip_trivia(bytes, index, end);
    if bytes[index..after_name]
        .iter()
        .any(|byte| matches!(byte, b'\n' | b'\r'))
    {
        return None;
    }
    index = after_name;
    if bytes.get(index) == Some(&b'<') {
        index = matching_delimiter(bytes, index, end, b'<', b'>')?.saturating_add(1);
    }
    let prefix_start = skip_trivia(bytes, index, end);
    if bytes[index..prefix_start]
        .iter()
        .any(|byte| matches!(byte, b'\n' | b'\r'))
    {
        return None;
    }
    index = prefix_start;
    let mut explicit = false;
    let open = loop {
        if index >= end {
            return None;
        }
        let previous = index;
        index = skip_trivia(bytes, index, end);
        if index >= end {
            return None;
        }
        match *bytes.get(index)? {
            b'@' => index = skip_source_annotation(bytes, index, end),
            b'<' => {
                index = matching_delimiter(bytes, index, end, b'<', b'>')?.saturating_add(1);
            }
            b'(' => break index,
            b':' | b'{' => return None,
            b'"' | b'\'' | b'`' => index = skip_quoted(bytes, index, end),
            _ if word_at(bytes, index, b"constructor") => {
                explicit = true;
                index += b"constructor".len();
            }
            _ => index = bounded_utf8_advance(bytes, index, end),
        }
        if index <= previous {
            return None;
        }
    };
    let close = matching_delimiter(bytes, open, end, b'(', b')')?;
    let parameters = Span::new(open as u32, (close + 1) as u32);
    let symbol = if explicit {
        Span::new(prefix_start as u32, parameters.hi)
    } else {
        parameters
    };
    Some((parameters, symbol))
}

fn skip_source_annotation(bytes: &[u8], at: usize, end: usize) -> usize {
    let mut index = skip_trivia(bytes, at.saturating_add(1), end);
    if let Some(span) = source_identifier_span(bytes, index, end) {
        index = span.hi as usize;
    }
    loop {
        index = skip_trivia(bytes, index, end);
        if index >= end {
            return end;
        }
        if bytes.get(index) == Some(&b':') || bytes.get(index) == Some(&b'.') {
            index = skip_trivia(bytes, index + 1, end);
            if let Some(span) = source_identifier_span(bytes, index, end) {
                index = span.hi as usize;
                continue;
            }
        }
        break;
    }
    index = skip_trivia(bytes, index, end);
    if index >= end {
        return end;
    }
    if bytes.get(index) == Some(&b'(') {
        matching_delimiter(bytes, index, end, b'(', b')')
            .map_or(end, |close| close.saturating_add(1))
    } else {
        index
    }
}

fn constructor_parameter_nodes(
    source: &str,
    class: &ClassDecl,
    constructor: Span,
    budget: &mut ExtractionBudget,
) -> Vec<SymbolNode> {
    let mut nodes = Vec::with_capacity(class.props.len().min(budget.remaining));
    let mut boundary = constructor.lo.saturating_add(1);
    for (index, parameter) in class.props.iter().enumerate() {
        let next_name = class.props.get(index + 1).map(|next| next.span.lo);
        let end_boundary = next_name.unwrap_or(constructor.hi.saturating_sub(1));
        let range =
            constructor_parameter_range(source, boundary, end_boundary, next_name.is_some());
        boundary = range.hi.saturating_add(1);
        if !parameter.is_property {
            continue;
        }
        if !budget.take() {
            break;
        }
        nodes.push(SymbolNode {
            name: parameter.name.clone(),
            kind: SYMBOL_KIND_VARIABLE,
            deprecated: source_prefix_is_deprecated(source, range.lo, parameter.span.lo),
            range,
            selection: definition_name_span(source, parameter.span),
            children: Vec::new(),
        });
    }
    nodes
}

fn constructor_parameter_range(
    source: &str,
    boundary: u32,
    end_boundary: u32,
    has_next: bool,
) -> Span {
    let bytes = source.as_bytes();
    let mut lo = boundary as usize;
    while bytes.get(lo).is_some_and(u8::is_ascii_whitespace) {
        lo += 1;
    }
    let mut hi = end_boundary as usize;
    if has_next {
        if let Some(comma) = parameter_separator(bytes, lo, hi) {
            hi = comma;
        }
    }
    while hi > lo && bytes.get(hi - 1).is_some_and(u8::is_ascii_whitespace) {
        hi -= 1;
    }
    Span::new(lo as u32, hi as u32)
}

fn parameter_separator(bytes: &[u8], mut index: usize, end: usize) -> Option<usize> {
    let mut parens = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;
    let mut angles = 0usize;
    let mut deferred_commas = Vec::<(usize, usize)>::new();
    while index < end {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < end && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index, end);
            }
            b'"' | b'\'' | b'`' => index = skip_quoted(bytes, index, end),
            b'(' => {
                parens += 1;
                index += 1;
            }
            b')' => {
                parens = parens.saturating_sub(1);
                index += 1;
            }
            b'[' => {
                brackets += 1;
                index += 1;
            }
            b']' => {
                brackets = brackets.saturating_sub(1);
                index += 1;
            }
            b'{' => {
                braces += 1;
                index += 1;
            }
            b'}' => {
                braces = braces.saturating_sub(1);
                index += 1;
            }
            b'<' if parens == 0 && brackets == 0 && braces == 0 => {
                angles += 1;
                index += 1;
            }
            b'>' if angles > 0 && parens == 0 && brackets == 0 && braces == 0 => {
                while deferred_commas
                    .last()
                    .is_some_and(|(depth, _)| *depth >= angles)
                {
                    deferred_commas.pop();
                }
                angles -= 1;
                index += 1;
            }
            b',' if parens == 0 && brackets == 0 && braces == 0 => {
                if angles == 0 {
                    return Some(index);
                }
                if deferred_commas
                    .last()
                    .is_none_or(|(depth, _)| *depth != angles)
                {
                    deferred_commas.push((angles, index));
                }
                index += 1;
            }
            _ => index += utf8_char_len(bytes[index]),
        }
    }
    deferred_commas.into_iter().map(|(_, comma)| comma).min()
}

fn enum_entry_range(source: &str, selection: Span, class_hi: u32, prefix_boundary: usize) -> Span {
    let bytes = source.as_bytes();
    let end = (class_hi as usize).min(bytes.len());
    let mut hi = selection.hi as usize;
    let mut parens = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;
    while hi < end {
        match bytes[hi] {
            b'/' if bytes.get(hi + 1) == Some(&b'/') => {
                hi += 2;
                while hi < end && bytes[hi] != b'\n' {
                    hi += 1;
                }
            }
            b'/' if bytes.get(hi + 1) == Some(&b'*') => {
                hi = skip_block_comment(bytes, hi, end);
            }
            b'"' | b'\'' | b'`' => hi = skip_quoted(bytes, hi, end),
            b'(' => {
                parens += 1;
                hi += 1;
            }
            b')' => {
                parens = parens.saturating_sub(1);
                hi += 1;
            }
            b'[' => {
                brackets += 1;
                hi += 1;
            }
            b']' => {
                brackets = brackets.saturating_sub(1);
                hi += 1;
            }
            b'{' => {
                braces += 1;
                hi += 1;
            }
            b'}' if braces > 0 => {
                braces -= 1;
                hi += 1;
            }
            b',' if parens == 0 && brackets == 0 && braces == 0 => {
                hi += 1;
                break;
            }
            b';' | b'}' if parens == 0 && brackets == 0 && braces == 0 => break,
            _ => hi += utf8_char_len(bytes[hi]),
        }
    }
    let lo = skip_trivia(bytes, prefix_boundary, selection.lo as usize);
    while hi > lo && bytes.get(hi - 1).is_some_and(u8::is_ascii_whitespace) {
        hi -= 1;
    }
    Span::new(lo as u32, hi as u32)
}

fn declaration_range(source: &str, span: Span) -> Span {
    let bytes = source.as_bytes();
    let mut hi = span.hi as usize;
    while hi > span.lo as usize && bytes.get(hi - 1).is_some_and(u8::is_ascii_whitespace) {
        hi -= 1;
    }
    if bytes.get(hi) == Some(&b';') {
        hi += 1;
    }
    Span::new(declaration_prefix_start(source, span.lo), hi as u32)
}

fn declaration_prefix_start(source: &str, declaration_lo: u32) -> u32 {
    let bytes = source.as_bytes();
    let declaration_lo = declaration_lo as usize;
    let line_start = bytes[..declaration_lo]
        .iter()
        .rposition(|byte| matches!(*byte, b'\n' | b';' | b'{' | b'}'))
        .map_or(0, |position| position + 1);
    let mut start = first_non_whitespace(bytes, line_start, declaration_lo);
    if start == declaration_lo {
        start = declaration_lo;
    }

    let mut previous_end = line_start.saturating_sub(1);
    while previous_end > 0 {
        let previous_start = bytes[..previous_end]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |position| position + 1);
        let content_start = first_non_whitespace(bytes, previous_start, previous_end);
        let mut content_end = previous_end;
        while content_end > content_start
            && bytes
                .get(content_end - 1)
                .is_some_and(u8::is_ascii_whitespace)
        {
            content_end -= 1;
        }
        let line = source.get(content_start..content_end).unwrap_or_default();
        if line.is_empty() {
            break;
        }
        if is_declaration_prefix_line(line) {
            start = content_start;
            previous_end = previous_start.saturating_sub(1);
            continue;
        }
        if matches!(line.as_bytes().last(), Some(b')' | b']')) {
            if let Some(annotation_start) =
                multiline_annotation_start(source, previous_end, content_start)
            {
                start = annotation_start;
                previous_end = bytes[..annotation_start]
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .unwrap_or(0);
                continue;
            }
        }
        break;
    }
    start as u32
}

fn multiline_annotation_start(
    source: &str,
    mut line_end: usize,
    first_content_start: usize,
) -> Option<usize> {
    let bytes = source.as_bytes();
    loop {
        let line_start = bytes[..line_end]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |position| position + 1);
        let content_start = first_non_whitespace(bytes, line_start, line_end);
        let mut content_end = line_end;
        while content_end > content_start
            && bytes
                .get(content_end - 1)
                .is_some_and(u8::is_ascii_whitespace)
        {
            content_end -= 1;
        }
        let line = source.get(content_start..content_end).unwrap_or_default();
        if line.starts_with('@') {
            return Some(content_start);
        }
        if line.is_empty()
            || line_starts_declaration(line)
            || matches!(line.as_bytes().last(), Some(b';' | b'{' | b'}'))
        {
            return None;
        }
        if line_start == 0 {
            return None;
        }
        line_end = line_start.saturating_sub(1);
        if first_content_start.saturating_sub(line_end) > 64 * 1024 {
            return None;
        }
    }
}

fn line_starts_declaration(line: &str) -> bool {
    let first = line.split_ascii_whitespace().next().unwrap_or_default();
    matches!(
        first,
        "class"
            | "data"
            | "enum"
            | "fun"
            | "interface"
            | "object"
            | "typealias"
            | "val"
            | "value"
            | "var"
    )
}

fn first_non_whitespace(bytes: &[u8], mut start: usize, end: usize) -> usize {
    while start < end && bytes.get(start).is_some_and(u8::is_ascii_whitespace) {
        start += 1;
    }
    start
}

fn is_declaration_prefix_line(line: &str) -> bool {
    if line.starts_with('@') || line.starts_with("context(") {
        return true;
    }
    line.split_ascii_whitespace().all(|word| {
        matches!(
            word,
            "actual"
                | "abstract"
                | "annotation"
                | "const"
                | "crossinline"
                | "data"
                | "enum"
                | "expect"
                | "external"
                | "final"
                | "fun"
                | "infix"
                | "inline"
                | "inner"
                | "internal"
                | "lateinit"
                | "noinline"
                | "open"
                | "operator"
                | "out"
                | "override"
                | "private"
                | "protected"
                | "public"
                | "reified"
                | "sealed"
                | "suspend"
                | "tailrec"
                | "value"
                | "vararg"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler_analysis::analyze_standalone_source_set;

    #[test]
    fn extraction_budget_stops_before_building_later_symbol_nodes() {
        let source = "fun first(): Int = 1\nfun second(): Int = 2\nfun third(): Int = 3\n";
        let mut analysis = analyze_standalone_source_set(&[source]);
        let file = analysis.files.pop().unwrap();

        let occurrences = document_symbol_occurrences(source, &file.file, 1);
        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0].name, "first");
        assert!(document_symbol_occurrences(source, &file.file, 0).is_empty());
    }

    #[test]
    fn annotation_argument_commas_are_not_declaration_boundaries() {
        let source = "@Deprecated(\"old\", ReplaceWith(\"fresh\")) val old = 1\n\
                      class C(@Other(\"a\", \"b\") val value: Int)\n\
                      class Defaults(\n\
                        val values: List<Pair<Int, String>> = emptyList<Pair<Int, String>>(),\n\
                        val flag: Boolean = 1 < 2,\n\
                        val next: Int\n\
                      )\n\
                      enum class E { @Deprecated(\"old\", ReplaceWith(\"new\")) A, /* prior */ B }\n";
        let mut analysis = analyze_standalone_source_set(&[source]);
        let file = analysis.files.pop().unwrap();
        let occurrences = document_symbol_occurrences(source, &file.file, 32);
        let range_text = |name: &str| {
            let occurrence = occurrences
                .iter()
                .find(|occurrence| occurrence.name == name)
                .unwrap_or_else(|| panic!("{name} occurrence"));
            &source[occurrence.range.lo as usize..occurrence.range.hi as usize]
        };

        assert_eq!(
            range_text("old"),
            "@Deprecated(\"old\", ReplaceWith(\"fresh\")) val old = 1"
        );
        assert_eq!(range_text("value"), "@Other(\"a\", \"b\") val value: Int");
        assert_eq!(
            range_text("values"),
            "val values: List<Pair<Int, String>> = emptyList<Pair<Int, String>>()"
        );
        assert_eq!(range_text("flag"), "val flag: Boolean = 1 < 2");
        assert_eq!(range_text("next"), "val next: Int");
        assert_eq!(
            range_text("A"),
            "@Deprecated(\"old\", ReplaceWith(\"new\")) A,"
        );
        assert_eq!(range_text("B"), "B");
        assert!(occurrences
            .iter()
            .find(|occurrence| occurrence.name == "old")
            .is_some_and(|occurrence| occurrence.deprecated));
    }

    #[test]
    fn class_header_scan_stays_within_the_declaration() {
        for malformed in [
            "class Sample \"text\"\n",
            "class Sample \"\"\"text\"\"\"\n",
            "class Sample `name`\n",
            "class Sample 'x'\n",
            "class Sampl\u{00e9} \"x\"\n",
            "class Sample constructor \"x\"\n",
            "class Sample @Ann \"x\"\n",
        ] {
            let source = format!("{malformed}class Later(val value: Int)\n");
            let later = source.find("class Later").expect("later declaration");
            let mut analysis = analyze_standalone_source_set(&[&source]);
            let file = analysis.files.pop().expect("analyzed file");
            let occurrences = document_symbol_occurrences(&source, &file.file, 64);
            assert!(occurrences.iter().any(
                |occurrence| occurrence.kind == SYMBOL_KIND_CLASS && occurrence.name == "Later"
            ));
            assert!(occurrences
                .iter()
                .filter(|occurrence| occurrence.kind == SYMBOL_KIND_CONSTRUCTOR)
                .all(|occurrence| occurrence.name == "Later"
                    && occurrence.range.lo as usize >= later));
            assert!(occurrences.iter().all(|occurrence| {
                let lo = occurrence.range.lo as usize;
                let hi = occurrence.range.hi as usize;
                lo <= hi
                    && hi <= source.len()
                    && source.is_char_boundary(lo)
                    && source.is_char_boundary(hi)
            }));
        }

        let source = "class Sample /* unterminated\n";
        let mut analysis = analyze_standalone_source_set(&[source]);
        let file = analysis.files.pop().expect("analyzed file");
        let occurrences = document_symbol_occurrences(source, &file.file, 64);
        assert!(occurrences
            .iter()
            .all(|occurrence| occurrence.kind != SYMBOL_KIND_CONSTRUCTOR));
    }

    #[test]
    fn class_header_scan_terminates_on_stray_quotes() {
        // A header whose scan runs past the declaration bound and then meets a quote used to
        // reset the cursor backwards and rescan the same bytes forever, wedging the analysis
        // thread on a single file. Asserted end to end, so it holds whichever way the
        // individual scan helpers are bounded.
        for source in [
            "class Foo \"bar\"\n",
            "class Foo \"\"\"bar\"\"\"\n",
            "class Foo `bar`\n",
            "class Foo 'x'\n",
            "class Fo\u{00e9} \"x\"\n",
            "class Foo constructor \"x\"\n",
            "class Foo @Ann \"x\"\n",
            "class Foo /* unterminated\n",
        ] {
            let mut analysis = analyze_standalone_source_set(&[source]);
            let file = analysis.files.pop().expect("analyzed file");
            let _ = document_symbol_occurrences(source, &file.file, 64);
        }
    }

    #[test]
    fn parameter_separator_discards_nested_generic_commas_once_per_depth() {
        let parameter = b"Map<A, Map<B, Map<C, D>>> = emptyMap<A, Map<B, C>>(), val next: Int";
        let separator = parameter_separator(parameter, 0, parameter.len()).expect("separator");
        assert_eq!(&parameter[separator..], b", val next: Int");

        let comparisons = b"a < b || c < d || e < f, val next: Int";
        let separator = parameter_separator(comparisons, 0, comparisons.len()).expect("separator");
        assert_eq!(&comparisons[separator..], b", val next: Int");
    }
}

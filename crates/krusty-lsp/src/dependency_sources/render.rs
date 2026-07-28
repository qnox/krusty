//! Render classpath declarations as browsable Kotlin source.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use krusty::diag::Span;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::jvm::metadata::{KotlinMeta, MetaFn, MetaProp};
use krusty::libraries::{LibraryMember, LibraryType, TypeKind};
use krusty::symbol_source::SymbolSource;
use krusty::types::{type_name, TypeName};

use crate::compiler_analysis::render_ty as render_semantic_ty;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberKey {
    pub name: String,
    pub descriptor: String,
}

pub struct RenderedClass {
    pub text: String,
    pub members: Vec<(MemberKey, Span)>,
    pub type_span: Span,
}

pub enum MaterializedSource {
    Attached { text: String, span: Span },
    Rendered(RenderedClass),
}

impl MaterializedSource {
    pub fn into_text_and_span(self, member_name: &str, member_descriptor: &str) -> (String, Span) {
        match self {
            MaterializedSource::Rendered(rendered) => {
                let span = rendered
                    .members
                    .iter()
                    .find(|(key, _)| {
                        key.name == member_name
                            && (member_descriptor.is_empty() || key.descriptor == member_descriptor)
                    })
                    .or_else(|| {
                        rendered
                            .members
                            .iter()
                            .find(|(key, _)| key.name == member_name)
                    })
                    .map_or(rendered.type_span, |(_, span)| *span);
                (rendered.text, span)
            }
            MaterializedSource::Attached { text, span } => (text, span),
        }
    }
}

/// Prefer an attached source file and otherwise render a declaration stub.
pub fn materialize(
    classpath: &Rc<Classpath>,
    internal: &str,
    member_name: &str,
    member_descriptor: &str,
    use_sources: bool,
) -> Option<MaterializedSource> {
    if use_sources {
        if let Some((text, span)) = classpath.declaring_jar(internal).and_then(|jar| {
            attached_source_with_descriptor(&jar, internal, member_name, member_descriptor)
        }) {
            return Some(MaterializedSource::Attached { text, span });
        }
    }
    let libraries = JvmLibraries::new(classpath.clone());
    let mut lib = libraries.resolve_type(internal)?;
    let mut meta = classpath
        .find(internal)
        .map(|class| class.meta.clone())
        .filter(KotlinMeta::is_present);
    if classpath.builtin_is_interface(internal).is_some() {
        lib.supertypes = classpath.builtin_supertypes_name(type_name(internal));
        lib.members = classpath.builtin_members(internal);
        meta = None;
    }
    Some(MaterializedSource::Rendered(render_library_class(
        internal,
        &lib,
        meta.as_ref(),
    )))
}

pub fn render_library_class(
    internal: &str,
    lib: &LibraryType,
    meta: Option<&KotlinMeta>,
) -> RenderedClass {
    let (package, class_name) = match internal.rsplit_once('/') {
        Some((pkg, name)) => (pkg.replace('/', "."), name.to_string()),
        None => (String::new(), internal.to_string()),
    };

    let mut text = String::new();
    if !package.is_empty() {
        text.push_str("package ");
        text.push_str(&package);
        text.push_str("\n\n");
    }

    text.push_str(keyword(lib.kind));
    text.push(' ');

    let type_lo = text.len() as u32;
    text.push_str(&class_name);
    let type_hi = text.len() as u32;

    if !lib.type_params.is_empty() {
        text.push('<');
        text.push_str(&lib.type_params.join(", "));
        text.push('>');
    }

    let mut retained_supertypes = Vec::new();
    for name in lib
        .supertypes
        .iter()
        .filter(|name| !name.matches("kotlin/Any") && !name.matches("java/lang/Object"))
    {
        if retained_supertypes
            .iter()
            .copied()
            .any(|seen| krusty::jvm::jvm_class_map::type_names_map_to_same_jvm_internal(seen, name))
        {
            continue;
        }
        retained_supertypes.push(name);
    }
    let supertypes: Vec<String> = retained_supertypes
        .iter()
        .copied()
        .map(|name| {
            let simple = simple_name(name);
            let ambiguous = retained_supertypes
                .iter()
                .copied()
                .filter(|other| simple_name(*other) == simple)
                .count()
                > 1;
            if ambiguous {
                name.render().replace(['/', '$'], ".")
            } else {
                simple
            }
        })
        .collect();
    if !supertypes.is_empty() {
        text.push_str(" : ");
        text.push_str(&supertypes.join(", "));
    }

    text.push_str(" {\n");
    let mut members = Vec::new();
    if lib.kind == TypeKind::Enum && !lib.enum_entries.is_empty() {
        let last = lib.enum_entries.len() - 1;
        for (i, entry) in lib.enum_entries.iter().enumerate() {
            text.push_str("    ");
            let entry_lo = text.len() as u32;
            text.push_str(entry);
            let entry_hi = text.len() as u32;
            members.push((
                MemberKey {
                    name: entry.clone(),
                    descriptor: String::new(),
                },
                Span::new(entry_lo, entry_hi),
            ));
            text.push_str(if i == last { ";\n" } else { ",\n" });
        }
    }
    if let Some(meta) = meta {
        render_properties(&mut text, &mut members, &meta.class_properties);
        render_properties(&mut text, &mut members, &meta.package_properties);
        render_functions(&mut text, &mut members, &meta.class_functions);
        render_functions(&mut text, &mut members, &meta.package_functions);
    } else {
        render_library_members(&mut text, &mut members, &lib.members);
    }
    if lib.companion_object.is_some() {
        text.push_str("    companion object {}\n");
    }
    text.push_str("}\n");

    RenderedClass {
        text,
        members,
        type_span: Span::new(type_lo, type_hi),
    }
}

fn render_library_members(
    text: &mut String,
    members: &mut Vec<(MemberKey, Span)>,
    library_members: &[LibraryMember],
) {
    for member in library_members
        .iter()
        .filter(|member| !member.name.starts_with('<'))
    {
        text.push_str("    fun ");
        let lo = text.len() as u32;
        text.push_str(&member.name);
        let hi = text.len() as u32;
        members.push((
            MemberKey {
                name: member.name.clone(),
                descriptor: member.descriptor.clone(),
            },
            Span::new(lo, hi),
        ));
        text.push('(');
        for (index, parameter) in member.params.iter().enumerate() {
            if index > 0 {
                text.push_str(", ");
            }
            text.push_str(&format!("p{index}: {}", render_semantic_ty(*parameter)));
        }
        text.push_str("): ");
        text.push_str(&render_semantic_ty(member.ret));
        text.push_str(" { TODO() }\n");
    }
}

fn render_functions(text: &mut String, members: &mut Vec<(MemberKey, Span)>, fns: &[MetaFn]) {
    for f in fns {
        text.push_str("    ");
        if f.is_suspend() {
            text.push_str("suspend ");
        }
        if f.is_inline() {
            text.push_str("inline ");
        }
        text.push_str("fun ");
        if let Some(formals) = f.generic_sig.as_ref().map(|sig| &sig.formals) {
            if !formals.is_empty() {
                text.push('<');
                text.push_str(&formals.join(", "));
                text.push_str("> ");
            }
        }
        if let Some(receiver) = f.receiver_class {
            text.push_str(&simple_name(receiver));
            text.push('.');
        }

        let name_lo = text.len() as u32;
        text.push_str(&f.kotlin_name);
        let name_hi = text.len() as u32;
        members.push((
            MemberKey {
                name: f.jvm_name.clone(),
                descriptor: f.jvm_desc.unwrap_or_default().to_string(),
            },
            Span::new(name_lo, name_hi),
        ));

        text.push('(');
        let params: Vec<String> = f
            .value_params
            .iter()
            .map(|p| {
                let vararg = if p.vararg() { "vararg " } else { "" };
                format!("{vararg}{}: {}", p.name, render_type(p.ty))
            })
            .collect();
        text.push_str(&params.join(", "));
        text.push(')');

        if let Some(ret) = f.ret_class {
            text.push_str(": ");
            text.push_str(&render_type(Some(ret)));
            if f.ret_nullable() {
                text.push('?');
            }
        }
        text.push_str(" { TODO() }\n");
    }
}

fn render_properties(text: &mut String, members: &mut Vec<(MemberKey, Span)>, props: &[MetaProp]) {
    for p in props {
        text.push_str("    ");
        if p.is_const {
            text.push_str("const ");
        }
        text.push_str(if p.is_var { "var " } else { "val " });
        if let Some(receiver) = p.receiver_class {
            text.push_str(&simple_name(receiver));
            text.push('.');
        }

        let name_lo = text.len() as u32;
        text.push_str(&p.name);
        let name_hi = text.len() as u32;
        let (key_name, key_desc) = match &p.getter {
            Some(getter) => (getter.name.clone(), getter.desc.clone()),
            None => (p.name.clone(), String::new()),
        };
        members.push((
            MemberKey {
                name: key_name,
                descriptor: key_desc,
            },
            Span::new(name_lo, name_hi),
        ));

        text.push_str(": ");
        text.push_str(&render_type(p.ret_class));
        if p.ret_nullable {
            text.push('?');
        }
        text.push('\n');
    }
}

pub fn attached_source(
    classes_jar: &Path,
    internal: &str,
    member_name: &str,
) -> Option<(String, Span)> {
    attached_source_with_descriptor(classes_jar, internal, member_name, "")
}

fn attached_source_with_descriptor(
    classes_jar: &Path,
    internal: &str,
    member_name: &str,
    member_descriptor: &str,
) -> Option<(String, Span)> {
    sources_jar_paths(classes_jar)
        .into_iter()
        .find_map(|jar| attached_source_in(&jar, internal, member_name, member_descriptor))
}

const MAX_ATTACHED_SOURCE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SCANNED_SOURCE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Ident,
    KwPackage,
    KwClass,
    KwFun,
    KwVal,
    KwVar,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Dot,
    Eq,
    Lt,
    Gt,
    Newline,
    Eof,
    Unknown,
}

#[derive(Clone, Copy)]
struct Token {
    kind: TokenKind,
    span: Span,
}

impl Token {
    fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.span.lo as usize..self.span.hi as usize]
    }
}

fn declaration_tokens(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let start = index;
        let kind = match bytes[index] {
            b' ' | b'\t' | b'\r' => {
                index += 1;
                continue;
            }
            b'\n' | b';' => {
                index += 1;
                TokenKind::Newline
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index + 2);
                continue;
            }
            b'"' => {
                index = skip_quoted(bytes, index, b'"');
                continue;
            }
            b'\'' => {
                index = skip_quoted(bytes, index, b'\'');
                continue;
            }
            b'`' => {
                index += 1;
                let name_start = index;
                while index < bytes.len() && bytes[index] != b'`' {
                    index += 1;
                }
                let name_end = index;
                index += usize::from(index < bytes.len());
                tokens.push(Token {
                    kind: TokenKind::Ident,
                    span: Span::new(name_start as u32, name_end as u32),
                });
                continue;
            }
            b'(' => one(&mut index, TokenKind::LParen),
            b')' => one(&mut index, TokenKind::RParen),
            b'{' => one(&mut index, TokenKind::LBrace),
            b'}' => one(&mut index, TokenKind::RBrace),
            b'[' => one(&mut index, TokenKind::LBracket),
            b']' => one(&mut index, TokenKind::RBracket),
            b',' => one(&mut index, TokenKind::Comma),
            b':' => one(&mut index, TokenKind::Colon),
            b'.' => one(&mut index, TokenKind::Dot),
            b'=' => one(&mut index, TokenKind::Eq),
            b'<' => one(&mut index, TokenKind::Lt),
            b'>' => one(&mut index, TokenKind::Gt),
            byte if is_identifier_start(byte) => {
                index += 1;
                while index < bytes.len() && is_identifier_part(bytes[index]) {
                    index += 1;
                }
                match &source[start..index] {
                    "package" => TokenKind::KwPackage,
                    "class" => TokenKind::KwClass,
                    "fun" => TokenKind::KwFun,
                    "val" => TokenKind::KwVal,
                    "var" => TokenKind::KwVar,
                    "return" | "throw" | "if" | "else" | "when" | "while" | "for" | "do"
                    | "new" => TokenKind::Unknown,
                    _ => TokenKind::Ident,
                }
            }
            byte if byte >= 0x80 => {
                index += 1;
                while index < bytes.len() && is_identifier_part(bytes[index]) {
                    index += 1;
                }
                TokenKind::Ident
            }
            _ => one(&mut index, TokenKind::Unknown),
        };
        tokens.push(Token {
            kind,
            span: Span::new(start as u32, index as u32),
        });
    }
    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span::new(index as u32, index as u32),
    });
    tokens
}

fn one(index: &mut usize, kind: TokenKind) -> TokenKind {
    *index += 1;
    kind
}

fn is_identifier_start(byte: u8) -> bool {
    byte == b'_' || byte == b'$' || byte.is_ascii_alphabetic()
}

fn is_identifier_part(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit() || byte >= 0x80
}

fn skip_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
    if quote == b'"' && bytes.get(start..start + 3) == Some(b"\"\"\"") {
        let mut index = start + 3;
        while index + 2 < bytes.len() && &bytes[index..index + 3] != b"\"\"\"" {
            index += 1;
        }
        return (index + 3).min(bytes.len());
    }
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index = (index + 2).min(bytes.len()),
            byte if byte == quote => return index + 1,
            _ => index += 1,
        }
    }
    index
}

fn skip_block_comment(bytes: &[u8], mut index: usize) -> usize {
    let mut depth = 1usize;
    while index < bytes.len() && depth > 0 {
        if bytes.get(index..index + 2) == Some(b"/*") {
            depth += 1;
            index += 2;
        } else if bytes.get(index..index + 2) == Some(b"*/") {
            depth -= 1;
            index += 2;
        } else {
            index += 1;
        }
    }
    index
}

fn attached_source_in(
    sources_jar: &Path,
    internal: &str,
    member_name: &str,
    member_descriptor: &str,
) -> Option<(String, Span)> {
    let file = std::fs::File::open(sources_jar).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let (package, class_name) = internal.rsplit_once('/').unwrap_or(("", internal));
    let outer_name = class_name.split('$').next().unwrap_or(class_name);
    let mut names = vec![class_name];
    if outer_name != class_name {
        names.push(outer_name);
    }
    if let Some(facade_name) = outer_name.strip_suffix("Kt") {
        if !names.contains(&facade_name) {
            names.push(facade_name);
        }
    }
    let entries: Vec<String> = archive.file_names().map(str::to_string).collect();
    let candidates = source_entry_candidates(&entries, package, &names);

    let mut fallback = None;
    let mut scanned = 0u64;
    for candidate in candidates {
        let Ok(mut entry) = archive.by_name(&candidate) else {
            continue;
        };
        if entry.size() > MAX_ATTACHED_SOURCE_BYTES {
            continue;
        }
        scanned += entry.size();
        if scanned > MAX_SCANNED_SOURCE_BYTES {
            break;
        }
        let mut text = String::new();
        if entry.read_to_string(&mut text).is_err() {
            continue;
        }
        let found = source_declaration(&text, package, class_name, member_name, member_descriptor);
        match found {
            Some(declaration) if !declaration.is_expect => return Some((text, declaration.span)),
            Some(declaration) => {
                fallback.get_or_insert((text, declaration.span));
            }
            None => continue,
        }
    }
    fallback
}

fn source_entry_candidates(entries: &[String], package: &str, names: &[&str]) -> Vec<String> {
    let mut exact = Vec::new();
    let mut by_file_name = Vec::new();
    let mut in_package = Vec::new();
    let mut remaining = Vec::new();
    for entry in entries {
        let (directory, file) = entry.rsplit_once('/').unwrap_or(("", entry));
        let Some(stem) = file
            .strip_suffix(".kt")
            .or_else(|| file.strip_suffix(".java"))
        else {
            continue;
        };
        let named = names.contains(&stem);
        let in_package_directory = directory == package
            || (!package.is_empty() && directory.ends_with(&format!("/{package}")));
        if named && in_package_directory {
            exact.push(entry.clone());
        } else if named {
            by_file_name.push(entry.clone());
        } else if in_package_directory {
            in_package.push(entry.clone());
        } else {
            remaining.push(entry.clone());
        }
    }
    exact.sort();
    by_file_name.sort();
    in_package.sort();
    remaining.sort();
    exact.extend(by_file_name);
    exact.extend(in_package);
    exact.extend(remaining);
    exact
}

#[derive(Clone, Copy)]
struct Declaration {
    span: Span,
    is_expect: bool,
}

#[derive(Clone, Copy)]
struct TypeDeclaration {
    declaration: Declaration,
    body: Option<(usize, usize, u32)>,
}

fn source_declaration(
    text: &str,
    package: &str,
    class_name: &str,
    member_name: &str,
    member_descriptor: &str,
) -> Option<Declaration> {
    let tokens = declaration_tokens(text);
    if source_package(&tokens, text).replace('.', "/") != package {
        return None;
    }
    let depths = token_depths(&tokens);
    let mut range = (0, tokens.len());
    let mut depth = 0;
    let names = class_name.split('$').collect::<Vec<_>>();
    let mut selected = None;
    let mut complete = true;
    for name in &names {
        let Some(found) = find_type_declaration(&tokens, &depths, text, name, range, depth) else {
            complete = false;
            break;
        };
        selected = Some(found);
        let Some((start, end, member_depth)) = found.body else {
            break;
        };
        range = (start, end);
        depth = member_depth;
    }
    if let Some(found) = selected.filter(|_| complete) {
        if !member_name.is_empty() {
            if let Some((start, end, member_depth)) = found.body {
                if let Some(member) = find_callable_declaration(
                    &tokens,
                    &depths,
                    text,
                    member_name,
                    member_descriptor,
                    (start, end),
                    member_depth,
                ) {
                    return Some(member);
                }
            }
        }
        return Some(found.declaration);
    }
    find_callable_declaration(
        &tokens,
        &depths,
        text,
        member_name,
        member_descriptor,
        (0, tokens.len()),
        0,
    )
}

fn source_package(tokens: &[Token], text: &str) -> String {
    let Some(start) = tokens
        .iter()
        .position(|token| token.kind == TokenKind::KwPackage)
    else {
        return String::new();
    };
    let mut package = String::new();
    for token in &tokens[start + 1..] {
        match token.kind {
            TokenKind::Ident => package.push_str(token.text(text)),
            TokenKind::Dot => package.push('.'),
            TokenKind::Newline | TokenKind::Eof | TokenKind::Unknown => break,
            _ => {}
        }
    }
    package
}

fn token_depths(tokens: &[Token]) -> Vec<u32> {
    let mut depth = 0u32;
    tokens
        .iter()
        .map(|token| {
            if token.kind == TokenKind::RBrace {
                depth = depth.saturating_sub(1);
            }
            let current = depth;
            if token.kind == TokenKind::LBrace {
                depth += 1;
            }
            current
        })
        .collect()
}

fn find_type_declaration(
    tokens: &[Token],
    depths: &[u32],
    text: &str,
    name: &str,
    range: (usize, usize),
    depth: u32,
) -> Option<TypeDeclaration> {
    if name.is_empty() {
        return None;
    }
    for index in range.0..range.1 {
        if depths[index] != depth || !is_type_keyword(&tokens[index], text) {
            continue;
        }
        if name == "Companion"
            && tokens[index].kind == TokenKind::Ident
            && tokens[index].text(text) == "object"
            && previous_non_newline(tokens, index)
                .is_some_and(|previous| tokens[previous].text(text) == "companion")
        {
            let name_index = previous_non_newline(tokens, index)?;
            return Some(TypeDeclaration {
                declaration: Declaration {
                    span: tokens[name_index].span,
                    is_expect: false,
                },
                body: type_body(tokens, depths, text, index + 1, range.1, depth),
            });
        }
        let Some(name_index) = next_identifier(tokens, index + 1, range.1) else {
            continue;
        };
        if tokens[name_index].text(text) != name {
            continue;
        }
        let declaration = Declaration {
            span: tokens[name_index].span,
            is_expect: has_line_modifier(tokens, text, index, "expect"),
        };
        let body = if tokens[index].text(text) == "typealias" {
            None
        } else {
            type_body(tokens, depths, text, name_index + 1, range.1, depth)
        };
        return Some(TypeDeclaration { declaration, body });
    }
    None
}

fn is_type_keyword(token: &Token, text: &str) -> bool {
    token.kind == TokenKind::KwClass
        || (token.kind == TokenKind::Ident
            && matches!(token.text(text), "interface" | "object" | "typealias"))
}

fn next_identifier(tokens: &[Token], mut index: usize, end: usize) -> Option<usize> {
    while index < end {
        match tokens[index].kind {
            TokenKind::Ident => return Some(index),
            TokenKind::Newline => index += 1,
            _ => return None,
        }
    }
    None
}

fn type_body(
    tokens: &[Token],
    depths: &[u32],
    text: &str,
    start: usize,
    end: usize,
    depth: u32,
) -> Option<(usize, usize, u32)> {
    let mut crossed_line = false;
    for index in start..end {
        if depths[index] < depth {
            return None;
        }
        if depths[index] != depth {
            continue;
        }
        if tokens[index].kind == TokenKind::Newline {
            crossed_line = true;
            continue;
        }
        if crossed_line
            && (is_type_keyword(&tokens[index], text)
                || matches!(
                    tokens[index].kind,
                    TokenKind::KwFun | TokenKind::KwVal | TokenKind::KwVar
                ))
        {
            return None;
        }
        if tokens[index].kind == TokenKind::LBrace {
            let close = (index + 1..end)
                .find(|candidate| {
                    tokens[*candidate].kind == TokenKind::RBrace && depths[*candidate] == depth
                })
                .unwrap_or(end);
            return Some((index + 1, close, depth + 1));
        }
    }
    None
}

fn find_callable_declaration(
    tokens: &[Token],
    depths: &[u32],
    text: &str,
    name: &str,
    descriptor: &str,
    range: (usize, usize),
    depth: u32,
) -> Option<Declaration> {
    if name.is_empty() || name.starts_with('<') {
        return None;
    }
    let expected_arity = descriptor_arity(descriptor);
    let mut fallback = None;
    for index in range.0..range.1 {
        if depths[index] != depth {
            continue;
        }
        match tokens[index].kind {
            TokenKind::KwFun => {
                if let Some((span, arity)) =
                    function_name(tokens, depths, text, name, index + 1, range.1, depth)
                {
                    let declaration = Declaration {
                        span,
                        is_expect: has_line_modifier(tokens, text, index, "expect"),
                    };
                    if expected_arity.is_none_or(|expected| expected == arity) {
                        return Some(declaration);
                    }
                    fallback.get_or_insert(declaration);
                }
            }
            TokenKind::KwVal | TokenKind::KwVar => {
                if let Some(span) =
                    property_name(tokens, depths, text, name, index + 1, range.1, depth)
                {
                    return Some(Declaration {
                        span,
                        is_expect: has_line_modifier(tokens, text, index, "expect"),
                    });
                }
            }
            TokenKind::Ident if tokens[index].text(text) == name => {
                let next = next_non_newline(tokens, index + 1, range.1);
                let previous = previous_non_newline(tokens, index);
                if next.is_some_and(|next| tokens[next].kind == TokenKind::LParen)
                    && previous.is_some_and(|previous| {
                        matches!(
                            tokens[previous].kind,
                            TokenKind::Ident | TokenKind::RBracket | TokenKind::Gt
                        )
                    })
                {
                    let declaration = Declaration {
                        span: tokens[index].span,
                        is_expect: false,
                    };
                    let arity = next.and_then(|open| parameter_arity(tokens, open, range.1));
                    if expected_arity
                        .zip(arity)
                        .is_none_or(|(expected, actual)| expected == actual)
                    {
                        return Some(declaration);
                    }
                    fallback.get_or_insert(declaration);
                }
            }
            _ => {}
        }
    }
    fallback
}

fn function_name(
    tokens: &[Token],
    depths: &[u32],
    text: &str,
    name: &str,
    start: usize,
    end: usize,
    depth: u32,
) -> Option<(Span, usize)> {
    for index in start..end {
        if depths[index] != depth {
            continue;
        }
        if tokens[index].kind == TokenKind::Ident
            && tokens[index].text(text) == name
            && next_non_newline(tokens, index + 1, end)
                .is_some_and(|next| tokens[next].kind == TokenKind::LParen)
        {
            let open = next_non_newline(tokens, index + 1, end)?;
            let mut arity = parameter_arity(tokens, open, end)?;
            if tokens[start..index]
                .iter()
                .any(|token| token.kind == TokenKind::Dot)
            {
                arity += 1;
            }
            if has_line_modifier(tokens, text, start.saturating_sub(1), "suspend") {
                arity += 1;
            }
            return Some((tokens[index].span, arity));
        }
        if matches!(
            tokens[index].kind,
            TokenKind::Eq | TokenKind::LBrace | TokenKind::Eof
        ) {
            return None;
        }
    }
    None
}

fn parameter_arity(tokens: &[Token], open: usize, end: usize) -> Option<usize> {
    if tokens.get(open)?.kind != TokenKind::LParen {
        return None;
    }
    let mut parens = 1u32;
    let mut brackets = 0u32;
    let mut angles = 0u32;
    let mut commas = 0usize;
    let mut has_parameter = false;
    for token in &tokens[open + 1..end] {
        match token.kind {
            TokenKind::LParen => parens += 1,
            TokenKind::RParen => {
                parens -= 1;
                if parens == 0 {
                    return Some(usize::from(has_parameter) + commas);
                }
            }
            TokenKind::LBracket => brackets += 1,
            TokenKind::RBracket => brackets = brackets.saturating_sub(1),
            TokenKind::Lt => angles += 1,
            TokenKind::Gt => angles = angles.saturating_sub(1),
            TokenKind::Comma if parens == 1 && brackets == 0 && angles == 0 => commas += 1,
            TokenKind::Newline => {}
            _ if parens == 1 => has_parameter = true,
            _ => {}
        }
    }
    None
}

fn descriptor_arity(descriptor: &str) -> Option<usize> {
    let bytes = descriptor.as_bytes();
    if bytes.first() != Some(&b'(') {
        return None;
    }
    let mut index = 1;
    let mut arity = 0;
    while index < bytes.len() && bytes[index] != b')' {
        while bytes.get(index) == Some(&b'[') {
            index += 1;
        }
        match bytes.get(index).copied()? {
            b'L' => {
                index += bytes[index..].iter().position(|byte| *byte == b';')? + 1;
            }
            b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' => index += 1,
            _ => return None,
        }
        arity += 1;
    }
    (bytes.get(index) == Some(&b')')).then_some(arity)
}

fn property_name(
    tokens: &[Token],
    depths: &[u32],
    text: &str,
    name: &str,
    start: usize,
    end: usize,
    depth: u32,
) -> Option<Span> {
    let mut candidate = None;
    for index in start..end {
        if depths[index] != depth {
            continue;
        }
        match tokens[index].kind {
            TokenKind::Ident => candidate = Some(index),
            TokenKind::Colon | TokenKind::Eq | TokenKind::Newline | TokenKind::Eof => {
                return candidate
                    .filter(|candidate| tokens[*candidate].text(text) == name)
                    .map(|candidate| tokens[candidate].span);
            }
            TokenKind::LBrace => return None,
            _ => {}
        }
    }
    None
}

fn next_non_newline(tokens: &[Token], mut index: usize, end: usize) -> Option<usize> {
    while index < end && tokens[index].kind == TokenKind::Newline {
        index += 1;
    }
    (index < end).then_some(index)
}

fn previous_non_newline(tokens: &[Token], mut index: usize) -> Option<usize> {
    while index > 0 {
        index -= 1;
        if tokens[index].kind != TokenKind::Newline {
            return Some(index);
        }
    }
    None
}

fn has_line_modifier(tokens: &[Token], text: &str, index: usize, modifier: &str) -> bool {
    tokens[..index]
        .iter()
        .rev()
        .take_while(|token| {
            !matches!(
                token.kind,
                TokenKind::Newline | TokenKind::LBrace | TokenKind::RBrace
            )
        })
        .any(|token| token.kind == TokenKind::Ident && token.text(text) == modifier)
}

fn sources_jar_paths(classes_jar: &Path) -> Vec<PathBuf> {
    const MAX_SIBLING_DIRECTORIES: usize = 64;
    let Some(name) = classes_jar
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| format!("{stem}-sources.jar"))
    else {
        return Vec::new();
    };
    let mut candidates = vec![classes_jar.with_file_name(&name)];
    let Some(parent) = classes_jar.parent().and_then(Path::parent) else {
        return candidates;
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return candidates;
    };
    let mut sibling_directories: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    sibling_directories.sort();
    sibling_directories.truncate(MAX_SIBLING_DIRECTORIES);
    let siblings = sibling_directories
        .into_iter()
        .map(|directory| directory.join(&name))
        .filter(|candidate| !candidates.contains(candidate) && candidate.is_file())
        .collect::<Vec<_>>();
    candidates.extend(siblings);
    candidates
}

fn keyword(kind: TypeKind) -> &'static str {
    match kind {
        TypeKind::Class => "class",
        TypeKind::Interface => "interface",
        TypeKind::Annotation => "annotation class",
        TypeKind::Enum => "enum class",
        TypeKind::Object => "object",
    }
}

fn render_type(ty: Option<TypeName>) -> String {
    match ty {
        Some(tn) => simple_name(tn),
        None => "Any".to_string(),
    }
}

fn simple_name(tn: TypeName) -> String {
    let rendered = tn.render();
    let simple = rendered.rsplit('/').next().unwrap_or(&rendered);
    simple.replace('$', ".")
}

//! Java source syntax shared by compiler stubs and editor analysis.

use crate::diag::Span;
use std::collections::HashSet;

const ACC_PUBLIC: u16 = 0x0001;
const ACC_PRIVATE: u16 = 0x0002;
const ACC_PROTECTED: u16 = 0x0004;
const ACC_STATIC: u16 = 0x0008;
const ACC_FINAL: u16 = 0x0010;
const ACC_ABSTRACT: u16 = 0x0400;
/// Real JVM flag: the method's last parameter is a `...` vararg (`ACC_VARARGS`).
pub(crate) const ACC_VARARGS: u16 = 0x0080;
pub(crate) const STUB_DEFAULT: u16 = 0x8000;

// --- Tokenizer -------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Punct(char),
}

pub(crate) struct LexedJava {
    tokens: Vec<Tok>,
    spans: Vec<Span>,
}

/// Tokenize Java source into identifiers and single-char punctuation with byte spans.
pub(crate) fn lex_java(src: &str) -> LexedJava {
    let mut i = 0;
    let mut tokens = Vec::new();
    let mut spans = Vec::new();
    while i < src.len() {
        let c = src[i..].chars().next().unwrap();
        let width = c.len_utf8();
        if c.is_whitespace() {
            i += width;
        } else if src[i..].starts_with("//") {
            i += 2;
            while i < src.len() {
                let next = src[i..].chars().next().unwrap();
                if next == '\n' {
                    break;
                }
                i += next.len_utf8();
            }
        } else if src[i..].starts_with("/*") {
            i += 2;
            while i < src.len() && !src[i..].starts_with("*/") {
                i += src[i..].chars().next().unwrap().len_utf8();
            }
            i = (i + 2).min(src.len());
        } else if c == '"' || c == '\'' {
            let quote = c;
            i += width;
            while i < src.len() {
                let next = src[i..].chars().next().unwrap();
                i += next.len_utf8();
                if next == '\\' {
                    if i < src.len() {
                        i += src[i..].chars().next().unwrap().len_utf8();
                    }
                } else if next == quote {
                    break;
                }
            }
        } else if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            i += width;
            while i < src.len() {
                let next = src[i..].chars().next().unwrap();
                if !next.is_alphanumeric() && next != '_' && next != '$' {
                    break;
                }
                i += next.len_utf8();
            }
            tokens.push(Tok::Ident(src[start..i].to_string()));
            spans.push(Span::new(start as u32, i as u32));
        } else {
            tokens.push(Tok::Punct(c));
            spans.push(Span::new(i as u32, (i + width) as u32));
            i += width;
        }
    }
    LexedJava { tokens, spans }
}

// --- Parsed shapes ----------------------------------------------------------

/// Per-file context: package (internal form, `""` = root) and imports.
pub(crate) struct FileCtx {
    pub(crate) package: String,
    pub(crate) imports: Vec<JavaImport>,
}

/// A source-level type reference: base name (dotted as written), generic args, array depth.
#[derive(Clone, Debug)]
pub(crate) struct SrcType {
    pub(crate) name: String,
    pub(crate) args: Vec<SrcType>,
    pub(crate) array: u32,
    pub(crate) span: Option<Span>,
}

/// A member signature: name, params, return (`None` for a constructor), flags, own type params.
pub(crate) struct Member {
    pub(crate) name: String,
    pub(crate) tparams: Vec<(String, Option<SrcType>)>,
    pub(crate) params: Vec<SrcType>,
    pub(crate) ret: Option<SrcType>,
    pub(crate) throws: Vec<SrcType>,
    pub(crate) access: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeclKind {
    Class,
    Interface,
    Enum,
    Record,
    Annotation,
}

/// A parsed type declaration with unresolved source types.
pub(crate) struct RawDecl {
    /// Internal name (`pkg/Outer$Inner`).
    pub(crate) internal: String,
    /// Syntactic enclosing declaration, independent of `$` characters in Java identifiers.
    pub(crate) outer_internal: Option<String>,
    /// Declared identifier, retained because it cannot be recovered by splitting the JVM name: `$`
    /// is legal inside a Java identifier and therefore is not evidence of a nesting boundary.
    pub(crate) simple_name: String,
    pub(crate) name_span: Span,
    pub(crate) access: u16,
    pub(crate) kind: DeclKind,
    pub(crate) is_abstract: bool,
    pub(crate) tparams: Vec<(String, Option<SrcType>)>,
    /// `extends` for a class (`None` = `java/lang/Object`); an interface's `extends` list is in
    /// `interfaces`.
    pub(crate) superclass: Option<SrcType>,
    pub(crate) interfaces: Vec<SrcType>,
    pub(crate) permits: Vec<SrcType>,
    pub(crate) ctors: Vec<Member>,
    pub(crate) methods: Vec<Member>,
    pub(crate) fields: Vec<(String, SrcType, u16)>,
    pub(crate) enum_constants: Vec<String>,
    /// Whether any enum constant declares an anonymous class body. Such an enum is not `final` even
    /// when it has no abstract member; the classfile and `InnerClasses` flags must agree on that fact.
    pub(crate) enum_has_constant_body: bool,
    pub(crate) record_components: Vec<(String, SrcType)>,
    pub(crate) record_is_varargs: bool,
}

impl RawDecl {
    pub(crate) fn is_interface(&self) -> bool {
        matches!(self.kind, DeclKind::Interface | DeclKind::Annotation)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaImport {
    pub path: String,
    pub wildcard: bool,
    pub is_static: bool,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaDeclaration {
    pub internal: String,
    /// Syntactic enclosing declaration. Consumers must use this relation instead of treating `$` in
    /// `internal` as a separator because `$` is also a legal character in a top-level identifier.
    pub outer_internal: Option<String>,
    pub name_span: Span,
    pub private: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaTypeReference {
    pub path: String,
    pub span: Span,
    pub owner: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JavaSourceFile {
    pub package: String,
    pub imports: Vec<JavaImport>,
    pub declarations: Vec<JavaDeclaration>,
    pub references: Vec<JavaTypeReference>,
}

impl JavaSourceFile {
    pub fn resolve_reference_path(
        &self,
        name: &str,
        exists: &dyn Fn(&str) -> bool,
    ) -> Option<String> {
        resolve_internal_name(&self.package, &self.imports, name, exists)
    }

    pub fn resolve_reference(
        &self,
        reference: &JavaTypeReference,
        exists: &dyn Fn(&str) -> bool,
    ) -> Option<String> {
        if let Some(owner) = &reference.owner {
            let nested = reference.path.replace('.', "$");
            let mut scope = Some(owner.as_str());
            while let Some(current) = scope {
                let candidate = format!("{current}${nested}");
                if exists(&candidate) {
                    return Some(candidate);
                }
                scope = self
                    .declarations
                    .iter()
                    .find(|declaration| declaration.internal == current)
                    .and_then(|declaration| declaration.outer_internal.as_deref());
            }
        }
        self.resolve_reference_path(&reference.path, exists)
    }
}

pub fn parse_source_file(source: &str) -> Option<JavaSourceFile> {
    let tokens = lex_java(source);
    let (ctx, declarations, body_refs) = parse_raw_file(&tokens)?;
    let mut references = Vec::new();
    for import in &ctx.imports {
        if !import.wildcard && !import.is_static {
            references.push(JavaTypeReference {
                path: import.path.clone(),
                span: import.span,
                owner: None,
            });
        }
    }
    for declaration in &declarations {
        let mut scope = declaration
            .tparams
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<HashSet<_>>();
        let mut outer = declaration.outer_internal.as_deref();
        while let Some(owner) = outer {
            if let Some(enclosing) = declarations
                .iter()
                .find(|candidate| candidate.internal == owner)
            {
                scope.extend(enclosing.tparams.iter().map(|(name, _)| name.as_str()));
                outer = enclosing.outer_internal.as_deref();
            } else {
                outer = None;
            }
        }
        for (_, bound) in &declaration.tparams {
            if let Some(bound) = bound {
                collect_source_type_references(
                    bound,
                    &scope,
                    Some(&declaration.internal),
                    &mut references,
                );
            }
        }
        if let Some(superclass) = &declaration.superclass {
            collect_source_type_references(
                superclass,
                &scope,
                Some(&declaration.internal),
                &mut references,
            );
        }
        for reference in declaration
            .interfaces
            .iter()
            .chain(&declaration.permits)
            .chain(declaration.fields.iter().map(|(_, ty, _)| ty))
            .chain(declaration.record_components.iter().map(|(_, ty)| ty))
        {
            collect_source_type_references(
                reference,
                &scope,
                Some(&declaration.internal),
                &mut references,
            );
        }
        for member in declaration.ctors.iter().chain(&declaration.methods) {
            let mut member_scope = scope.clone();
            member_scope.extend(member.tparams.iter().map(|(name, _)| name.as_str()));
            for (_, bound) in &member.tparams {
                if let Some(bound) = bound {
                    collect_source_type_references(
                        bound,
                        &member_scope,
                        Some(&declaration.internal),
                        &mut references,
                    );
                }
            }
            for reference in member
                .params
                .iter()
                .chain(member.ret.iter())
                .chain(&member.throws)
            {
                collect_source_type_references(
                    reference,
                    &member_scope,
                    Some(&declaration.internal),
                    &mut references,
                );
            }
        }
    }
    for reference in &body_refs {
        collect_source_type_references(reference, &HashSet::new(), None, &mut references);
    }
    references.sort_unstable_by_key(|reference| {
        (reference.span.lo, reference.span.hi, reference.path.clone())
    });
    references.dedup();
    Some(JavaSourceFile {
        package: ctx.package,
        imports: ctx.imports,
        declarations: declarations
            .into_iter()
            .map(|declaration| JavaDeclaration {
                internal: declaration.internal,
                outer_internal: declaration.outer_internal,
                name_span: declaration.name_span,
                private: declaration.access & ACC_PRIVATE != 0,
            })
            .collect(),
        references,
    })
}

pub fn parse_file(source: &str) -> Option<JavaSourceFile> {
    parse_source_file(source)
}

fn collect_source_type_references(
    reference: &SrcType,
    type_parameters: &HashSet<&str>,
    owner: Option<&str>,
    out: &mut Vec<JavaTypeReference>,
) {
    if primitive_desc(&reference.name).is_none()
        && !type_parameters.contains(reference.name.as_str())
    {
        if let Some(span) = reference.span {
            out.push(JavaTypeReference {
                path: reference.name.clone(),
                span,
                owner: owner.map(str::to_owned),
            });
        }
    }
    for argument in &reference.args {
        collect_source_type_references(argument, type_parameters, owner, out);
    }
}

// --- Parser ----------------------------------------------------------------

struct P<'a> {
    t: &'a [Tok],
    spans: &'a [Span],
    i: usize,
    body_refs: Vec<SrcType>,
}

impl P<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn bump(&mut self) -> Option<&Tok> {
        let t = self.t.get(self.i);
        self.i += 1;
        t
    }
    fn eat_punct(&mut self, c: char) -> bool {
        if self.peek() == Some(&Tok::Punct(c)) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn eat_ident(&mut self, s: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Ident(x)) if x == s) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn ident(&mut self) -> Option<String> {
        match self.t.get(self.i) {
            Some(Tok::Ident(s)) => {
                self.i += 1;
                Some(s.clone())
            }
            _ => None,
        }
    }
    fn span(&self) -> Option<Span> {
        self.spans.get(self.i).copied()
    }
    /// Dotted name `a.b.C` as written. A `.` is consumed only when an identifier FOLLOWS it, so a
    /// trailing `...` (varargs) or `.*` (wildcard import) is left for the caller.
    fn dotted(&mut self) -> Option<String> {
        self.dotted_spanned().map(|(name, _)| name)
    }
    fn dotted_spanned(&mut self) -> Option<(String, Span)> {
        let start = self.span()?;
        let mut s = self.ident()?;
        let mut end = start;
        while self.peek() == Some(&Tok::Punct('.'))
            && matches!(self.t.get(self.i + 1), Some(Tok::Ident(_)))
        {
            self.i += 1;
            s.push('.');
            end = *self.spans.get(self.i)?;
            s.push_str(&self.ident()?);
        }
        Some((s, Span::new(start.lo, end.hi)))
    }
    /// Skip a balanced `{ ... }` (opening brace already consumed).
    fn skip_braces(&mut self) {
        let mut depth = 1;
        while depth > 0 {
            match self.bump() {
                Some(Tok::Punct('{')) => depth += 1,
                Some(Tok::Punct('}')) => depth -= 1,
                Some(_) => {}
                None => return,
            }
        }
    }
    fn skip_body(&mut self) {
        let start = self.i;
        self.skip_braces();
        let end = self.i.saturating_sub(1);
        self.body_refs.extend(explicit_body_type_refs(
            &self.t[start..end],
            &self.spans[start..end],
        ));
    }
    /// Skip an annotation use: `@Name` or `@Name(...)` (`@` already consumed).
    fn skip_annotation(&mut self) -> Option<()> {
        self.dotted()?;
        if self.eat_punct('(') {
            let mut depth = 1;
            while depth > 0 {
                match self.bump() {
                    Some(Tok::Punct('(')) => depth += 1,
                    Some(Tok::Punct(')')) => depth -= 1,
                    Some(_) => {}
                    None => return None,
                }
            }
        }
        Some(())
    }
    fn skip_default_value(&mut self) -> Option<()> {
        if self.eat_punct('{') {
            self.skip_braces();
            return Some(());
        }
        if self.eat_punct('@') {
            return self.skip_annotation();
        }
        let mut depth = 0i32;
        loop {
            match self.peek() {
                Some(Tok::Punct(';')) if depth == 0 => return Some(()),
                Some(Tok::Punct('(' | '[')) => {
                    depth += 1;
                    self.i += 1;
                }
                Some(Tok::Punct(')' | ']')) => {
                    depth -= 1;
                    self.i += 1;
                }
                Some(_) => self.i += 1,
                None => return None,
            }
        }
    }
}

const MODIFIERS: &[&str] = &[
    "public",
    "protected",
    "private",
    "static",
    "final",
    "abstract",
    "strictfp",
    "native",
    "synchronized",
    "transient",
    "volatile",
    "default",
    "sealed",
    "non",
];

/// Collect modifiers + annotation uses, returning the access bits we model (plus the internal
/// [`STUB_DEFAULT`] marker for `default`).
fn modifiers(p: &mut P) -> Option<u16> {
    let mut acc = 0u16;
    loop {
        if p.peek() == Some(&Tok::Punct('@')) {
            // `@interface` is a declaration kind, not an annotation use — leave it to the caller.
            if matches!(p.t.get(p.i + 1), Some(Tok::Ident(s)) if s == "interface") {
                return Some(acc);
            }
            p.i += 1;
            p.skip_annotation()?;
            continue;
        }
        match p.peek() {
            Some(Tok::Ident(s)) if MODIFIERS.contains(&s.as_str()) => {
                match s.as_str() {
                    "public" => acc |= ACC_PUBLIC,
                    "protected" => acc |= ACC_PROTECTED,
                    "private" => acc |= ACC_PRIVATE,
                    "static" => acc |= ACC_STATIC,
                    "final" => acc |= ACC_FINAL,
                    "abstract" => acc |= ACC_ABSTRACT,
                    "default" => acc |= STUB_DEFAULT,
                    // `non-sealed` arrives as `non`, `-`, `sealed`; eat the tail.
                    "non" => {
                        p.i += 1;
                        p.eat_punct('-').then_some(())?;
                        p.eat_ident("sealed").then_some(())?;
                        continue;
                    }
                    _ => {}
                }
                p.i += 1;
            }
            _ => return Some(acc),
        }
    }
}

/// `<E extends A & B, F>` — type-parameter list (leading `<` already consumed). Erasure uses the
/// FIRST bound; extra `& Bound`s are validated but dropped.
fn tparam_list(p: &mut P) -> Option<Vec<(String, Option<SrcType>)>> {
    let mut out = Vec::new();
    loop {
        let name = p.ident()?;
        let mut bound = None;
        if p.eat_ident("extends") {
            bound = Some(src_type(p)?);
            while p.eat_punct('&') {
                let _ = src_type(p)?;
            }
        }
        out.push((name, bound));
        if p.eat_punct(',') {
            continue;
        }
        if p.eat_punct('>') {
            return Some(out);
        }
        return None;
    }
}

/// A source type: `int`, `java.util.List<String>[]`, `E`, `Map.Entry<K,V>`, `?`, `? extends X`.
fn src_type(p: &mut P) -> Option<SrcType> {
    skip_type_annotations(p)?;
    if p.eat_punct('?') {
        // A wildcard is modeled as its bound (or Object) — sound for a stub's erasure/signature.
        if p.eat_ident("extends") || p.eat_ident("super") {
            return src_type(p);
        }
        return Some(SrcType {
            name: "java.lang.Object".into(),
            args: Vec::new(),
            array: 0,
            span: None,
        });
    }
    let (name, span) = p.dotted_spanned()?;
    let mut args = Vec::new();
    if p.eat_punct('<') && !p.eat_punct('>') {
        loop {
            args.push(src_type(p)?);
            if p.eat_punct(',') {
                continue;
            }
            if p.eat_punct('>') {
                break;
            }
            return None;
        }
    }
    let mut array = 0;
    loop {
        skip_type_annotations(p)?;
        if !p.eat_punct('[') {
            break;
        }
        if !p.eat_punct(']') {
            return None;
        }
        array += 1;
    }
    Some(SrcType {
        name,
        args,
        array,
        span: Some(span),
    })
}

fn skip_type_annotations(p: &mut P) -> Option<()> {
    while p.eat_punct('@') {
        p.skip_annotation()?;
    }
    Some(())
}

/// Parse one file: package/imports, then top-level type declarations.
pub(crate) fn parse_raw_file(toks: &LexedJava) -> Option<(FileCtx, Vec<RawDecl>, Vec<SrcType>)> {
    let mut p = P {
        t: &toks.tokens,
        spans: &toks.spans,
        i: 0,
        body_refs: Vec::new(),
    };
    let mut ctx = FileCtx {
        package: String::new(),
        imports: Vec::new(),
    };
    let mut decls = Vec::new();
    while let Some(tok) = p.peek() {
        match tok {
            Tok::Ident(s) if s == "package" => {
                p.i += 1;
                ctx.package = p.dotted()?.replace('.', "/");
                p.eat_punct(';').then_some(())?;
            }
            Tok::Ident(s) if s == "import" => {
                p.i += 1;
                let is_static = p.eat_ident("static");
                let (path, span) = p.dotted_spanned()?;
                let wildcard = if p.eat_punct('.') {
                    p.eat_punct('*').then_some(())?;
                    true
                } else {
                    false
                };
                p.eat_punct(';').then_some(())?;
                ctx.imports.push(JavaImport {
                    path,
                    wildcard,
                    is_static,
                    span,
                });
            }
            _ => {
                type_decl(&mut p, &ctx.package, None, &mut decls)?;
            }
        }
    }
    Some((ctx, decls, p.body_refs))
}

fn type_decl(p: &mut P, package: &str, outer: Option<&str>, out: &mut Vec<RawDecl>) -> Option<()> {
    let acc = modifiers(p)?;
    type_decl_with_access(p, package, outer, out, acc)
}

fn type_decl_with_access(
    p: &mut P,
    package: &str,
    outer: Option<&str>,
    out: &mut Vec<RawDecl>,
    acc: u16,
) -> Option<()> {
    if p.peek() == Some(&Tok::Punct('@')) {
        p.i += 1;
        p.eat_ident("interface").then_some(())?;
        return annotation_type_decl(p, package, outer, out, acc);
    }
    let kind = if p.eat_ident("class") {
        DeclKind::Class
    } else if p.eat_ident("interface") {
        DeclKind::Interface
    } else if p.eat_ident("enum") {
        DeclKind::Enum
    } else if p.eat_ident("record") {
        DeclKind::Record
    } else {
        return None;
    };
    let is_interface = matches!(kind, DeclKind::Interface | DeclKind::Annotation);
    let name_span = p.span()?;
    let simple = p.ident()?;
    let internal = match outer {
        Some(o) => format!("{o}${simple}"),
        None if package.is_empty() => simple.clone(),
        None => format!("{package}/{simple}"),
    };
    let tparams = if p.eat_punct('<') {
        tparam_list(p)?
    } else {
        Vec::new()
    };
    let (record_components, record_is_varargs) = if kind == DeclKind::Record {
        p.eat_punct('(').then_some(())?;
        record_component_list(p)?
    } else {
        (Vec::new(), false)
    };
    let mut superclass = None;
    let mut interfaces = Vec::new();
    let mut permits = Vec::new();
    if p.eat_ident("extends") {
        if is_interface {
            loop {
                interfaces.push(src_type(p)?);
                if !p.eat_punct(',') {
                    break;
                }
            }
        } else {
            superclass = Some(src_type(p)?);
        }
    }
    if p.eat_ident("implements") {
        loop {
            interfaces.push(src_type(p)?);
            if !p.eat_punct(',') {
                break;
            }
        }
    }
    if p.eat_ident("permits") {
        loop {
            permits.push(src_type(p)?);
            if !p.eat_punct(',') {
                break;
            }
        }
    }
    p.eat_punct('{').then_some(())?;

    let mut decl = RawDecl {
        internal: internal.clone(),
        outer_internal: outer.map(str::to_string),
        simple_name: simple.clone(),
        name_span,
        access: acc,
        kind,
        is_abstract: acc & ACC_ABSTRACT != 0,
        tparams,
        superclass,
        interfaces,
        permits,
        ctors: Vec::new(),
        methods: Vec::new(),
        fields: Vec::new(),
        enum_constants: Vec::new(),
        enum_has_constant_body: false,
        record_components,
        record_is_varargs,
    };

    if kind == DeclKind::Enum {
        loop {
            if p.eat_punct(';') {
                break;
            }
            if p.peek() == Some(&Tok::Punct('}')) {
                break;
            }
            skip_type_annotations(p)?;
            let cname = p.ident()?;
            if p.eat_punct('(') {
                let mut d = 1;
                while d > 0 {
                    match p.bump()? {
                        Tok::Punct('(') => d += 1,
                        Tok::Punct(')') => d -= 1,
                        _ => {}
                    }
                }
            }
            if p.eat_punct('{') {
                decl.enum_has_constant_body = true;
                p.skip_braces();
            }
            decl.enum_constants.push(cname);
            if p.eat_punct(',') {
                continue;
            }
            if p.eat_punct(';') {
                break;
            }
            if p.peek() == Some(&Tok::Punct('}')) {
                break;
            }
            return None;
        }
    }

    // Members until the closing `}`.
    loop {
        if p.eat_punct('}') {
            break;
        }
        if p.eat_punct(';') {
            continue;
        }
        let macc = modifiers(p)?;
        if matches!(p.peek(), Some(Tok::Ident(s)) if s == "class" || s == "interface" || s == "enum" || s == "record")
            || (p.peek() == Some(&Tok::Punct('@'))
                && matches!(p.t.get(p.i + 1), Some(Tok::Ident(s)) if s == "interface"))
        {
            // A type nested in an interface/annotation is implicitly public and static (JLS
            // §9.5), like interface methods and fields.
            let nested_access = if matches!(kind, DeclKind::Interface | DeclKind::Annotation) {
                macc | ACC_PUBLIC | ACC_STATIC
            } else {
                macc
            };
            type_decl_with_access(p, package, Some(&internal), out, nested_access)?;
            continue;
        }
        // Initializer block: `static { … }` (its `static` was eaten by `modifiers`) or `{ … }`.
        if p.eat_punct('{') {
            p.skip_body();
            continue;
        }
        if kind == DeclKind::Record
            && matches!(p.peek(), Some(Tok::Ident(s)) if *s == simple)
            && p.t.get(p.i + 1) == Some(&Tok::Punct('{'))
        {
            p.i += 1;
            p.eat_punct('{').then_some(())?;
            p.skip_body();
            continue;
        }
        // Method-level type params.
        let mtparams = if p.eat_punct('<') {
            tparam_list(p)?
        } else {
            Vec::new()
        };
        // Constructor: `Simple (` with no return type.
        if matches!(p.peek(), Some(Tok::Ident(s)) if *s == simple)
            && p.t.get(p.i + 1) == Some(&Tok::Punct('('))
        {
            p.i += 1;
            p.eat_punct('(').then_some(())?;
            let (params, varargs) = param_list(p)?;
            let throws = skip_throws_and_body(p)?;
            decl.ctors.push(Member {
                name: "<init>".into(),
                tparams: mtparams,
                params,
                ret: None,
                throws,
                access: (macc & (ACC_PUBLIC | ACC_PROTECTED | ACC_PRIVATE))
                    | if varargs { ACC_VARARGS } else { 0 },
            });
            continue;
        }
        // Field or method: `Type name (` → method; `Type name [;=,]` → field.
        let ty = src_type(p)?;
        let name = p.ident()?;
        if p.eat_punct('(') {
            let (params, varargs) = param_list(p)?;
            let throws = skip_throws_and_body(p)?;
            decl.methods.push(Member {
                name,
                tparams: mtparams,
                params,
                ret: Some(ty),
                throws,
                access: macc | if varargs { ACC_VARARGS } else { 0 },
            });
        } else {
            // Field, possibly a list (`int a, b = 1;`); initializers are skipped balancedly.
            decl.fields.push((name, ty.clone(), macc));
            loop {
                if p.eat_punct(',') {
                    let n = p.ident()?;
                    decl.fields.push((n, ty.clone(), macc));
                    continue;
                }
                if p.eat_punct(';') {
                    break;
                }
                match p.bump()? {
                    Tok::Punct('{') => p.skip_braces(),
                    Tok::Punct('(') => {
                        let mut d = 1;
                        while d > 0 {
                            match p.bump()? {
                                Tok::Punct('(') => d += 1,
                                Tok::Punct(')') => d -= 1,
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    if kind == DeclKind::Enum
        && decl
            .methods
            .iter()
            .any(|method| method.access & ACC_ABSTRACT != 0)
    {
        decl.is_abstract = true;
    }
    out.push(decl);
    Some(())
}

fn annotation_type_decl(
    p: &mut P,
    package: &str,
    outer: Option<&str>,
    out: &mut Vec<RawDecl>,
    acc: u16,
) -> Option<()> {
    let name_span = p.span()?;
    let simple = p.ident()?;
    let internal = match outer {
        Some(o) => format!("{o}${simple}"),
        None if package.is_empty() => simple.clone(),
        None => format!("{package}/{simple}"),
    };
    p.eat_punct('{').then_some(())?;

    let mut decl = RawDecl {
        internal: internal.clone(),
        outer_internal: outer.map(str::to_string),
        simple_name: simple,
        name_span,
        access: acc,
        kind: DeclKind::Annotation,
        is_abstract: acc & ACC_ABSTRACT != 0,
        tparams: Vec::new(),
        superclass: None,
        interfaces: Vec::new(),
        permits: Vec::new(),
        ctors: Vec::new(),
        methods: Vec::new(),
        fields: Vec::new(),
        enum_constants: Vec::new(),
        enum_has_constant_body: false,
        record_components: Vec::new(),
        record_is_varargs: false,
    };

    loop {
        if p.eat_punct('}') {
            break;
        }
        if p.eat_punct(';') {
            continue;
        }
        let macc = modifiers(p)?;
        if matches!(p.peek(), Some(Tok::Ident(s)) if s == "class" || s == "interface" || s == "enum" || s == "record")
            || (p.peek() == Some(&Tok::Punct('@'))
                && matches!(p.t.get(p.i + 1), Some(Tok::Ident(s)) if s == "interface"))
        {
            // Nested types of an annotation type are implicitly public and static (JLS §9.5).
            type_decl_with_access(
                p,
                package,
                Some(&internal),
                out,
                macc | ACC_PUBLIC | ACC_STATIC,
            )?;
            continue;
        }
        let ty = src_type(p)?;
        let name = p.ident()?;
        p.eat_punct('(').then_some(())?;
        p.eat_punct(')').then_some(())?;
        if p.eat_ident("default") {
            p.skip_default_value()?;
        }
        p.eat_punct(';').then_some(())?;
        decl.methods.push(Member {
            name,
            tparams: Vec::new(),
            params: Vec::new(),
            ret: Some(ty),
            throws: Vec::new(),
            access: macc,
        });
    }
    out.push(decl);
    Some(())
}

/// Shared `modifiers Type [... ] name` prefix for method parameters and record components. A vararg
/// spelling maps to its JVM array type here; list-level callers enforce that it is the final element.
fn typed_parameter(p: &mut P) -> Option<(String, SrcType, bool)> {
    // Method parameters and record components share this exact Java grammar prefix. Parse it once so
    // accepting an annotation/modifier, recognizing `...`, and converting that spelling to its JVM
    // array type cannot drift between the explicit-method and synthesized-record constructor paths.
    let _ = modifiers(p)?;
    let mut ty = src_type(p)?;
    let is_varargs = if p.eat_punct('.') {
        p.eat_punct('.').then_some(())?;
        p.eat_punct('.').then_some(())?;
        ty.array += 1;
        true
    } else {
        false
    };
    let name = p.ident()?;
    Some((name, ty, is_varargs))
}

/// `( Type name, Type... name )` — parameter list (opening paren consumed). Varargs `...` maps to
/// an array, exactly as javac compiles it.
fn param_list(p: &mut P) -> Option<(Vec<SrcType>, bool)> {
    let mut out = Vec::new();
    if p.eat_punct(')') {
        return Some((out, false));
    }
    loop {
        let (_name, mut ty, parameter_is_varargs) = typed_parameter(p)?;
        if parameter_is_varargs {
            // Java permits variable arity only in the final parameter. Requiring the closing delimiter
            // here rejects malformed declarations instead of retaining ACC_VARARGS on an earlier array.
            out.push(ty);
            p.eat_punct(')').then_some(())?;
            return Some((out, true));
        }
        // C-style array suffix on the NAME (`int a[]`).
        while p.eat_punct('[') {
            p.eat_punct(']').then_some(())?;
            ty.array += 1;
        }
        out.push(ty);
        if p.eat_punct(',') {
            continue;
        }
        p.eat_punct(')').then_some(())?;
        return Some((out, false));
    }
}

fn record_component_list(p: &mut P) -> Option<(Vec<(String, SrcType)>, bool)> {
    let mut out = Vec::new();
    if p.eat_punct(')') {
        return Some((out, false));
    }
    loop {
        let (name, ty, component_is_varargs) = typed_parameter(p)?;
        out.push((name, ty));
        if component_is_varargs {
            // The variable-arity record component defines the canonical constructor ABI and, like an
            // ordinary method vararg, must be last. `)` is therefore part of recognizing this form.
            p.eat_punct(')').then_some(())?;
            return Some((out, true));
        }
        if p.eat_punct(',') {
            continue;
        }
        p.eat_punct(')').then_some(())?;
        return Some((out, false));
    }
}

/// After a method/ctor parameter list: optional `throws A, B`, then `{ body }` or `;`.
fn skip_throws_and_body(p: &mut P) -> Option<Vec<SrcType>> {
    let mut throws = Vec::new();
    if p.eat_ident("throws") {
        loop {
            throws.push(src_type(p)?);
            if !p.eat_punct(',') {
                break;
            }
        }
    }
    if p.eat_punct('{') {
        p.skip_body();
        return Some(throws);
    }
    p.eat_punct(';').then_some(throws)
}

fn explicit_body_type_refs(tokens: &[Tok], spans: &[Span]) -> Vec<SrcType> {
    let mut p = P {
        t: tokens,
        spans,
        i: 0,
        body_refs: Vec::new(),
    };
    let mut refs = Vec::new();
    while p.i < p.t.len() {
        if p.eat_ident("new") || p.eat_ident("instanceof") {
            if let Some(reference) = src_type(&mut p) {
                refs.push(reference);
            }
            continue;
        }
        if p.eat_ident("catch") && p.eat_punct('(') {
            if let Some(reference) = src_type(&mut p) {
                refs.push(reference);
            }
            continue;
        }
        p.i += 1;
    }
    refs
}

// --- Resolution + emission -------------------------------------------------

fn classifier_path(name: &str, exists: &dyn Fn(&str) -> bool) -> Option<String> {
    let segments = name
        .split(['.', '/'])
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    for classifier_end in 1..=segments.len() {
        let classifier = segments[..classifier_end].join("/");
        if !exists(&classifier) {
            continue;
        }
        let mut resolved = classifier;
        for nested in &segments[classifier_end..] {
            resolved.push('$');
            resolved.push_str(nested);
            if !exists(&resolved) {
                return None;
            }
        }
        return Some(resolved);
    }
    None
}

fn existing_candidates(
    candidates: impl IntoIterator<Item = String>,
    exists: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut matches = candidates
        .into_iter()
        .filter_map(|candidate| classifier_path(&candidate, exists))
        .collect::<Vec<_>>();
    matches.sort_unstable();
    matches.dedup();
    matches
}

pub(crate) fn resolve_internal_name(
    package: &str,
    imports: &[JavaImport],
    name: &str,
    exists: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let (head, tail) = name
        .split_once('.')
        .map_or((name, None), |(head, tail)| (head, Some(tail)));
    let explicit = imports
        .iter()
        .filter(|import| {
            !import.is_static && !import.wildcard && import.path.rsplit('.').next() == Some(head)
        })
        .map(|import| match tail {
            Some(tail) => format!("{}.{}", import.path, tail),
            None => import.path.clone(),
        })
        .collect::<Vec<_>>();
    if !explicit.is_empty() {
        let mut matches = existing_candidates(explicit, exists);
        return (matches.len() == 1).then(|| matches.pop().unwrap());
    }

    if !package.is_empty() {
        let candidate = format!("{package}/{name}");
        if let Some(candidate) = classifier_path(&candidate, exists) {
            return Some(candidate);
        }
    }

    let wildcard = imports
        .iter()
        .filter(|import| !import.is_static && import.wildcard)
        .map(|import| format!("{}/{name}", import.path));
    let mut wildcard_matches = existing_candidates(wildcard, exists);
    match wildcard_matches.len() {
        1 => return wildcard_matches.pop(),
        2.. => return None,
        _ => {}
    }

    if let Some(candidate) = classifier_path(name, exists) {
        return Some(candidate);
    }
    classifier_path(&format!("java/lang/{name}"), exists)
}

pub(crate) fn primitive_desc(name: &str) -> Option<&'static str> {
    Some(match name {
        "void" => "V",
        "boolean" => "Z",
        "byte" => "B",
        "short" => "S",
        "char" => "C",
        "int" => "I",
        "long" => "J",
        "float" => "F",
        "double" => "D",
        _ => return None,
    })
}

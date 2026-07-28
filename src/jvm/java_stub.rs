//! Java **signature stubs** — slice 2 of Java-source interop (`docs/JAVA_INTEROP.md`).
//!
//! Kotlin-first mixed compilation needs Java *signatures* before javac can run (the Java may
//! reference Kotlin declarations, so javac must come AFTER krusty). This module parses the
//! signature surface of a Java source — package, imports, type declarations, extends/implements,
//! member signatures; never bodies — and emits **stub `.class` files** carrying exactly what
//! krusty's classreader consumes: names, descriptors, access flags, and generic `Signature`
//! attributes. The stubs sit on krusty's compile classpath and are then DISCARDED: javac compiles
//! the real Java against krusty's output, and only javac's classes ship. A stub is never loaded by
//! a JVM, so concrete method bodies are a 2-byte `aconst_null; athrow`.
//!
//! Name resolution is delegated to the caller through a `resolve` callback (candidate internal
//! name → exists?), so the parser holds NO class lists: candidates are the explicit imports, the
//! file's own package, wildcard imports, the root package, and `java.lang` (the language-mandated
//! implicit import) — checked against the caller's world (Kotlin module symbols + classpath).
//! An unresolvable reference type aborts stub generation (`None`): a guessed supertype or
//! parameter type would MIS-COMPILE the Kotlin side, and the callers' contract is skip-not-wrong.
//!
//! The modeled subset includes classes, interfaces, enums, records, annotations, generic
//! signatures, fields, constructors, and methods.

use super::classfile::{
    ClassWriter, CodeBuilder, ACC_ABSTRACT, ACC_ANNOTATION, ACC_ENUM, ACC_FINAL, ACC_INTERFACE,
    ACC_PRIVATE, ACC_PROTECTED, ACC_PUBLIC, ACC_STATIC, ACC_SUPER,
};
use std::collections::{HashMap, HashSet};

/// Internal-only marker bit for a `default` interface method (cleared before emission — it shares
/// no bit with a real JVM class-file flag we emit).
const STUB_DEFAULT: u16 = 0x8000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StubMode {
    Strict,
    Lenient,
}

impl StubMode {
    fn is_lenient(self) -> bool {
        self == Self::Lenient
    }
}

/// Generate stub classes for Java sources.
pub fn stub_classes(
    sources: &[(String, String)],
    mode: StubMode,
    resolve: &dyn Fn(&str) -> bool,
) -> Option<Vec<(String, Vec<u8>)>> {
    // Two passes: collect every declared type's internal name first, so same-compilation Java
    // types resolve against each other regardless of file order.
    let mut parsed: Vec<(FileCtx, Vec<RawDecl>)> = Vec::new();
    let mut declared: Vec<String> = Vec::new();
    for (_, src) in sources {
        let toks = lex_java(src);
        let (ctx, decls) = match parse_file(&toks) {
            Some(parsed) => parsed,
            None if mode.is_lenient() => continue,
            None => return None,
        };
        for d in &decls {
            declared.push(d.internal.clone());
        }
        parsed.push((ctx, decls));
    }
    let mut declaration_counts = HashMap::new();
    for internal in &declared {
        *declaration_counts
            .entry(internal.as_str())
            .or_insert(0usize) += 1;
    }
    let emittable_declarations = parsed
        .iter()
        .flat_map(|(_, declarations)| declarations)
        .filter(|declaration| {
            declaration_counts.get(declaration.internal.as_str()) == Some(&1)
                && (!declaration.internal.contains('$') || declaration.access & ACC_PRIVATE == 0)
        })
        .map(|declaration| declaration.internal.as_str())
        .collect::<HashSet<_>>();
    let resolve_all = |cand: &str| emittable_declarations.contains(cand) || resolve(cand);

    let mut out = Vec::new();
    for (ctx, decls) in &parsed {
        let r = Resolver {
            ctx,
            resolve: &resolve_all,
            mode,
        };
        for raw in decls {
            if declaration_counts.get(raw.internal.as_str()) != Some(&1)
                || (raw.internal.contains('$') && raw.access & ACC_PRIVATE != 0)
            {
                continue;
            }
            match r.emit(raw) {
                Some(bytes) => out.push((raw.internal.clone(), bytes)),
                None if mode.is_lenient() => continue,
                None => return None,
            }
        }
    }
    Some(out)
}

// --- Tokenizer -------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Punct(char),
}

/// Tokenize Java source into identifiers and single-char punctuation. Comments and string/char
/// literal contents are dropped (literals only ever appear inside bodies, which are skipped).
fn lex_java(src: &str) -> Vec<Tok> {
    let b: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(b.len());
        } else if c == '"' || c == '\'' {
            let quote = c;
            i += 1;
            while i < b.len() && b[i] != quote {
                i += if b[i] == '\\' { 2 } else { 1 };
            }
            i += 1;
        } else if c.is_alphanumeric() || c == '_' || c == '$' {
            let start = i;
            while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_' || b[i] == '$') {
                i += 1;
            }
            out.push(Tok::Ident(b[start..i].iter().collect()));
        } else {
            out.push(Tok::Punct(c));
            i += 1;
        }
    }
    out
}

// --- Parsed shapes ----------------------------------------------------------

/// Per-file context: package (internal form, `""` = root) and imports.
struct FileCtx {
    package: String,
    /// Explicit imports: simple name → internal name.
    imports: Vec<(String, String)>,
    /// Wildcard import packages (internal form).
    wildcards: Vec<String>,
}

/// A source-level type reference: base name (dotted as written), generic args, array depth.
#[derive(Clone, Debug)]
struct SrcType {
    name: String,
    args: Vec<SrcType>,
    array: u32,
}

/// A member signature: name, params, return (`None` for a constructor), flags, own type params.
struct Member {
    name: String,
    tparams: Vec<(String, Option<SrcType>)>,
    params: Vec<SrcType>,
    ret: Option<SrcType>,
    access: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeclKind {
    Class,
    Interface,
    Enum,
    Record,
    Annotation,
}

/// A parsed type declaration with unresolved source types.
struct RawDecl {
    /// Internal name (`pkg/Outer$Inner`).
    internal: String,
    access: u16,
    kind: DeclKind,
    is_abstract: bool,
    tparams: Vec<(String, Option<SrcType>)>,
    /// `extends` for a class (`None` = `java/lang/Object`); an interface's `extends` list is in
    /// `interfaces`.
    superclass: Option<SrcType>,
    interfaces: Vec<SrcType>,
    ctors: Vec<Member>,
    methods: Vec<Member>,
    fields: Vec<(String, SrcType, u16)>,
    enum_constants: Vec<String>,
    record_components: Vec<(String, SrcType)>,
}

impl RawDecl {
    fn is_interface(&self) -> bool {
        matches!(self.kind, DeclKind::Interface | DeclKind::Annotation)
    }
}

// --- Parser ----------------------------------------------------------------

struct P<'a> {
    t: &'a [Tok],
    i: usize,
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
    /// Dotted name `a.b.C` as written. A `.` is consumed only when an identifier FOLLOWS it, so a
    /// trailing `...` (varargs) or `.*` (wildcard import) is left for the caller.
    fn dotted(&mut self) -> Option<String> {
        let mut s = self.ident()?;
        while self.peek() == Some(&Tok::Punct('.'))
            && matches!(self.t.get(self.i + 1), Some(Tok::Ident(_)))
        {
            self.i += 1;
            s.push('.');
            s.push_str(&self.ident()?);
        }
        Some(s)
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
    if p.eat_punct('?') {
        // A wildcard is modeled as its bound (or Object) — sound for a stub's erasure/signature.
        if p.eat_ident("extends") || p.eat_ident("super") {
            return src_type(p);
        }
        return Some(SrcType {
            name: "java.lang.Object".into(),
            args: Vec::new(),
            array: 0,
        });
    }
    let name = p.dotted()?;
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
    while p.eat_punct('[') {
        if !p.eat_punct(']') {
            return None;
        }
        array += 1;
    }
    Some(SrcType { name, args, array })
}

/// Parse one file: package/imports, then top-level type declarations.
fn parse_file(toks: &[Tok]) -> Option<(FileCtx, Vec<RawDecl>)> {
    let mut p = P { t: toks, i: 0 };
    let mut ctx = FileCtx {
        package: String::new(),
        imports: Vec::new(),
        wildcards: Vec::new(),
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
                if p.eat_ident("static") {
                    let _ = p.dotted()?;
                    let _ = p.eat_punct('.');
                    let _ = p.eat_punct('*');
                    p.eat_punct(';').then_some(())?;
                    continue;
                }
                let path = p.dotted()?;
                if p.eat_punct('.') {
                    p.eat_punct('*').then_some(())?;
                    ctx.wildcards.push(path.replace('.', "/"));
                } else {
                    let simple = path.rsplit('.').next()?.to_string();
                    ctx.imports.push((simple, path.replace('.', "/")));
                }
                p.eat_punct(';').then_some(())?;
            }
            _ => {
                type_decl(&mut p, &ctx.package, None, &mut decls)?;
            }
        }
    }
    Some((ctx, decls))
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
    let record_components = if kind == DeclKind::Record {
        p.eat_punct('(').then_some(())?;
        record_component_list(p)?
    } else {
        Vec::new()
    };
    let mut superclass = None;
    let mut interfaces = Vec::new();
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
            let _ = src_type(p)?;
            if !p.eat_punct(',') {
                break;
            }
        }
    }
    p.eat_punct('{').then_some(())?;

    let mut decl = RawDecl {
        internal: internal.clone(),
        access: acc,
        kind,
        is_abstract: acc & ACC_ABSTRACT != 0,
        tparams,
        superclass,
        interfaces,
        ctors: Vec::new(),
        methods: Vec::new(),
        fields: Vec::new(),
        enum_constants: Vec::new(),
        record_components,
    };

    if kind == DeclKind::Enum {
        loop {
            if p.eat_punct(';') {
                break;
            }
            if p.peek() == Some(&Tok::Punct('}')) {
                break;
            }
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
            type_decl_with_access(p, package, Some(&internal), out, macc)?;
            continue;
        }
        // Initializer block: `static { … }` (its `static` was eaten by `modifiers`) or `{ … }`.
        if p.eat_punct('{') {
            p.skip_braces();
            continue;
        }
        if kind == DeclKind::Record
            && matches!(p.peek(), Some(Tok::Ident(s)) if *s == simple)
            && p.t.get(p.i + 1) == Some(&Tok::Punct('{'))
        {
            p.i += 1;
            p.eat_punct('{').then_some(())?;
            p.skip_braces();
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
            let params = param_list(p)?;
            skip_throws_and_body(p)?;
            decl.ctors.push(Member {
                name: "<init>".into(),
                tparams: mtparams,
                params,
                ret: None,
                access: macc & (ACC_PUBLIC | ACC_PROTECTED | ACC_PRIVATE),
            });
            continue;
        }
        // Field or method: `Type name (` → method; `Type name [;=,]` → field.
        let ty = src_type(p)?;
        let name = p.ident()?;
        if p.eat_punct('(') {
            let params = param_list(p)?;
            skip_throws_and_body(p)?;
            decl.methods.push(Member {
                name,
                tparams: mtparams,
                params,
                ret: Some(ty),
                access: macc,
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
    let simple = p.ident()?;
    let internal = match outer {
        Some(o) => format!("{o}${simple}"),
        None if package.is_empty() => simple.clone(),
        None => format!("{package}/{simple}"),
    };
    p.eat_punct('{').then_some(())?;

    let mut decl = RawDecl {
        internal: internal.clone(),
        access: acc,
        kind: DeclKind::Annotation,
        is_abstract: acc & ACC_ABSTRACT != 0,
        tparams: Vec::new(),
        superclass: None,
        interfaces: Vec::new(),
        ctors: Vec::new(),
        methods: Vec::new(),
        fields: Vec::new(),
        enum_constants: Vec::new(),
        record_components: Vec::new(),
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
            type_decl_with_access(p, package, Some(&internal), out, macc)?;
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
            access: macc,
        });
    }
    out.push(decl);
    Some(())
}

/// `( Type name, Type... name )` — parameter list (opening paren consumed). Varargs `...` maps to
/// an array, exactly as javac compiles it.
fn param_list(p: &mut P) -> Option<Vec<SrcType>> {
    let mut out = Vec::new();
    if p.eat_punct(')') {
        return Some(out);
    }
    loop {
        let _ = modifiers(p)?; // `final`, annotations
        let mut ty = src_type(p)?;
        if p.eat_punct('.') {
            p.eat_punct('.').then_some(())?;
            p.eat_punct('.').then_some(())?;
            ty.array += 1;
        }
        let _name = p.ident()?;
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
        return Some(out);
    }
}

fn record_component_list(p: &mut P) -> Option<Vec<(String, SrcType)>> {
    let mut out = Vec::new();
    if p.eat_punct(')') {
        return Some(out);
    }
    loop {
        let _ = modifiers(p)?;
        let mut ty = src_type(p)?;
        if p.eat_punct('.') {
            p.eat_punct('.').then_some(())?;
            p.eat_punct('.').then_some(())?;
            ty.array += 1;
        }
        let name = p.ident()?;
        out.push((name, ty));
        if p.eat_punct(',') {
            continue;
        }
        p.eat_punct(')').then_some(())?;
        return Some(out);
    }
}

/// After a method/ctor parameter list: optional `throws A, B`, then `{ body }` or `;`.
fn skip_throws_and_body(p: &mut P) -> Option<()> {
    if p.eat_ident("throws") {
        loop {
            let _ = src_type(p)?;
            if !p.eat_punct(',') {
                break;
            }
        }
    }
    if p.eat_punct('{') {
        p.skip_braces();
        return Some(());
    }
    p.eat_punct(';').then_some(())
}

// --- Resolution + emission -------------------------------------------------

struct Resolver<'a> {
    ctx: &'a FileCtx,
    resolve: &'a dyn Fn(&str) -> bool,
    mode: StubMode,
}

impl Resolver<'_> {
    /// The internal name a source type name resolves to, or `None`. Candidate order mirrors the
    /// Java language: explicit import, own package, wildcard imports, root package, `java.lang`.
    fn internal_of(&self, name: &str) -> Option<String> {
        if name.contains('.') {
            // Fully-qualified as written, or a nested `Outer.Inner` — convert `/`→`$` from the
            // right until the candidate exists (krusty's own nested-import recovery).
            let mut cand = name.replace('.', "/");
            loop {
                if (self.resolve)(&cand) {
                    return Some(cand);
                }
                match cand.rfind('/') {
                    Some(i) => cand.replace_range(i..=i, "$"),
                    None => return None,
                }
            }
        }
        if let Some((_, full)) = self.ctx.imports.iter().find(|(s, _)| s == name) {
            return Some(full.clone());
        }
        let mut cands: Vec<String> = Vec::new();
        if self.ctx.package.is_empty() {
            cands.push(name.to_string());
        } else {
            cands.push(format!("{}/{name}", self.ctx.package));
        }
        for w in &self.ctx.wildcards {
            cands.push(format!("{w}/{name}"));
        }
        cands.push(name.to_string());
        cands.push(format!("java/lang/{name}"));
        cands.into_iter().find(|c| (self.resolve)(c))
    }

    /// Erased JVM descriptor of a source type. `None` if a reference type doesn't resolve.
    fn desc(&self, t: &SrcType, tparams: &[&str]) -> Option<String> {
        let mut s = "[".repeat(t.array as usize);
        if let Some(p) = primitive_desc(&t.name) {
            s.push_str(p);
        } else if tparams.contains(&t.name.as_str()) {
            s.push_str("Ljava/lang/Object;");
        } else {
            match self.internal_of(&t.name) {
                Some(i) => {
                    s.push('L');
                    s.push_str(&i);
                    s.push(';');
                }
                None if self.mode.is_lenient() => s.push_str("Ljava/lang/Object;"),
                None => return None,
            }
        }
        for a in &t.args {
            self.desc(a, tparams)?;
        }
        Some(s)
    }

    /// JVM generic-`Signature` form of a source type (`LA<TE;>;`, `TE;`, `I`).
    fn sig(&self, t: &SrcType, tparams: &[&str]) -> Option<String> {
        let mut s = "[".repeat(t.array as usize);
        if let Some(p) = primitive_desc(&t.name) {
            s.push_str(p);
            return Some(s);
        }
        if tparams.contains(&t.name.as_str()) {
            s.push('T');
            s.push_str(&t.name);
            s.push(';');
            return Some(s);
        }
        s.push('L');
        s.push_str(&self.internal_of(&t.name)?);
        if !t.args.is_empty() {
            s.push('<');
            for a in &t.args {
                s.push_str(&self.sig(a, tparams)?);
            }
            s.push('>');
        }
        s.push(';');
        Some(s)
    }

    fn internal_or_object(&self, name: &str) -> Option<String> {
        match self.internal_of(name) {
            Some(i) => Some(i),
            None if self.mode.is_lenient() => Some("java/lang/Object".to_string()),
            None => None,
        }
    }

    /// `<E:Bound;F:Ljava/lang/Object;>` — the type-parameter block of a `Signature` attribute.
    fn tparam_block(
        &self,
        tparams: &[(String, Option<SrcType>)],
        scope: &[&str],
    ) -> Option<String> {
        if tparams.is_empty() {
            return Some(String::new());
        }
        let mut s = String::from("<");
        for (name, bound) in tparams {
            s.push_str(name);
            s.push(':');
            match bound {
                Some(b) => s.push_str(&self.sig(b, scope)?),
                None => s.push_str("Ljava/lang/Object;"),
            }
        }
        s.push('>');
        Some(s)
    }

    fn build_class_sig(&self, d: &RawDecl, tp: &[&str], default_super: &str) -> Option<String> {
        let mut sig = self.tparam_block(&d.tparams, tp)?;
        match &d.superclass {
            Some(t) => sig.push_str(&self.sig(t, tp)?),
            None => {
                sig.push('L');
                sig.push_str(default_super);
                sig.push(';');
            }
        }
        for i in &d.interfaces {
            sig.push_str(&self.sig(i, tp)?);
        }
        Some(sig)
    }

    fn emit(&self, d: &RawDecl) -> Option<Vec<u8>> {
        let tp: Vec<&str> = d.tparams.iter().map(|(n, _)| n.as_str()).collect();
        let is_enum = d.kind == DeclKind::Enum;
        let is_record = d.kind == DeclKind::Record;
        let super_internal = if is_enum {
            "java/lang/Enum".to_string()
        } else if is_record {
            "java/lang/Record".to_string()
        } else {
            match &d.superclass {
                Some(t) => self.internal_or_object(&t.name)?,
                None => "java/lang/Object".to_string(),
            }
        };
        let is_annotation = d.kind == DeclKind::Annotation;
        let mut w = ClassWriter::new(&d.internal, &super_internal);
        let visibility = d.access & ACC_PUBLIC;
        w.set_access(if is_enum {
            visibility | ACC_FINAL | ACC_ENUM | ACC_SUPER
        } else if is_record {
            visibility | ACC_FINAL | ACC_SUPER
        } else if is_annotation {
            visibility | ACC_INTERFACE | ACC_ABSTRACT | ACC_ANNOTATION
        } else if d.is_interface() {
            visibility | ACC_INTERFACE | ACC_ABSTRACT
        } else if d.is_abstract {
            visibility | ACC_SUPER | ACC_ABSTRACT
        } else {
            visibility | (d.access & ACC_FINAL) | ACC_SUPER
        });
        for i in &d.interfaces {
            match self.internal_of(&i.name) {
                Some(internal) => w.add_interface(&internal),
                None if self.mode.is_lenient() => {}
                None => return None,
            }
        }
        if is_annotation {
            w.add_interface("java/lang/annotation/Annotation");
        }
        if is_enum {
            let mut signature = format!("Ljava/lang/Enum<L{};>;", d.internal);
            let mut complete = true;
            for interface in &d.interfaces {
                match self.sig(interface, &tp) {
                    Some(interface) => signature.push_str(&interface),
                    None if self.mode.is_lenient() => {
                        complete = false;
                        break;
                    }
                    None => return None,
                }
            }
            if complete {
                w.set_signature(&signature);
            } else {
                w.set_signature(&format!("Ljava/lang/Enum<L{};>;", d.internal));
            }
        } else {
            let generic = !d.tparams.is_empty()
                || d.superclass
                    .iter()
                    .chain(d.interfaces.iter())
                    .any(|t| !t.args.is_empty() || tp.contains(&t.name.as_str()));
            if generic {
                let default_super = if is_record {
                    "java/lang/Record"
                } else {
                    "java/lang/Object"
                };
                match self.build_class_sig(d, &tp, default_super) {
                    Some(sig) => w.set_signature(&sig),
                    None if self.mode.is_lenient() => {}
                    None => return None,
                }
            }
        }

        for c in &d.enum_constants {
            w.add_field_sig(
                ACC_PUBLIC | ACC_STATIC | ACC_FINAL | ACC_ENUM,
                c,
                &format!("L{};", d.internal),
                None,
            );
        }

        for (name, ty, acc) in &d.fields {
            let desc = self.desc(ty, &tp)?;
            let fsig = if !ty.args.is_empty() || tp.contains(&ty.name.as_str()) {
                match self.sig(ty, &tp) {
                    Some(s) => Some(s),
                    None if self.mode.is_lenient() => None,
                    None => return None,
                }
            } else {
                None
            };
            w.add_field_sig(*acc & !STUB_DEFAULT, name, &desc, fsig.as_deref());
        }

        if is_record {
            for (name, ty) in &d.record_components {
                let desc = self.desc(ty, &tp)?;
                let fsig = if !ty.args.is_empty() || tp.contains(&ty.name.as_str()) {
                    match self.sig(ty, &tp) {
                        Some(s) => Some(s),
                        None if self.mode.is_lenient() => None,
                        None => return None,
                    }
                } else {
                    None
                };
                w.add_field_sig(ACC_PRIVATE | ACC_FINAL, name, &desc, fsig.as_deref());
            }
        }

        let default_ctor = Member {
            name: "<init>".into(),
            tparams: Vec::new(),
            params: Vec::new(),
            ret: None,
            access: ACC_PUBLIC,
        };
        let enum_default_ctor = Member {
            name: "<init>".into(),
            tparams: Vec::new(),
            params: Vec::new(),
            ret: None,
            access: ACC_PRIVATE,
        };
        let record_canonical_ctor = Member {
            name: "<init>".into(),
            tparams: Vec::new(),
            params: d.record_components.iter().map(|(_, t)| t.clone()).collect(),
            ret: None,
            access: ACC_PUBLIC,
        };
        let ctors: Vec<&Member> = if is_record {
            let canonical_descriptor =
                self.erased_parameters(&record_canonical_ctor.params, &tp)?;
            let has_explicit_canonical = d
                .ctors
                .iter()
                .filter_map(|member| self.erased_parameters(&member.params, &tp))
                .any(|descriptor| descriptor == canonical_descriptor);
            let mut v: Vec<&Member> = Vec::new();
            if !has_explicit_canonical {
                v.push(&record_canonical_ctor);
            }
            v.extend(d.ctors.iter());
            v
        } else if !d.ctors.is_empty() {
            d.ctors.iter().collect()
        } else if is_enum {
            vec![&enum_default_ctor]
        } else if !d.is_interface() {
            vec![&default_ctor]
        } else {
            Vec::new()
        };

        let mut record_accessors: Vec<Member> = Vec::new();
        if is_record {
            for (name, ty) in &d.record_components {
                let declared = d
                    .methods
                    .iter()
                    .any(|m| m.name == *name && m.params.is_empty());
                if !declared {
                    record_accessors.push(Member {
                        name: name.clone(),
                        tparams: Vec::new(),
                        params: Vec::new(),
                        ret: Some(ty.clone()),
                        access: ACC_PUBLIC,
                    });
                }
            }
        }

        for m in ctors
            .into_iter()
            .chain(record_accessors.iter())
            .chain(d.methods.iter())
        {
            self.emit_member(&mut w, d, m, &tp)?;
        }
        if is_enum {
            let arr = format!("()[L{};", d.internal);
            let vof = format!("(Ljava/lang/String;)L{};", d.internal);
            add_static_stub(&mut w, "values", &arr, 0);
            add_static_stub(&mut w, "valueOf", &vof, 1);
        }
        Some(w.finish())
    }

    fn build_member_sig(&self, m: &Member, scope: &[&str]) -> Option<String> {
        let mut s = self.tparam_block(&m.tparams, scope)?;
        s.push('(');
        for p in &m.params {
            s.push_str(&self.sig(p, scope)?);
        }
        s.push(')');
        match &m.ret {
            Some(r) => s.push_str(&self.sig(r, scope)?),
            None => s.push('V'),
        }
        Some(s)
    }

    fn emit_member(
        &self,
        w: &mut ClassWriter,
        d: &RawDecl,
        m: &Member,
        class_tp: &[&str],
    ) -> Option<()> {
        let mut scope = class_tp.to_vec();
        scope.extend(m.tparams.iter().map(|(n, _)| n.as_str()));
        let enum_constructor = d.kind == DeclKind::Enum && m.name == "<init>";
        let mut desc = if enum_constructor {
            "(Ljava/lang/String;I".to_string()
        } else {
            "(".to_string()
        };
        desc.push_str(&self.erased_parameters(&m.params, &scope)?);
        desc.push(')');
        match &m.ret {
            Some(r) => desc.push_str(&self.desc(r, &scope)?),
            None => desc.push('V'),
        }
        let generic = !m.tparams.is_empty()
            || m.params
                .iter()
                .chain(m.ret.iter())
                .any(|t| !t.args.is_empty() || scope.contains(&t.name.as_str()));
        let sig = if generic {
            match self.build_member_sig(m, &scope) {
                Some(s) => Some(s),
                None if self.mode.is_lenient() => None,
                None => return None,
            }
        } else {
            None
        };
        // Abstractness: explicit `abstract`, or an interface method that is neither `default` nor
        // `static`. Everything else gets a 2-byte dummy body (stubs are never JVM-loaded).
        let is_abstract = m.access & ACC_ABSTRACT != 0
            || (d.is_interface() && m.access & (STUB_DEFAULT | ACC_STATIC) == 0);
        let mut acc = (m.access & !STUB_DEFAULT & !ACC_ABSTRACT)
            | if d.is_interface() { ACC_PUBLIC } else { 0 };
        if enum_constructor {
            acc = (acc & !(ACC_PUBLIC | ACC_PROTECTED)) | ACC_PRIVATE;
        }
        if is_abstract {
            w.add_abstract_method_sig(acc, &m.name, &desc, sig.as_deref());
        } else {
            let this_slot = if m.access & ACC_STATIC != 0 { 0 } else { 1 };
            let synthetic_locals = if enum_constructor { 2 } else { 0 };
            let arg_locals =
                this_slot + synthetic_locals + m.params.iter().map(slot_width).sum::<u16>();
            let mut code = CodeBuilder::new(arg_locals);
            code.aconst_null();
            code.athrow();
            w.add_method_sig(acc, &m.name, &desc, &code, sig.as_deref());
        }
        Some(())
    }

    fn erased_parameters(&self, params: &[SrcType], scope: &[&str]) -> Option<String> {
        let mut descriptor = String::new();
        for parameter in params {
            descriptor.push_str(&self.desc(parameter, scope)?);
        }
        Some(descriptor)
    }
}

fn add_static_stub(w: &mut ClassWriter, name: &str, desc: &str, arg_locals: u16) {
    let mut code = CodeBuilder::new(arg_locals);
    code.aconst_null();
    code.athrow();
    w.add_method_sig(ACC_PUBLIC | ACC_STATIC, name, desc, &code, None);
}

/// JVM local-slot width of a parameter (2 for `long`/`double` scalars, else 1).
fn slot_width(t: &SrcType) -> u16 {
    if t.array == 0 && (t.name == "long" || t.name == "double") {
        2
    } else {
        1
    }
}

fn primitive_desc(name: &str) -> Option<&'static str> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classreader::parse_class;

    fn stubs(java: &str, known: &[&str]) -> Option<Vec<(String, Vec<u8>)>> {
        let sources = vec![("T.java".to_string(), java.to_string())];
        let known: Vec<String> = known.iter().map(|s| s.to_string()).collect();
        stub_classes(&sources, StubMode::Strict, &|c| {
            known.iter().any(|k| k == c)
        })
    }

    #[test]
    fn plain_class_with_static_method() {
        let out = stubs(
            "public class J { public static String greet() { return \"x\"; } }",
            &["java/lang/String", "java/lang/Object"],
        )
        .expect("stub");
        assert_eq!(out.len(), 1);
        let ci = parse_class(&out[0].1).expect("parse stub");
        assert_eq!(ci.this_class.render(), "J");
        assert_eq!(
            ci.super_class.as_ref().map(|s| s.render()).as_deref(),
            Some("java/lang/Object")
        );
        let m = ci
            .method("greet", "()Ljava/lang/String;")
            .expect("greet present");
        assert!(m.is_static());
        // Implicit default ctor synthesized.
        assert!(ci.method("<init>", "()V").is_some());
    }

    #[test]
    fn generic_class_extends_known_generic_supertype_with_signature() {
        // The kt40180_3 shape: Java abstract class extends a (Kotlin) generic class and
        // implements a (Kotlin) generic interface.
        let out = stubs(
            "public abstract class B<E> extends A<E> implements L<E> {\n\
             public String callIndexAdd(int x) { add(0, null); return null; }\n\
             }",
            &["A", "L", "java/lang/String", "java/lang/Object"],
        )
        .expect("stub");
        let ci = parse_class(&out[0].1).expect("parse stub");
        assert_eq!(
            ci.super_class.as_ref().map(|s| s.render()).as_deref(),
            Some("A")
        );
        assert_eq!(
            ci.interfaces.iter().map(|i| i.render()).collect::<Vec<_>>(),
            ["L"]
        );
        assert_eq!(
            ci.signature.as_deref(),
            Some("<E:Ljava/lang/Object;>LA<TE;>;LL<TE;>;")
        );
        assert!(ci.method("callIndexAdd", "(I)Ljava/lang/String;").is_some());
    }

    #[test]
    fn interface_methods_are_abstract_unless_default() {
        let out = stubs(
            "public interface Test<T> { T test(T p); default int n() { return 1; } }",
            &["java/lang/Object"],
        )
        .expect("stub");
        let ci = parse_class(&out[0].1).expect("parse stub");
        let t = ci
            .method("test", "(Ljava/lang/Object;)Ljava/lang/Object;")
            .expect("test");
        assert!(t.access & ACC_ABSTRACT != 0);
        let n = ci.method("n", "()I").expect("default n");
        assert!(n.access & ACC_ABSTRACT == 0);
    }

    #[test]
    fn unresolvable_reference_type_aborts() {
        assert!(stubs(
            "public class J { public Missing f() { return null; } }",
            &["java/lang/Object"],
        )
        .is_none());
    }

    #[test]
    fn imports_package_and_varargs_resolve() {
        let out = stubs(
            "package p.q;\nimport java.util.List;\npublic class J {\n\
             public List<String> xs(int... ns) { return null; }\n\
             }",
            &["java/util/List", "java/lang/String", "java/lang/Object"],
        )
        .expect("stub");
        assert_eq!(out[0].0, "p/q/J");
        let ci = parse_class(&out[0].1).expect("parse");
        assert!(ci.method("xs", "([I)Ljava/util/List;").is_some());
    }

    #[test]
    fn nested_class_gets_dollar_name() {
        let out = stubs(
            "public class Outer { public static class Inner { public int v; } }",
            &["java/lang/Object"],
        )
        .expect("stub");
        let names: Vec<&str> = out.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"Outer") && names.contains(&"Outer$Inner"),
            "{names:?}"
        );
    }

    #[test]
    fn enum_emits_constants_values_and_valueof() {
        let out = stubs(
            "package p;\npublic enum Color { RED, GREEN, BLUE; public int rank() { return 0; } }",
            &["java/lang/Enum", "java/lang/String", "java/lang/Object"],
        )
        .expect("enum stub");
        let ci = crate::jvm::classreader::parse_class(&out[0].1).expect("parse");
        assert_eq!(
            ci.super_class.as_ref().map(|s| s.render()).as_deref(),
            Some("java/lang/Enum")
        );
        assert!(ci.access & ACC_ENUM != 0, "class is ACC_ENUM");
        assert!(
            ci.fields.iter().any(|f| f.name == "RED") && ci.fields.iter().any(|f| f.name == "BLUE"),
            "constants as fields"
        );
        assert!(ci.method("values", "()[Lp/Color;").is_some());
        assert!(ci
            .method("valueOf", "(Ljava/lang/String;)Lp/Color;")
            .is_some());
        assert!(ci.method("rank", "()I").is_some());
        assert!(
            ci.method("<init>", "()V").is_none(),
            "no public no-arg ctor for an enum with no declared constructor"
        );
        let ctor = ci
            .method("<init>", "(Ljava/lang/String;I)V")
            .expect("implicit enum ctor (String, int)");
        assert!(
            ctor.access & ACC_PUBLIC == 0,
            "implicit enum ctor must be private, not public"
        );
    }

    #[test]
    fn enum_keeps_generic_interface_and_explicit_constructor_abi() {
        let out = stubs(
            "public interface Label<T> {} \
             public enum Color implements Label<String> { RED(1); Color(int rank) {} }",
            &["java/lang/Enum", "java/lang/String", "java/lang/Object"],
        )
        .expect("enum stubs");
        let ci = out
            .iter()
            .find(|(name, _)| name == "Color")
            .and_then(|(_, bytes)| parse_class(bytes).ok())
            .expect("Color");

        assert_eq!(
            ci.signature.as_deref(),
            Some("Ljava/lang/Enum<LColor;>;LLabel<Ljava/lang/String;>;")
        );
        let ctor = ci
            .method("<init>", "(Ljava/lang/String;II)V")
            .expect("explicit enum constructor");
        assert_eq!(ctor.access & (ACC_PUBLIC | ACC_PROTECTED), 0);
    }

    #[test]
    fn same_compilation_cross_file_types_resolve() {
        let sources = vec![
            (
                "A.java".to_string(),
                "public class A { public B mk() { return null; } }".to_string(),
            ),
            ("B.java".to_string(), "public class B {}".to_string()),
        ];
        let out =
            stub_classes(&sources, StubMode::Strict, &|c| c == "java/lang/Object").expect("stubs");
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn record_emits_fields_accessors_and_canonical_ctor() {
        let out = stubs(
            "package p;\npublic record Point(int x, String y) {}",
            &["java/lang/Record", "java/lang/String", "java/lang/Object"],
        )
        .expect("record stub");
        let ci = crate::jvm::classreader::parse_class(&out[0].1).expect("parse");
        assert_eq!(
            ci.super_class.as_ref().map(|s| s.render()).as_deref(),
            Some("java/lang/Record")
        );
        assert!(ci.method("x", "()I").is_some(), "component accessor x()");
        assert!(
            ci.method("y", "()Ljava/lang/String;").is_some(),
            "component accessor y()"
        );
        assert!(
            ci.method("<init>", "(ILjava/lang/String;)V").is_some(),
            "canonical ctor"
        );
    }

    #[test]
    fn record_canonical_constructor_deduplicates_resolved_types() {
        let out = stubs(
            "public record Name(String value) { \
             public Name(java.lang.String value) {} \
             }",
            &["java/lang/Record", "java/lang/String", "java/lang/Object"],
        )
        .expect("record stub");
        let ci = parse_class(&out[0].1).expect("parse");
        assert_eq!(
            ci.methods
                .iter()
                .filter(|method| {
                    method.name == "<init>" && method.descriptor == "(Ljava/lang/String;)V"
                })
                .count(),
            1
        );
    }

    #[test]
    fn annotation_type_emits_abstract_element_methods() {
        let out = stubs(
            "package p;\npublic @interface Tag { int value() default 1; String[] names(); }",
            &[
                "java/lang/annotation/Annotation",
                "java/lang/String",
                "java/lang/Object",
            ],
        )
        .expect("annotation stub");
        let ci = crate::jvm::classreader::parse_class(&out[0].1).expect("parse");
        assert!(ci.access & ACC_ANNOTATION != 0 && ci.access & ACC_INTERFACE != 0);
        assert_eq!(
            ci.interfaces.iter().map(|i| i.render()).collect::<Vec<_>>(),
            ["java/lang/annotation/Annotation"]
        );
        assert!(ci.method("value", "()I").is_some());
        assert!(ci.method("names", "()[Ljava/lang/String;").is_some());
    }

    #[test]
    fn lenient_erases_unresolvable_type_to_object_without_aborting() {
        let sources = vec![(
            "T.java".to_string(),
            "public class J { public Missing f() { return null; } }".to_string(),
        )];
        let out = stub_classes(&sources, StubMode::Lenient, &|c| c == "java/lang/Object")
            .expect("lenient emits despite unresolvable Missing");
        let ci = crate::jvm::classreader::parse_class(&out[0].1).expect("parse");
        assert!(ci.method("f", "()Ljava/lang/Object;").is_some());
    }

    #[test]
    fn lenient_mode_isolates_malformed_files_and_unresolved_interfaces() {
        let sources = vec![
            (
                "Broken.java".to_string(),
                "public class Broken {".to_string(),
            ),
            (
                "Good.java".to_string(),
                "public class Good implements Missing {}".to_string(),
            ),
        ];
        let out = stub_classes(&sources, StubMode::Lenient, &|candidate| {
            candidate == "java/lang/Object"
        })
        .expect("partial stubs");
        assert_eq!(out.len(), 1);
        let ci = parse_class(&out[0].1).expect("Good");
        assert_eq!(ci.this_class.render(), "Good");
        assert!(ci.interfaces.is_empty());
    }

    #[test]
    fn duplicate_and_private_types_do_not_enter_the_overlay() {
        let sources = vec![
            ("First.java".to_string(), "public class Same {}".to_string()),
            (
                "Second.java".to_string(),
                "public class Same {}".to_string(),
            ),
            (
                "Outer.java".to_string(),
                "class Outer { private static class Secret {} \
                 public static class Visible {} }"
                    .to_string(),
            ),
            (
                "Holder.java".to_string(),
                "class Holder { Same value() { return null; } }".to_string(),
            ),
        ];
        let out = stub_classes(&sources, StubMode::Lenient, &|candidate| {
            candidate == "java/lang/Object"
        })
        .expect("stubs");
        assert!(!out.iter().any(|(name, _)| name == "Same"));
        assert!(!out.iter().any(|(name, _)| name == "Outer$Secret"));
        let outer = out
            .iter()
            .find(|(name, _)| name == "Outer")
            .and_then(|(_, bytes)| parse_class(bytes).ok())
            .expect("Outer");
        assert!(!outer.is_public());
        let visible = out
            .iter()
            .find(|(name, _)| name == "Outer$Visible")
            .and_then(|(_, bytes)| parse_class(bytes).ok())
            .expect("Visible");
        assert!(visible.is_public());
        let holder = out
            .iter()
            .find(|(name, _)| name == "Holder")
            .and_then(|(_, bytes)| parse_class(bytes).ok())
            .expect("Holder");
        assert!(holder.method("value", "()Ljava/lang/Object;").is_some());
    }
}

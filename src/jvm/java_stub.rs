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
//! Modeled subset: top-level and nested `class`/`interface` declarations, type parameters with
//! bounds, generic supertypes, methods/constructors/fields, varargs, arrays, wildcards (modeled as
//! their bound). Outside the subset — enums, records, annotation types — `None`.

use super::classfile::{
    ClassWriter, CodeBuilder, ACC_ABSTRACT, ACC_FINAL, ACC_INTERFACE, ACC_PUBLIC, ACC_STATIC,
    ACC_SUPER,
};

/// Internal-only marker bit for a `default` interface method (cleared before emission — it shares
/// no bit with a real JVM class-file flag we emit).
const STUB_DEFAULT: u16 = 0x8000;

/// Generate stub classes for `sources` (`(file_name, java_source)` pairs).
/// `resolve(candidate_internal)` answers whether a type exists in the caller's world. Returns
/// `(internal_name, class_bytes)` for every declared type (nested included), or `None` for shapes
/// outside the modeled subset or with unresolvable types — the caller skips, never guesses.
pub fn stub_classes(
    sources: &[(String, String)],
    resolve: &dyn Fn(&str) -> bool,
) -> Option<Vec<(String, Vec<u8>)>> {
    // Two passes: collect every declared type's internal name first, so same-compilation Java
    // types resolve against each other regardless of file order.
    let mut parsed: Vec<(FileCtx, Vec<RawDecl>)> = Vec::new();
    let mut declared: Vec<String> = Vec::new();
    for (_, src) in sources {
        let toks = lex_java(src);
        let (ctx, decls) = parse_file(&toks)?;
        for d in &decls {
            declared.push(d.internal.clone());
        }
        parsed.push((ctx, decls));
    }
    let resolve_all = |cand: &str| declared.iter().any(|d| d == cand) || resolve(cand);

    let mut out = Vec::new();
    for (ctx, decls) in &parsed {
        let r = Resolver {
            ctx,
            resolve: &resolve_all,
        };
        for raw in decls {
            out.push((raw.internal.clone(), r.emit(raw)?));
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

/// A parsed type declaration with unresolved source types.
struct RawDecl {
    /// Internal name (`pkg/Outer$Inner`).
    internal: String,
    is_interface: bool,
    is_abstract: bool,
    tparams: Vec<(String, Option<SrcType>)>,
    /// `extends` for a class (`None` = `java/lang/Object`); an interface's `extends` list is in
    /// `interfaces`.
    superclass: Option<SrcType>,
    interfaces: Vec<SrcType>,
    ctors: Vec<Member>,
    methods: Vec<Member>,
    fields: Vec<(String, SrcType, u16)>,
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
                    // private/protected and the rest carry no bit the stub consumer reads.
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

/// Parse a `class`/`interface` declaration (nested types recurse with `outer` set). Enums,
/// records and annotation types are outside the modeled subset (`None`).
fn type_decl(p: &mut P, package: &str, outer: Option<&str>, out: &mut Vec<RawDecl>) -> Option<()> {
    let acc = modifiers(p)?;
    let is_interface = if p.eat_ident("class") {
        false
    } else if p.eat_ident("interface") {
        true
    } else {
        return None; // enum / record / @interface / stray token
    };
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
        is_interface,
        is_abstract: acc & ACC_ABSTRACT != 0,
        tparams,
        superclass,
        interfaces,
        ctors: Vec::new(),
        methods: Vec::new(),
        fields: Vec::new(),
    };

    // Members until the closing `}`.
    loop {
        if p.eat_punct('}') {
            break;
        }
        if p.eat_punct(';') {
            continue;
        }
        let macc = modifiers(p)?;
        // Nested type?
        if matches!(p.peek(), Some(Tok::Ident(s)) if s == "class" || s == "interface") {
            type_decl(p, package, Some(&internal), out)?;
            continue;
        }
        // Initializer block: `static { … }` (its `static` was eaten by `modifiers`) or `{ … }`.
        if p.eat_punct('{') {
            p.skip_braces();
            continue;
        }
        // Nested enum / record / @interface: outside the subset.
        if matches!(p.peek(), Some(Tok::Ident(s)) if s == "enum" || s == "record")
            || p.peek() == Some(&Tok::Punct('@'))
        {
            return None;
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
                access: macc & ACC_PUBLIC,
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
            s.push('L');
            s.push_str(&self.internal_of(&t.name)?);
            s.push(';');
        }
        // Type ARGUMENTS don't shape the erased descriptor, but an unresolvable one must still
        // abort (the signature attribute embeds it).
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

    /// Emit one stub class. `None` if any referenced type fails to resolve.
    fn emit(&self, d: &RawDecl) -> Option<Vec<u8>> {
        let tp: Vec<&str> = d.tparams.iter().map(|(n, _)| n.as_str()).collect();
        let super_internal = match &d.superclass {
            Some(t) => self.internal_of(&t.name)?,
            None => "java/lang/Object".to_string(),
        };
        let mut w = ClassWriter::new(&d.internal, &super_internal);
        w.set_access(if d.is_interface {
            ACC_PUBLIC | ACC_INTERFACE | ACC_ABSTRACT
        } else if d.is_abstract {
            ACC_PUBLIC | ACC_SUPER | ACC_ABSTRACT
        } else {
            ACC_PUBLIC | ACC_SUPER
        });
        for i in &d.interfaces {
            let internal = self.internal_of(&i.name)?;
            w.add_interface(&internal);
        }
        // Class-level Signature when the declaration involves generics anywhere.
        let generic = !d.tparams.is_empty()
            || d.superclass
                .iter()
                .chain(d.interfaces.iter())
                .any(|t| !t.args.is_empty() || tp.contains(&t.name.as_str()));
        if generic {
            let mut sig = self.tparam_block(&d.tparams, &tp)?;
            match &d.superclass {
                Some(t) => sig.push_str(&self.sig(t, &tp)?),
                None => sig.push_str("Ljava/lang/Object;"),
            }
            for i in &d.interfaces {
                sig.push_str(&self.sig(i, &tp)?);
            }
            w.set_signature(&sig);
        }

        for (name, ty, acc) in &d.fields {
            let desc = self.desc(ty, &tp)?;
            let fsig = if !ty.args.is_empty() || tp.contains(&ty.name.as_str()) {
                Some(self.sig(ty, &tp)?)
            } else {
                None
            };
            w.add_field_sig(*acc & !STUB_DEFAULT, name, &desc, fsig.as_deref());
        }

        // Constructors: as declared, or Java's implicit public no-arg default constructor.
        let default_ctor = Member {
            name: "<init>".into(),
            tparams: Vec::new(),
            params: Vec::new(),
            ret: None,
            access: ACC_PUBLIC,
        };
        let ctors: Vec<&Member> = if d.ctors.is_empty() && !d.is_interface {
            vec![&default_ctor]
        } else {
            d.ctors.iter().collect()
        };
        for m in ctors.into_iter().chain(d.methods.iter()) {
            self.emit_member(&mut w, d, m, &tp)?;
        }
        Some(w.finish())
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
        let mut desc = String::from("(");
        for p in &m.params {
            desc.push_str(&self.desc(p, &scope)?);
        }
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
            let mut s = self.tparam_block(&m.tparams, &scope)?;
            s.push('(');
            for p in &m.params {
                s.push_str(&self.sig(p, &scope)?);
            }
            s.push(')');
            match &m.ret {
                Some(r) => s.push_str(&self.sig(r, &scope)?),
                None => s.push('V'),
            }
            Some(s)
        } else {
            None
        };
        // Abstractness: explicit `abstract`, or an interface method that is neither `default` nor
        // `static`. Everything else gets a 2-byte dummy body (stubs are never JVM-loaded).
        let is_abstract = m.access & ACC_ABSTRACT != 0
            || (d.is_interface && m.access & (STUB_DEFAULT | ACC_STATIC) == 0);
        let acc = (m.access & !STUB_DEFAULT & !ACC_ABSTRACT)
            | if d.is_interface { ACC_PUBLIC } else { 0 };
        if is_abstract {
            w.add_abstract_method_sig(acc, &m.name, &desc, sig.as_deref());
        } else {
            let arg_locals = 1 + m.params.iter().map(slot_width).sum::<u16>();
            let mut code = CodeBuilder::new(arg_locals);
            code.aconst_null();
            code.athrow();
            w.add_method_sig(acc, &m.name, &desc, &code, sig.as_deref());
        }
        Some(())
    }
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
        stub_classes(&sources, &|c| known.iter().any(|k| k == c))
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
    fn enum_is_outside_the_subset() {
        assert!(stubs("public enum E { A, B }", &[]).is_none());
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
        let out = stub_classes(&sources, &|c| c == "java/lang/Object").expect("stubs");
        assert_eq!(out.len(), 2);
    }
}

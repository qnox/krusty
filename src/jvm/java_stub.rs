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
    ClassWriter, CodeBuilder, InnerClassSpec, ACC_ABSTRACT, ACC_ANNOTATION, ACC_ENUM, ACC_FINAL,
    ACC_INTERFACE, ACC_PRIVATE, ACC_PROTECTED, ACC_PUBLIC, ACC_STATIC, ACC_SUPER,
};
use crate::java_source::{
    lex_java, parse_raw_file, primitive_desc, resolve_internal_name, DeclKind, FileCtx,
    JavaConstant, Member, RawDecl, SrcType, STUB_DEFAULT,
};
use std::collections::{HashMap, HashSet};

#[cfg(test)]
use crate::java_source::parse_source_file;

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
    for (_, src) in sources {
        let toks = lex_java(src);
        let (ctx, decls, _) = match parse_raw_file(&toks) {
            Some(parsed) => parsed,
            None if mode.is_lenient() => continue,
            None => return None,
        };
        parsed.push((ctx, decls));
    }
    let emittable_declarations = parsed
        .iter()
        .flat_map(|(_, declarations)| declarations)
        .filter(|declaration| {
            declaration.outer_internal.is_none() || declaration.access & ACC_PRIVATE == 0
        })
        .map(|declaration| declaration.internal.as_str())
        .collect::<HashSet<_>>();
    // This parser-owned graph is the only authority for lexical nesting. Walking encoded names by
    // splitting `$` would make a legal `$` inside an identifier look like an enclosing declaration.
    let declaration_outers = parsed
        .iter()
        .flat_map(|(_, declarations)| declarations)
        .map(|declaration| {
            (
                declaration.internal.as_str(),
                declaration.outer_internal.as_deref(),
            )
        })
        .collect::<HashMap<_, _>>();
    let resolve_all = |cand: &str| emittable_declarations.contains(cand) || resolve(cand);

    let mut out = Vec::new();
    let mut emitted: HashSet<&str> = HashSet::new();
    for (ctx, decls) in &parsed {
        for raw in decls {
            if (raw.outer_internal.is_some() && raw.access & ACC_PRIVATE != 0)
                || !emitted.insert(raw.internal.as_str())
            {
                continue;
            }
            let r = Resolver {
                ctx,
                resolve: &resolve_all,
                mode,
                owner: Some(raw.internal.as_str()),
                declaration_outers: &declaration_outers,
            };
            match r.emit(raw) {
                Some(bytes) => out.push((raw.internal.clone(), bytes)),
                None if mode.is_lenient() => continue,
                None => return None,
            }
        }
    }
    Some(out)
}

struct Resolver<'a> {
    ctx: &'a FileCtx,
    resolve: &'a dyn Fn(&str) -> bool,
    mode: StubMode,
    /// Internal name of the declaration being emitted — member types of the enclosing chain
    /// shadow the package (`Proc` inside `Builder` is `Builder$Proc`, JLS scoping).
    owner: Option<&'a str>,
    /// Parsed internal name → syntactic enclosing declaration. This is intentionally distinct from
    /// the encoded `$` spelling, which is ambiguous for legal Java identifiers containing `$`.
    declaration_outers: &'a HashMap<&'a str, Option<&'a str>>,
}

/// The declaration flags Java assigns after applying kind-specific implicit modifiers. This is the
/// single semantic source for both the class header and the declaration's `InnerClasses` self entry.
/// Those two classfile locations allow different flag subsets, but must never disagree about whether
/// an enum is final/abstract or whether a member interface, annotation, enum, or record is static.
fn semantic_declaration_access(d: &RawDecl) -> u16 {
    let mut access = d.access
        & (ACC_PUBLIC | ACC_PRIVATE | ACC_PROTECTED | ACC_STATIC | ACC_FINAL | ACC_ABSTRACT);
    let member_static = if d.outer_internal.is_some() {
        ACC_STATIC
    } else {
        0
    };
    match d.kind {
        DeclKind::Enum => {
            access &= !(ACC_FINAL | ACC_ABSTRACT);
            access |= member_static | ACC_ENUM;
            if d.is_abstract {
                access |= ACC_ABSTRACT;
            } else if !d.enum_has_constant_body {
                access |= ACC_FINAL;
            }
        }
        DeclKind::Interface => {
            access &= !ACC_FINAL;
            access |= member_static | ACC_INTERFACE | ACC_ABSTRACT;
        }
        DeclKind::Annotation => {
            access &= !ACC_FINAL;
            access |= member_static | ACC_INTERFACE | ACC_ABSTRACT | ACC_ANNOTATION;
        }
        DeclKind::Record => {
            access &= !ACC_ABSTRACT;
            access |= member_static | ACC_FINAL;
        }
        DeclKind::Class => {
            if d.is_abstract {
                access &= !ACC_FINAL;
                access |= ACC_ABSTRACT;
            }
        }
    }
    access
}

/// Project semantic declaration flags onto JVMS `ClassFile.access_flags`. Member-only visibility and
/// `static` live in `InnerClasses`, while every non-interface class header carries `ACC_SUPER`.
fn classfile_header_access(d: &RawDecl, declaration_access: u16) -> u16 {
    let mut access = declaration_access
        & (ACC_PUBLIC | ACC_FINAL | ACC_INTERFACE | ACC_ABSTRACT | ACC_ANNOTATION | ACC_ENUM);
    if !d.is_interface() {
        access |= ACC_SUPER;
    }
    access
}

impl Resolver<'_> {
    fn internal_of(&self, name: &str) -> Option<String> {
        if let Some(owner) = self.owner {
            let nested = name.replace('.', "$");
            let mut scope = Some(owner);
            while let Some(current) = scope {
                let candidate = format!("{current}${nested}");
                if (self.resolve)(&candidate) {
                    return Some(candidate);
                }
                scope = self.declaration_outers.get(current).copied().flatten();
            }
        }
        resolve_internal_name(&self.ctx.package, &self.ctx.imports, name, self.resolve)
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
        for a in t.arguments() {
            self.desc(a, tparams)?;
        }
        Some(s)
    }

    fn append_type_arguments(
        &self,
        output: &mut String,
        arguments: &[SrcType],
        tparams: &[&str],
    ) -> Option<()> {
        if arguments.is_empty() {
            return Some(());
        }
        output.push('<');
        for argument in arguments {
            output.push_str(&self.sig(argument, tparams)?);
        }
        output.push('>');
        Some(())
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
        // Find the first source segment that denotes a classifier. Segments before it are package
        // qualifiers; segments after it are member classifiers and use `.` (not `$`) in the JVMS
        // generic Signature grammar. Resolve every prefix so strict mode still rejects an unknown
        // member in the chain.
        let classifier_start = (0..t.segments.len()).find(|end| {
            let prefix = t.segments[..=*end]
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            self.internal_of(&prefix).is_some()
        })?;
        let root_name = t.segments[..=classifier_start]
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join(".");
        s.push('L');
        s.push_str(&self.internal_of(&root_name)?);
        self.append_type_arguments(&mut s, &t.segments[classifier_start].args, tparams)?;
        for segment_index in classifier_start + 1..t.segments.len() {
            let prefix = t.segments[..=segment_index]
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            self.internal_of(&prefix)?;
            let segment = &t.segments[segment_index];
            s.push('.');
            s.push_str(&segment.name);
            self.append_type_arguments(&mut s, &segment.args, tparams)?;
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
        let declaration_access = semantic_declaration_access(d);
        w.set_access(classfile_header_access(d, declaration_access));
        if let Some(outer) = &d.outer_internal {
            // A nested class carries its own InnerClasses entry. Classpath visibility and inherited
            // classifier lookup read this entry rather than the class header. Reuse the declaration
            // access computed above so the two projections cannot independently encode kind flags.
            w.add_inner_class(InnerClassSpec {
                inner: d.internal.clone(),
                outer: Some(outer.clone()),
                name: Some(d.simple_name.clone()),
                access: declaration_access,
            });
        }
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
                    .any(|t| t.has_type_arguments() || tp.contains(&t.name.as_str()));
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

        for (name, ty, acc, constant) in &d.fields {
            let desc = self.desc(ty, &tp)?;
            let fsig = if ty.has_type_arguments() || tp.contains(&ty.name.as_str()) {
                match self.sig(ty, &tp) {
                    Some(s) => Some(s),
                    None if self.mode.is_lenient() => None,
                    None => return None,
                }
            } else {
                None
            };
            // Interface fields are implicitly public static final (JLS §9.3). Stamping the
            // semantic JVM flags here keeps every constant-holder interface on the same path;
            // callers never need to recognize a particular holder or field name.
            let acc = (*acc & !STUB_DEFAULT)
                | if d.is_interface() {
                    ACC_PUBLIC | ACC_STATIC | ACC_FINAL
                } else {
                    0
                };
            let constant = (acc & (ACC_STATIC | ACC_FINAL) == (ACC_STATIC | ACC_FINAL))
                .then(|| constant.as_ref())
                .flatten()
                .map(|constant| match constant {
                    JavaConstant::String(value) => {
                        crate::ir::IrConst::String(crate::kt_string::KtString::from(value.clone()))
                    }
                });
            w.add_field_late_sig(acc, name, &desc, fsig.as_deref(), constant, None);
        }

        if is_record {
            for (name, ty) in &d.record_components {
                let desc = self.desc(ty, &tp)?;
                let fsig = if ty.has_type_arguments() || tp.contains(&ty.name.as_str()) {
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
            throws: Vec::new(),
            access: ACC_PUBLIC,
            has_annotation_default: false,
        };
        let enum_default_ctor = Member {
            name: "<init>".into(),
            tparams: Vec::new(),
            params: Vec::new(),
            ret: None,
            throws: Vec::new(),
            access: ACC_PRIVATE,
            has_annotation_default: false,
        };
        let record_canonical_ctor = Member {
            name: "<init>".into(),
            tparams: Vec::new(),
            params: d.record_components.iter().map(|(_, t)| t.clone()).collect(),
            ret: None,
            throws: Vec::new(),
            access: ACC_PUBLIC
                | if d.record_is_varargs {
                    crate::java_source::ACC_VARARGS
                } else {
                    0
                },
            has_annotation_default: false,
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
                        throws: Vec::new(),
                        access: ACC_PUBLIC,
                        has_annotation_default: false,
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
                .any(|t| t.has_type_arguments() || scope.contains(&t.name.as_str()));
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
        if m.has_annotation_default {
            w.mark_annotation_default(&m.name, &desc);
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
    fn annotation_element_defaults_survive_source_header_stubs() {
        let out = stubs(
            "public @interface State { String name(); boolean reloadable() default true; }",
            &["java/lang/String", "java/lang/Object"],
        )
        .expect("annotation stub");
        let class = parse_class(&out[0].1).expect("parse annotation stub");

        assert!(
            !class
                .method("name", "()Ljava/lang/String;")
                .unwrap()
                .has_annotation_default
        );
        assert!(
            class
                .method("reloadable", "()Z")
                .unwrap()
                .has_annotation_default
        );
    }

    #[test]
    fn source_string_constant_survives_the_java_header_stub() {
        let out = stubs(
            "public class Constants { public static final String VALUE = \"OK\"; }",
            &["java/lang/String", "java/lang/Object"],
        )
        .expect("constant stub");
        let class = parse_class(&out[0].1).expect("parse constant stub");
        let value = class
            .fields
            .iter()
            .find(|field| field.name == "VALUE")
            .and_then(|field| field.const_value.as_ref());

        assert_eq!(
            value,
            Some(&crate::jvm::classreader::ConstVal::Str(
                crate::kt_string::KtString::from("OK")
            ))
        );
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
    fn parameterized_inner_return_retains_owner_and_member_arguments() {
        let out = stubs(
            "public interface Sam<TO, TI> { Outer<TO>.Inner<TI> get(String s); }",
            &[
                "Outer",
                "Outer$Inner",
                "java/lang/String",
                "java/lang/Object",
            ],
        )
        .expect("stub");
        let ci = parse_class(&out[0].1).expect("parse stub");
        let method = ci
            .method("get", "(Ljava/lang/String;)LOuter$Inner;")
            .expect("generic inner return");
        assert_eq!(
            method.signature.as_deref(),
            Some("(Ljava/lang/String;)LOuter<TTO;>.Inner<TTI;>;")
        );
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
        let inner = out
            .iter()
            .find(|(name, _)| name == "Outer$Inner")
            .and_then(|(_, bytes)| parse_class(bytes).ok())
            .expect("nested class");
        assert!(inner.inner_classes.iter().any(|entry| {
            entry.inner == "Outer$Inner"
                && entry.outer.as_deref() == Some("Outer")
                && entry.name.as_deref() == Some("Inner")
                && entry.access & (ACC_PUBLIC | ACC_STATIC) == (ACC_PUBLIC | ACC_STATIC)
        }));
    }

    #[test]
    fn interface_and_annotation_member_classes_are_public_static() {
        let out = stubs(
            "public interface Host { class Nested {} } @interface Mark { class Nested {} }",
            &["java/lang/Object", "java/lang/annotation/Annotation"],
        )
        .expect("stubs");
        for internal in ["Host$Nested", "Mark$Nested"] {
            let nested = out
                .iter()
                .find(|(name, _)| name == internal)
                .and_then(|(_, bytes)| parse_class(bytes).ok())
                .expect("member class");
            assert!(nested.inner_classes.iter().any(|entry| {
                entry.inner == internal
                    && entry.access & (ACC_PUBLIC | ACC_STATIC) == (ACC_PUBLIC | ACC_STATIC)
            }));
        }
    }

    #[test]
    fn dollar_in_declared_names_does_not_change_inner_class_ownership() {
        let out = stubs(
            "class Dollar$Top {} class Outer { class Inner$Part {} }",
            &["java/lang/Object"],
        )
        .expect("stubs");
        let top = out
            .iter()
            .find(|(name, _)| name == "Dollar$Top")
            .and_then(|(_, bytes)| parse_class(bytes).ok())
            .expect("top-level class");
        assert!(top.inner_classes.is_empty());

        let member = out
            .iter()
            .find(|(name, _)| name == "Outer$Inner$Part")
            .and_then(|(_, bytes)| parse_class(bytes).ok())
            .expect("member class");
        assert!(member.inner_classes.iter().any(|entry| {
            entry.inner == "Outer$Inner$Part"
                && entry.outer.as_deref() == Some("Outer")
                && entry.name.as_deref() == Some("Inner$Part")
        }));
    }

    #[test]
    fn dollar_in_top_level_name_does_not_create_a_lexical_parent() {
        // `Dollar$Top` is one top-level identifier, not `Top` nested in `Dollar`. The old encoded-name
        // walk incorrectly found `Dollar$Target` as an enclosing-scope member and accepted this stub.
        // A strict parse must reject the unresolved `Target` instead of inventing that relationship.
        assert!(stubs(
            "class Dollar { static class Target {} } class Dollar$Top { Target value; }",
            &["java/lang/Object"],
        )
        .is_none());
    }

    #[test]
    fn member_kind_flags_share_one_semantic_projection() {
        // These are the implicit JLS member modifiers that must be projected consistently into the
        // class header and its `InnerClasses` self entry. `static` and non-public visibility exist only
        // in the latter, while kind/finality/abstractness must agree between both locations.
        let out = stubs(
            "class Host { \
             interface Contract {} \
             @interface Mark {} \
             record Data(int value) {} \
             static final class Leaf {} \
             }",
            &[
                "java/lang/Object",
                "java/lang/Record",
                "java/lang/annotation/Annotation",
            ],
        )
        .expect("member-kind stubs");
        let cases = [
            (
                "Host$Contract",
                ACC_STATIC | ACC_INTERFACE | ACC_ABSTRACT,
                ACC_FINAL | ACC_ANNOTATION,
            ),
            (
                "Host$Mark",
                ACC_STATIC | ACC_INTERFACE | ACC_ABSTRACT | ACC_ANNOTATION,
                ACC_FINAL,
            ),
            (
                "Host$Data",
                ACC_STATIC | ACC_FINAL,
                ACC_INTERFACE | ACC_ABSTRACT,
            ),
            (
                "Host$Leaf",
                ACC_STATIC | ACC_FINAL,
                ACC_INTERFACE | ACC_ABSTRACT,
            ),
        ];
        for (internal, expected, forbidden) in cases {
            let class = out
                .iter()
                .find(|(name, _)| name == internal)
                .and_then(|(_, bytes)| parse_class(bytes).ok())
                .unwrap_or_else(|| panic!("missing {internal}"));
            let entry = class
                .inner_classes
                .iter()
                .find(|entry| entry.inner == internal)
                .unwrap_or_else(|| panic!("missing self entry for {internal}"));
            assert_eq!(entry.access & expected, expected, "{internal} self entry");
            assert_eq!(entry.access & forbidden, 0, "{internal} self entry");
            // `ACC_STATIC` is member metadata and therefore absent from ClassFile.access_flags.
            let shared = expected & !ACC_STATIC;
            assert_eq!(class.access & shared, shared, "{internal} class header");
            assert_eq!(class.access & forbidden, 0, "{internal} class header");
            assert_eq!(class.access & ACC_STATIC, 0, "{internal} class header");
        }
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
    fn nested_enum_flags_follow_constant_bodies_and_abstract_members() {
        let out = stubs(
            "class Outer { \
             enum Style { PLAIN, CUSTOM {} } \
             enum AbstractStyle { ONE { int value() { return 1; } }; abstract int value(); } \
             }",
            &["java/lang/Enum", "java/lang/String", "java/lang/Object"],
        )
        .expect("stubs");
        let nested = out
            .iter()
            .find(|(name, _)| name == "Outer$Style")
            .and_then(|(_, bytes)| parse_class(bytes).ok())
            .expect("nested enum");
        assert_eq!(nested.access & ACC_FINAL, 0);
        let entry = nested
            .inner_classes
            .iter()
            .find(|entry| entry.inner == "Outer$Style")
            .expect("self entry");
        assert_eq!(entry.access & ACC_FINAL, 0);
        assert_ne!(entry.access & ACC_ENUM, 0);
        assert_ne!(entry.access & ACC_STATIC, 0);

        let abstract_enum = out
            .iter()
            .find(|(name, _)| name == "Outer$AbstractStyle")
            .and_then(|(_, bytes)| parse_class(bytes).ok())
            .expect("abstract nested enum");
        assert_eq!(abstract_enum.access & ACC_FINAL, 0);
        assert_ne!(abstract_enum.access & ACC_ABSTRACT, 0);
        let abstract_entry = abstract_enum
            .inner_classes
            .iter()
            .find(|entry| entry.inner == "Outer$AbstractStyle")
            .expect("abstract self entry");
        assert_eq!(abstract_entry.access & ACC_FINAL, 0);
        assert_ne!(abstract_entry.access & ACC_ABSTRACT, 0);
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
        let constructor = ci
            .method("<init>", "(ILjava/lang/String;)V")
            .expect("canonical ctor");
        assert!(
            !constructor.is_vararg(),
            "an ordinary final record component must not gain ACC_VARARGS"
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
    fn duplicate_types_emit_once_and_private_nested_stay_out() {
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
        assert_eq!(out.iter().filter(|(name, _)| name == "Same").count(), 1);
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
        assert!(holder.method("value", "()LSame;").is_some());
    }

    #[test]
    fn interface_nested_types_are_implicitly_public() {
        // JLS §9.5: member types of an interface are implicitly public even without a modifier.
        let out = stubs(
            "public interface Registry { final class Handler { public static void go() {} } }",
            &["java/lang/Object"],
        )
        .expect("interface with nested class");
        let handler = out
            .iter()
            .find(|(name, _)| name == "Registry$Handler")
            .map(|(_, bytes)| parse_class(bytes).expect("parse"))
            .expect("Handler stub");
        assert!(handler.is_public(), "interface member types are public");
    }

    #[test]
    fn java_varargs_parameters_emit_acc_varargs() {
        // `Fix... fixes` must carry ACC_VARARGS so member resolution accepts element-style
        // calls and spreads; without it the parameter is an ordinary array.
        let out = stubs(
            "public class Holder {\n\
             \u{20} public void reg(String s, Object... fixes) {}\n\
             \u{20} public Holder(int... ns) {}\n\
             }",
            &["java/lang/Object", "java/lang/String"],
        )
        .expect("varargs stub");
        let ci = parse_class(&out[0].1).expect("parse");
        let reg = ci
            .method("reg", "(Ljava/lang/String;[Ljava/lang/Object;)V")
            .expect("reg method");
        assert!(reg.is_vararg(), "method varargs flag");
        let ctor = ci.method("<init>", "([I)V").expect("varargs ctor");
        assert!(ctor.is_vararg(), "constructor varargs flag");
    }

    #[test]
    fn record_component_varargs_mark_the_canonical_constructor() {
        let out = stubs(
            "public record Set(String... values) {}",
            &["java/lang/Record", "java/lang/String", "java/lang/Object"],
        )
        .expect("stub");
        let class = parse_class(&out[0].1).expect("parse stub");
        let constructor = class
            .method("<init>", "([Ljava/lang/String;)V")
            .expect("canonical constructor");

        assert!(constructor.is_vararg());
    }

    #[test]
    fn nonfinal_varargs_parameter_is_rejected() {
        assert!(stubs(
            "public class Broken { void call(String... values, int count) {} }",
            &["java/lang/String", "java/lang/Object"],
        )
        .is_none());
        assert!(stubs(
            "public record Broken(String... values, int count) {}",
            &["java/lang/Record", "java/lang/String", "java/lang/Object"],
        )
        .is_none());
    }

    #[test]
    fn interface_fields_are_implicitly_public_static_final() {
        // JLS §9.3: every interface field is public static final. Generic fixture names keep
        // this test tied to the language rule instead of any downstream API.
        let out = stubs(
            "public interface Mods { String STATIC = \"static\"; int LEVEL = 3; }",
            &["java/lang/Object", "java/lang/String"],
        )
        .expect("interface constants stub");
        let ci = parse_class(&out[0].1).expect("parse");
        for name in ["STATIC", "LEVEL"] {
            let field = ci
                .fields
                .iter()
                .find(|field| field.name == name)
                .expect("constant field");
            assert_eq!(
                field.access & (ACC_PUBLIC | ACC_STATIC | ACC_FINAL),
                ACC_PUBLIC | ACC_STATIC | ACC_FINAL,
                "{name} must be public static final"
            );
        }
    }

    #[test]
    fn duplicate_declarations_keep_the_first_and_stay_resolvable() {
        let sources = vec![
            (
                "first/Dup.java".to_string(),
                "package p; public class Dup { public int first() { return 1; } }".to_string(),
            ),
            (
                "second/Dup.java".to_string(),
                "package p; public class Dup { public int second() { return 2; } }".to_string(),
            ),
            (
                "Use.java".to_string(),
                "package p; public class Use { public Dup get() { return null; } }".to_string(),
            ),
        ];
        let out =
            stub_classes(&sources, StubMode::Strict, &|c| c == "java/lang/Object").expect("stubs");
        let dups: Vec<_> = out.iter().filter(|(name, _)| name == "p/Dup").collect();
        assert_eq!(dups.len(), 1);
        let ci = parse_class(&dups[0].1).expect("parse");
        assert!(
            ci.method("first", "()I").is_some(),
            "first declaration wins"
        );
        let use_ci = out
            .iter()
            .find(|(name, _)| name == "p/Use")
            .map(|(_, bytes)| parse_class(bytes).expect("parse"))
            .expect("Use stub");
        assert!(use_ci.method("get", "()Lp/Dup;").is_some());
    }

    #[test]
    fn type_use_annotations_are_skipped_in_every_type_position() {
        let out = stubs(
            "import java.util.List; import java.util.function.Supplier;\n\
             public class T {\n\
             \u{20} private List<Supplier<@Deprecated String>> synonyms;\n\
             \u{20} public String @Deprecated [] arr() { return null; }\n\
             \u{20} public List<? extends @Deprecated CharSequence> bound() { return null; }\n\
             }",
            &[
                "java/util/List",
                "java/util/function/Supplier",
                "java/lang/String",
                "java/lang/CharSequence",
                "java/lang/Object",
            ],
        )
        .expect("type-annotated members stub");
        let ci = parse_class(&out[0].1).expect("parse");
        assert!(ci.method("arr", "()[Ljava/lang/String;").is_some());
        assert!(ci.method("bound", "()Ljava/util/List;").is_some());
    }

    #[test]
    fn annotated_enum_constants_parse() {
        let out = stubs(
            "public enum Thread { BGT, EDT, @Deprecated OLD_EDT }",
            &["java/lang/Enum", "java/lang/Object", "java/lang/String"],
        )
        .expect("annotated enum constant stub");
        let ci = parse_class(&out[0].1).expect("parse");
        assert!(ci.fields.iter().any(|field| field.name == "OLD_EDT"));
    }

    #[test]
    fn source_model_uses_exact_byte_spans_for_declarations_and_types() {
        let source = "package demo;\nclass Üse { java.util.List<String> values; }\n";
        let file = parse_source_file(source).expect("Java source");
        assert_eq!(file.declarations.len(), 1);
        let declaration = &file.declarations[0];
        assert_eq!(
            &source[declaration.name_span.lo as usize..declaration.name_span.hi as usize],
            "Üse"
        );
        let referenced = file
            .references
            .iter()
            .map(|reference| &source[reference.span.lo as usize..reference.span.hi as usize])
            .collect::<Vec<_>>();
        assert_eq!(referenced, ["java.util.List<String>", "String"]);
    }

    #[test]
    fn source_model_does_not_infer_reference_scope_from_dollar_spelling() {
        // Navigation consumes the public Java source model rather than the stub resolver. Pin the same
        // structural rule at that boundary: `Dollar$Top` must not climb into top-level `Dollar` and
        // resolve its member merely because their encoded names share a textual prefix.
        let source = "class Dollar { static class Target {} } class Dollar$Top { Target value; }";
        let file = parse_source_file(source).expect("Java source");
        let target = file
            .references
            .iter()
            .find(|reference| reference.path == "Target")
            .expect("Target reference");
        assert_eq!(
            file.resolve_reference(target, &|candidate| candidate == "Dollar$Target"),
            None
        );
    }

    #[test]
    fn source_model_collects_only_grammar_backed_body_types() {
        let source = "class Use<T> { T value; void f() { int Greeter = 1; consume(Greeter); new Actual(); } }";
        let file = parse_source_file(source).expect("Java source");
        let referenced = file
            .references
            .iter()
            .map(|reference| reference.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(referenced, ["Actual"]);
    }
}

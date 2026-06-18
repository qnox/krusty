//! The JVM implementation of the [`LibrarySet`] abstraction: resolves symbols from a `.class`-jar
//! classpath (the bytecode target). All classpath reads, JVM method-descriptor parsing, and
//! `java/lang ↔ kotlin` name normalization live here — the front end (`resolve`, `ir_lower`) sees
//! only Kotlin-level `Ty`s and opaque descriptor tokens through the trait.

use crate::libraries::{LibrarySet, LibrarySeed, LibraryType, LibraryMember, LibraryCallable};
use crate::types::Ty;
use super::classpath::Classpath;
use super::jvm_class_map::{to_kotlin_internal, to_jvm_internal, kotlin_builtin_to_jvm, BUILTIN_MAPPED_NAMES};

/// A platform backed by a JVM classpath (dirs + jars + the JDK jimage).
pub struct JvmLibraries {
    cp: Classpath,
}

impl JvmLibraries {
    pub fn new(cp: Classpath) -> JvmLibraries {
        JvmLibraries { cp }
    }
}

/// Parse a JVM field/return descriptor to a `Ty`, normalizing a JVM built-in name to its Kotlin
/// identity (`java/lang/Object` → `kotlin/Any`) so the front end compares types in Kotlin terms.
pub fn desc_to_ty(d: &str) -> Ty {
    match d {
        "I" | "B" | "S" => Ty::Int,
        "J" => Ty::Long,
        "F" => Ty::Float,
        "D" => Ty::Double,
        "Z" => Ty::Boolean,
        "C" => Ty::Char,
        "V" => Ty::Unit,
        s if s == Ty::String.descriptor() => Ty::String,
        s if s.starts_with('[') => Ty::array(desc_to_ty(&s[1..])),
        s if s.starts_with('L') && s.ends_with(';') => Ty::obj(to_kotlin_internal(&s[1..s.len() - 1])),
        _ => Ty::Error,
    }
}

/// Split one JVM field descriptor off the front of `s`, returning `(descriptor, rest)`.
fn split_one(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i] == b'[' {
        i += 1;
    }
    if i >= b.len() {
        return None;
    }
    match b[i] {
        b'L' => {
            let end = s[i..].find(';')? + i + 1;
            Some((&s[..end], &s[end..]))
        }
        _ => Some((&s[..i + 1], &s[i + 1..])),
    }
}

/// Parse a method descriptor `(p…)ret` into parameter `Ty`s and the return `Ty`.
fn parse_method_desc(desc: &str) -> (Vec<Ty>, Ty) {
    let close = desc.find(')').unwrap_or(0);
    let mut rest = &desc[1..close];
    let mut params = Vec::new();
    while let Some((one, tail)) = split_one(rest) {
        params.push(desc_to_ty(one));
        rest = tail;
    }
    (params, desc_to_ty(&desc[close + 1..]))
}

/// The receiver type's descriptor and those of its supertypes (superclass chain + interfaces),
/// breadth-first so a more specific receiver is tried before a more general one.
fn supertype_descriptors(cp: &Classpath, receiver: Ty) -> Vec<String> {
    let start = match receiver {
        Ty::Obj(i, _) => to_jvm_internal(i).to_string(),
        Ty::String => to_jvm_internal("kotlin/String").to_string(),
        _ => return vec![receiver.descriptor()],
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut q = std::collections::VecDeque::new();
    q.push_back(start);
    while let Some(name) = q.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        out.push(format!("L{name};"));
        if let Some(ci) = cp.find(&name) {
            for i in &ci.interfaces {
                q.push_back(i.clone());
            }
            if let Some(s) = &ci.super_class {
                q.push_back(s.clone());
            }
        }
    }
    out
}

impl LibrarySet for JvmLibraries {
    fn seed(&self) -> LibrarySeed {
        let idx = self.cp.scan_types();
        let mut class_names = idx.class_names.clone();
        // Seed the Kotlin built-in → JVM class mapping (ported `JavaToKotlinClassMap`): intrinsic
        // mapped types (`Comparable`, `Throwable`, `List`, …), not `.class` files. Classpath types
        // above take precedence (`or_insert`).
        for name in BUILTIN_MAPPED_NAMES {
            if let Some(internal) = kotlin_builtin_to_jvm(name) {
                class_names.entry(name.to_string()).or_insert_with(|| internal.to_string());
            }
        }
        LibrarySeed { class_names, type_aliases: idx.type_aliases.clone() }
    }

    fn resolve_type(&self, internal: &str) -> Option<LibraryType> {
        let ci = self.cp.find(internal)?;
        let mut constructors = Vec::new();
        let mut members = Vec::new();
        let mut companion = Vec::new();
        for m in &ci.methods {
            // Only public members are callable from generated code.
            if !m.is_public() {
                continue;
            }
            let (params, ret) = parse_method_desc(&m.descriptor);
            let member = LibraryMember { name: m.name.clone(), params, ret, descriptor: m.descriptor.clone() };
            if m.name == "<init>" {
                constructors.push(member);
            } else if m.is_static() {
                // A Kotlin companion member compiles to a JVM static on the class.
                companion.push(member);
            } else {
                members.push(member);
            }
        }
        // Every JDK `Throwable` has a no-arg and a single-message constructor; synthesize those two
        // shapes when the classpath reader can't surface the jimage constructor descriptors.
        if constructors.is_empty() && super::jvm_class_map::is_throwable_internal(internal) {
            constructors.push(LibraryMember { name: "<init>".into(), params: vec![], ret: Ty::Unit, descriptor: "()V".into() });
            constructors.push(LibraryMember { name: "<init>".into(), params: vec![Ty::String], ret: Ty::Unit, descriptor: format!("({})V", Ty::String.descriptor()) });
        }
        let mut supertypes = ci.interfaces.clone();
        if let Some(s) = &ci.super_class {
            supertypes.push(s.clone());
        }
        Some(LibraryType {
            is_public: ci.is_public(),
            is_interface: ci.is_interface(),
            is_annotation: ci.access & 0x2000 != 0,
            supertypes,
            constructors,
            members,
            companion,
        })
    }

    fn resolve_callable(&self, name: &str, receiver: Option<Ty>, args: &[Ty]) -> Option<LibraryCallable> {
        let Some(receiver) = receiver else {
            // Receiver-less top-level function (`listOf(…)`): find every static method of this name
            // and pick the overload matching `args` — an exact-arity match (boxing-aware), else a
            // vararg match (the final reference-array parameter absorbs the trailing arguments).
            let cands = self.cp.find_top_level(name);
            let parsed: Vec<(crate::jvm::classpath::ExtCandidate, Vec<Ty>, Ty)> = cands.into_iter().map(|c| {
                let (params, ret) = parse_method_desc(&c.descriptor);
                (c, params, ret)
            }).collect();
            let assignable = |p: &Ty, a: &Ty| p == a || *p == Ty::obj("kotlin/Any");
            // Exact arity first.
            let pick = parsed.iter().find(|(_, params, _)| {
                params.len() == args.len() && params.iter().zip(args).all(|(p, a)| assignable(p, a))
            }).or_else(|| parsed.iter().find(|(_, params, _)| {
                // Vararg: fixed leading params match positionally, the last (array) param's element
                // type absorbs the rest.
                if params.is_empty() { return args.len() == 0; }
                let fixed = params.len() - 1;
                let Some(elem) = params[fixed].array_elem() else { return false };
                args.len() >= fixed
                    && params[..fixed].iter().zip(args).all(|(p, a)| assignable(p, a))
                    && args[fixed..].iter().all(|a| assignable(&elem, a))
            }));
            let (c, params, ret) = pick?;
            return Some(LibraryCallable { owner: c.owner.clone(), name: c.name.clone(), params: params.clone(), ret: *ret, descriptor: c.descriptor.clone() });
        };
        let rest_params: String = args.iter().map(|t| t.descriptor()).collect();
        // Try the receiver type and its supertypes, most specific first — the extension's declared
        // receiver may be a supertype (kotlinc's `String.repeat` is a `CharSequence` extension).
        for recv_desc in supertype_descriptors(&self.cp, receiver) {
            let full_prefix = format!("({recv_desc}{rest_params})");
            for c in self.cp.find_extensions(&recv_desc, name) {
                if c.descriptor.starts_with(&full_prefix) {
                    let (params, ret) = parse_method_desc(&c.descriptor);
                    return Some(LibraryCallable { owner: c.owner.clone(), name: c.name.clone(), params, ret, descriptor: c.descriptor.clone() });
                }
            }
        }
        None
    }
}

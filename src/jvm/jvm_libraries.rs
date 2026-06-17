//! The JVM implementation of the [`LibrarySet`] abstraction: resolves symbols from a `.class`-jar
//! classpath (the bytecode target). All classpath reads, JVM method-descriptor parsing, and
//! `java/lang ↔ kotlin` name normalization live here — the front end (`resolve`, `ir_lower`) sees
//! only Kotlin-level `Ty`s and opaque descriptor strings through the trait.

use crate::libraries::{LibrarySet, LibrarySeed};
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

    fn annotation_members(&self, internal: &str) -> Option<Vec<(String, Ty)>> {
        let ci = self.cp.find(internal)?;
        if ci.access & 0x2000 == 0 {
            return None; // not ACC_ANNOTATION
        }
        let mut members = Vec::new();
        for m in &ci.methods {
            if m.descriptor.starts_with("()") {
                let ty = desc_to_ty(&m.descriptor[2..]);
                if ty == Ty::Error {
                    return None; // a member type we can't model — skip the whole annotation
                }
                members.push((m.name.clone(), ty));
            }
        }
        Some(members)
    }

    fn resolve_static(&self, internal: &str, method: &str, arg_tys: &[Ty]) -> Option<(String, String, Ty)> {
        let ci = self.cp.find(internal)?;
        // Only a public method on a public class is callable from generated code.
        if !ci.is_public() {
            return None;
        }
        let params: String = arg_tys.iter().map(|t| t.descriptor()).collect();
        let prefix = format!("({params})");
        let m = ci.methods.iter().find(|m| m.name == method && m.is_static() && m.is_public() && m.descriptor.starts_with(&prefix))?;
        let ret = m.descriptor[m.descriptor.find(')').unwrap() + 1..].to_string();
        Some((internal.to_string(), m.descriptor.clone(), desc_to_ty(&ret)))
    }

    fn resolve_instance(&self, internal: &str, method: &str, arg_tys: &[Ty]) -> Option<(String, Ty)> {
        // The receiver's static type must be public; the method may be inherited from a (possibly
        // non-public) superclass/interface — walk the chain as the JVM does for invokevirtual.
        if !self.cp.find(internal)?.is_public() {
            return None;
        }
        let params: String = arg_tys.iter().map(|t| t.descriptor()).collect();
        let prefix = format!("({params})");
        let mut seen = std::collections::HashSet::new();
        let mut q = std::collections::VecDeque::new();
        q.push_back(internal.to_string());
        while let Some(name) = q.pop_front() {
            if !seen.insert(name.clone()) {
                continue;
            }
            let Some(ci) = self.cp.find(&name) else { continue };
            if let Some(m) = ci.methods.iter().find(|m| m.name == method && !m.is_static() && m.is_public() && m.descriptor.starts_with(&prefix)) {
                let ret = m.descriptor[m.descriptor.find(')').unwrap() + 1..].to_string();
                return Some((m.descriptor.clone(), desc_to_ty(&ret)));
            }
            for i in &ci.interfaces {
                q.push_back(i.clone());
            }
            if let Some(s) = &ci.super_class {
                q.push_back(s.clone());
            }
        }
        None
    }

    fn resolve_ctor(&self, internal: &str, arg_tys: &[Ty]) -> Option<String> {
        let ci = self.cp.find(internal)?;
        let params: String = arg_tys.iter().map(|t| t.descriptor()).collect();
        let exact = format!("({params})V");
        if let Some(m) = ci.methods.iter().find(|m| m.name == "<init>" && m.is_public() && m.descriptor == exact) {
            return Some(m.descriptor.clone());
        }
        // Widening fallback: replace each reference-type arg with Object (e.g. String → Object).
        // Needed because e.g. AssertionError has no public (String) ctor, only public (Object).
        let widened: String = arg_tys.iter().map(|t| match t {
            Ty::String | Ty::Obj(..) | Ty::Array(_) | Ty::Null | Ty::Fun(_) => Ty::obj("kotlin/Any").descriptor(),
            _ => t.descriptor(),
        }).collect();
        let widened_exact = format!("({widened})V");
        if let Some(m) = ci.methods.iter().find(|m| m.name == "<init>" && m.is_public() && m.descriptor == widened_exact) {
            return Some(m.descriptor.clone());
        }
        // Every JDK `Throwable` has a no-arg and a single-message constructor; accept those two
        // shapes even when the classpath reader can't see the jimage constructor descriptors.
        if super::jvm_class_map::is_throwable_internal(internal) {
            return match arg_tys {
                [] => Some("()V".to_string()),
                [Ty::String] => Some(format!("({})V", Ty::String.descriptor())),
                _ => None,
            };
        }
        None
    }

    fn resolve_extension(&self, receiver: Ty, method: &str, arg_tys: &[Ty]) -> Option<(String, String, String, Ty)> {
        let rest_params: String = arg_tys.iter().map(|t| t.descriptor()).collect();
        // Try the receiver type and its supertypes, most specific first — the extension's declared
        // receiver may be a supertype (kotlinc's `String.repeat` is a `CharSequence` extension).
        for recv_desc in supertype_descriptors(&self.cp, receiver) {
            let full_prefix = format!("({recv_desc}{rest_params})");
            for c in self.cp.find_extensions(&recv_desc, method) {
                if c.descriptor.starts_with(&full_prefix) {
                    return Some((c.owner.clone(), c.name.clone(), c.descriptor.clone(), desc_to_ty(&c.ret_desc)));
                }
            }
        }
        None
    }

    fn desc_to_ty(&self, desc: &str) -> Ty {
        desc_to_ty(desc)
    }

    fn is_interface(&self, internal: &str) -> bool {
        self.cp.find(internal).map_or(false, |c| c.is_interface())
    }

    fn is_throwable(&self, internal: &str) -> bool {
        super::jvm_class_map::is_throwable_internal(internal)
    }

    fn boxed_type(&self, prim: Ty) -> Option<Ty> {
        super::jvm_class_map::wrapper_internal(prim).map(Ty::obj)
    }
}

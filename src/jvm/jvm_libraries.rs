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

/// A node of a parsed JVM generic *type signature* (the grammar behind the `Signature` attribute):
/// a type variable (`TT;`), a class type with type arguments (`Ljava/util/List<TT;>;`), an array, or
/// a primitive. Enough to recover a generic method's parameterized return from its erased descriptor.
#[derive(Clone, Debug)]
enum GSig {
    Var(String),
    Class(String, Vec<GSig>),
    Arr(Box<GSig>),
    Prim(Ty),
}

/// Parse one type signature off the front of `s`, returning `(node, rest)`.
fn parse_gsig(s: &str) -> Option<(GSig, &str)> {
    let b = s.as_bytes();
    match *b.first()? {
        b'T' => {
            let end = s.find(';')?;
            Some((GSig::Var(s[1..end].to_string()), &s[end + 1..]))
        }
        b'[' => {
            let (inner, rest) = parse_gsig(&s[1..])?;
            Some((GSig::Arr(Box::new(inner)), rest))
        }
        b'L' => {
            // Class name up to `<` (type args) or `;`. Type args (if any) are parsed, then `;`.
            let lt = s.find('<');
            let semi = s.find(';')?;
            let name_end = match lt {
                Some(i) if i < semi => i,
                _ => semi,
            };
            let internal = to_kotlin_internal(&s[1..name_end]).to_string();
            if let Some(i) = lt.filter(|&i| i < semi) {
                let mut rest = &s[i + 1..];
                let mut args = Vec::new();
                while !rest.starts_with('>') {
                    // A wildcard prefix (`+`/`-`) or unbounded `*` argument — treat as opaque (`Any`).
                    if let Some(stripped) = rest.strip_prefix('*') {
                        args.push(GSig::Class("kotlin/Any".to_string(), vec![]));
                        rest = stripped;
                        continue;
                    }
                    let r2 = rest.strip_prefix('+').or_else(|| rest.strip_prefix('-')).unwrap_or(rest);
                    let (a, tail) = parse_gsig(r2)?;
                    args.push(a);
                    rest = tail;
                }
                let after = rest.strip_prefix('>')?.strip_prefix(';')?;
                Some((GSig::Class(internal, args), after))
            } else {
                Some((GSig::Class(internal, vec![]), &s[semi + 1..]))
            }
        }
        c => {
            let t = match c {
                b'I' | b'B' | b'S' => Ty::Int,
                b'J' => Ty::Long,
                b'F' => Ty::Float,
                b'D' => Ty::Double,
                b'Z' => Ty::Boolean,
                b'C' => Ty::Char,
                b'V' => Ty::Unit,
                _ => return None,
            };
            Some((GSig::Prim(t), &s[1..]))
        }
    }
}

/// Parse a method generic signature `<formals>(params)ret` into the parameter and return nodes,
/// skipping the leading `<…>` formal-type-parameter block.
fn parse_method_gsig(sig: &str) -> Option<(Vec<GSig>, GSig)> {
    let mut s = sig;
    if s.starts_with('<') {
        // Skip the balanced `<…>` formal block.
        let mut depth = 0;
        let mut end = 0;
        for (i, ch) in s.char_indices() {
            match ch {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        if end == 0 {
            return None;
        }
        s = &s[end..];
    }
    let inner = s.strip_prefix('(')?;
    let close = inner.find(')')?;
    let mut params_s = &inner[..close];
    let mut params = Vec::new();
    while !params_s.is_empty() {
        let (p, rest) = parse_gsig(params_s)?;
        params.push(p);
        params_s = rest;
    }
    let (ret, _) = parse_gsig(&inner[close + 1..])?;
    Some((params, ret))
}

/// Parse a class generic signature into its formal type-parameter names and its supertypes (the
/// superclass followed by interfaces) as signature nodes, e.g. `java/util/List`'s
/// `<E:Ljava/lang/Object;>Ljava/lang/Object;Ljava/util/Collection<TE;>;` → (`[E]`, `[Object,
/// Collection<E>]`). The supertypes carry their own type arguments (in terms of this class's formals),
/// which is what lets a type argument propagate up the hierarchy (`List<Int>` → `Collection<Int>`).
fn parse_class_gsig(sig: &str) -> Option<(Vec<String>, Vec<GSig>)> {
    let mut s = sig;
    let mut formals = Vec::new();
    if let Some(rest) = s.strip_prefix('<') {
        // Each formal is `Name:Bound...` up to the matching `>`; collect names at depth 1.
        let mut depth = 1;
        let bytes = rest.as_bytes();
        let mut i = 0;
        let mut at_name_start = true;
        while i < bytes.len() && depth > 0 {
            match bytes[i] {
                b'<' => { depth += 1; at_name_start = false; }
                b'>' => { depth -= 1; }
                b':' => { at_name_start = false; }
                _ if depth == 1 && at_name_start => {
                    let start = i;
                    while i < bytes.len() && bytes[i] != b':' { i += 1; }
                    formals.push(rest[start..i].to_string());
                    at_name_start = false;
                    continue;
                }
                b';' if depth == 1 => { at_name_start = true; }
                _ => {}
            }
            i += 1;
        }
        s = &rest[i..];
    }
    let mut supers = Vec::new();
    while !s.is_empty() {
        let (g, rest) = parse_gsig(s)?;
        supers.push(g);
        s = rest;
    }
    Some((formals, supers))
}

/// Bind type variables by unifying a parameter signature node with an actual argument `Ty`.
fn unify_gsig(sig: &GSig, actual: Ty, binds: &mut std::collections::HashMap<String, Ty>) {
    match sig {
        GSig::Var(n) => {
            binds.entry(n.clone()).or_insert(actual);
        }
        GSig::Arr(inner) => {
            if let Some(elem) = actual.array_elem() {
                unify_gsig(inner, elem, binds);
            }
        }
        GSig::Class(_, args) => {
            // Unify the type arguments positionally against the actual's carried arguments, if any.
            if let Ty::Obj(_, targs) = actual {
                for (a, t) in args.iter().zip(targs.iter()) {
                    unify_gsig(a, *t, binds);
                }
            }
        }
        GSig::Prim(_) => {}
    }
}

/// Realize a signature node to a `Ty` under the current bindings — an unbound variable erases to
/// `Any`, a class becomes `Ty::obj_args` carrying its (substituted) type arguments.
fn gsig_to_ty(sig: &GSig, binds: &std::collections::HashMap<String, Ty>) -> Ty {
    match sig {
        GSig::Var(n) => binds.get(n).copied().unwrap_or_else(|| Ty::obj("kotlin/Any")),
        GSig::Prim(t) => *t,
        GSig::Arr(inner) => Ty::array(gsig_to_ty(inner, binds)),
        GSig::Class(internal, args) => {
            if args.is_empty() {
                Ty::obj(internal)
            } else {
                let targs: Vec<Ty> = args.iter().map(|a| gsig_to_ty(a, binds)).collect();
                Ty::obj_args(internal, &targs)
            }
        }
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

    fn member_return(&self, recv: Ty, name: &str, args: &[Ty]) -> Option<Ty> {
        let Ty::Obj(start, start_args) = recv else { return None };
        if start_args.is_empty() {
            return None; // no type arguments to propagate — the erased return is already correct
        }
        // Walk the generic hierarchy carrying each class's type arguments, substituting them through
        // each `extends`/`implements` edge. Stop at the first class declaring `name`; substitute that
        // member's generic return under the bindings reached there.
        let assignable = |p: &Ty, a: &Ty| p == a || *p == Ty::obj("kotlin/Any");
        let mut seen = std::collections::HashSet::new();
        let mut q = std::collections::VecDeque::new();
        q.push_back((to_jvm_internal(start).to_string(), start_args.to_vec()));
        while let Some((internal, targs)) = q.pop_front() {
            if !seen.insert(internal.clone()) {
                continue;
            }
            let Some(ci) = self.cp.find(&internal) else { continue };
            let (formals, supers) = ci.signature.as_deref().and_then(parse_class_gsig).unzip();
            let formals = formals.unwrap_or_default();
            let binds: std::collections::HashMap<String, Ty> =
                formals.iter().cloned().zip(targs.iter().copied()).collect();
            // A member declared here whose parameters match the call.
            let found = ci.methods.iter().filter(|m| m.is_public() && !m.is_static() && m.name == name).find(|m| {
                let (params, _) = parse_method_desc(&m.descriptor);
                params.len() == args.len() && params.iter().zip(args).all(|(p, a)| assignable(p, a))
            });
            if let Some(m) = found {
                let sig = m.signature.as_deref()?;
                let (_, rsig) = parse_method_gsig(sig)?;
                return Some(gsig_to_ty(&rsig, &binds));
            }
            // Propagate type arguments up each supertype edge (substituting this class's bindings).
            if let Some(supers) = supers {
                for sup in supers {
                    if let GSig::Class(sup_internal, sup_args) = sup {
                        let sup_targs: Vec<Ty> = sup_args.iter().map(|a| gsig_to_ty(a, &binds)).collect();
                        q.push_back((to_jvm_internal(&sup_internal).to_string(), sup_targs));
                    }
                }
            } else {
                // No generic class signature — follow raw supertypes (members there are non-generic).
                for i in ci.interfaces.iter().chain(ci.super_class.iter()) {
                    q.push_back((i.clone(), vec![]));
                }
            }
        }
        None
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
            // Recover the parameterized return from the generic signature: bind the type variables
            // from the actual arguments (the vararg element unifies with each trailing arg) and
            // substitute into the return node. Falls back to the erased return when absent.
            let ret_ty = c.signature.as_ref().and_then(|sig| parse_method_gsig(sig)).map(|(psigs, rsig)| {
                let mut binds = std::collections::HashMap::new();
                let vararg = params.len() != args.len();
                if vararg && !psigs.is_empty() {
                    let fixed = psigs.len() - 1;
                    for (i, ps) in psigs.iter().take(fixed).enumerate() {
                        if let Some(a) = args.get(i) {
                            unify_gsig(ps, *a, &mut binds);
                        }
                    }
                    if let GSig::Arr(inner) = &psigs[fixed] {
                        for a in &args[fixed..] {
                            unify_gsig(inner, *a, &mut binds);
                        }
                    }
                } else {
                    for (ps, a) in psigs.iter().zip(args) {
                        unify_gsig(ps, *a, &mut binds);
                    }
                }
                gsig_to_ty(&rsig, &binds)
            }).unwrap_or(*ret);
            return Some(LibraryCallable { owner: c.owner.clone(), name: c.name.clone(), params: params.clone(), ret: ret_ty, descriptor: c.descriptor.clone() });
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

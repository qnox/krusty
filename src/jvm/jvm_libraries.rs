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

/// Parse a leading `<Name:Bound…>` formal-type-parameter block, returning the formal names and the
/// remaining signature. No block → empty names, input unchanged.
fn parse_formals(s: &str) -> (Vec<String>, &str) {
    let Some(rest) = s.strip_prefix('<') else { return (Vec::new(), s) };
    let mut depth = 1;
    let bytes = rest.as_bytes();
    let mut i = 0;
    let mut at_name_start = true;
    let mut formals = Vec::new();
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
    (formals, &rest[i..])
}

/// Parse a method generic signature `<formals>(params)ret` into the formal type-parameter names, the
/// parameter nodes, and the return node.
fn parse_method_gsig(sig: &str) -> Option<(Vec<String>, Vec<GSig>, GSig)> {
    let (formals, s) = parse_formals(sig);
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
    Some((formals, params, ret))
}

/// Parse a class generic signature into its formal type-parameter names and its supertypes (the
/// superclass followed by interfaces) as signature nodes, e.g. `java/util/List`'s
/// `<E:Ljava/lang/Object;>Ljava/lang/Object;Ljava/util/Collection<TE;>;` → (`[E]`, `[Object,
/// Collection<E>]`). The supertypes carry their own type arguments (in terms of this class's formals),
/// which is what lets a type argument propagate up the hierarchy (`List<Int>` → `Collection<Int>`).
fn parse_class_gsig(sig: &str) -> Option<(Vec<String>, Vec<GSig>)> {
    let (formals, mut s) = parse_formals(sig);
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
        GSig::Class(internal, args) if internal.starts_with("kotlin/jvm/functions/Function") => {
            // A function parameter (`Function1<T, R>`) unifies against a lambda argument (`Ty::Fun`):
            // the leading type arguments bind the lambda's parameters, the last binds its return —
            // so `map`'s `R` binds from the lambda body's type (`{ it * 2 }` → `Int`).
            if let Ty::Fun(fsig) = actual {
                if let Some((ret_sig, in_sigs)) = args.split_last() {
                    for (a, p) in in_sigs.iter().zip(fsig.params.iter()) {
                        unify_gsig(a, *p, binds);
                    }
                    unify_gsig(ret_sig, fsig.ret, binds);
                }
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

/// If `sig` is a `kotlin/jvm/functions/FunctionN` type, the (substituted) types of its lambda
/// parameters — its first N type arguments (the last is the return type). Empty for anything else.
fn function_input_types(sig: &GSig, binds: &std::collections::HashMap<String, Ty>) -> Vec<Ty> {
    if let GSig::Class(internal, targs) = sig {
        if internal.starts_with("kotlin/jvm/functions/Function") && !targs.is_empty() {
            return targs[..targs.len() - 1].iter().map(|a| gsig_to_ty(a, binds)).collect();
        }
    }
    Vec::new()
}

/// Whether argument `a` can be passed where parameter `p` is expected, in erased Kotlin terms: an
/// exact match, any argument into an erased `Any` parameter, or the *same erased class* (a parameter
/// `Pair` accepts an argument `Pair<Int, String>` — generic parameters erase to the raw type).
fn arg_fits(p: &Ty, a: &Ty) -> bool {
    if p == a || *p == Ty::obj("kotlin/Any") {
        return true;
    }
    // A lambda (`Ty::Fun`) is passed where a `kotlin/jvm/functions/FunctionN` is expected.
    if let (Ty::Obj(pi, _), Ty::Fun(_)) = (p, a) {
        return pi.starts_with("kotlin/jvm/functions/Function");
    }
    matches!((p, a), (Ty::Obj(pi, _), Ty::Obj(ai, _)) if pi == ai)
}

/// Like [`arg_fits`], but also accepts a reference argument that is a *subtype* of a reference
/// parameter (`String` into a `CharSequence` parameter) by walking the classpath supertype chain.
/// Used where overload selection must distinguish a real subtype from an unrelated type (a `Char`
/// argument must NOT match a `CharSequence` parameter).
fn arg_fits_subtype(cp: &Classpath, p: &Ty, a: &Ty) -> bool {
    if arg_fits(p, a) {
        return true;
    }
    if a.is_reference() && matches!(p, Ty::Obj(..) | Ty::String) {
        let pd = p.descriptor();
        return supertype_descriptors(cp, *a).iter().any(|d| *d == pd);
    }
    false
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
    // Every type is a subtype of `Any`, so a generic extension declared on `T` (erased to `Object`)
    // applies to any receiver — always try `java/lang/Object` last (after the specific supertypes).
    let object = "Ljava/lang/Object;".to_string();
    let start = match receiver {
        Ty::Obj(i, _) => to_jvm_internal(i).to_string(),
        Ty::String => to_jvm_internal("kotlin/String").to_string(),
        _ => return vec![receiver.descriptor(), object],
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
    if !out.iter().any(|d| d == &object) {
        out.push(object);
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
                params.len() == args.len() && params.iter().zip(args).all(|(p, a)| arg_fits(p, a))
            });
            if let Some(m) = found {
                let sig = m.signature.as_deref()?;
                let (_, _, rsig) = parse_method_gsig(sig)?;
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

    fn extension_lambda_param_types(&self, recv: Ty, name: &str, arg_tys: &[Option<Ty>]) -> Option<Vec<Vec<Ty>>> {
        // Find a generic extension named `name` on the receiver (or a supertype) that takes a function
        // argument; bind its type variables from the receiver and the already-typed non-lambda
        // arguments, then report each lambda argument's element-typed parameters (`Function1<? super
        // T, …>` on `List<Int>` → `[Int]`; `fold(0) { acc, x -> }` binds the accumulator from `0`).
        for recv_desc in supertype_descriptors(&self.cp, recv) {
            for c in self.cp.find_extensions(&recv_desc, name) {
                let Some(sig) = c.signature.as_deref() else { continue };
                let Some((_, psigs, _)) = parse_method_gsig(sig) else { continue };
                if psigs.is_empty() || psigs.len() != arg_tys.len() + 1 {
                    continue;
                }
                let mut binds = std::collections::HashMap::new();
                unify_gsig(&psigs[0], recv, &mut binds); // bind from the receiver parameter
                for (ps, at) in psigs[1..].iter().zip(arg_tys) {
                    if let Some(t) = at {
                        unify_gsig(ps, *t, &mut binds); // bind from each typed non-lambda argument
                    }
                }
                let out: Vec<Vec<Ty>> = psigs[1..].iter().map(|ps| function_input_types(ps, &binds)).collect();
                if out.iter().any(|v| !v.is_empty()) {
                    return Some(out);
                }
            }
        }
        None
    }

    fn resolve_callable(&self, name: &str, receiver: Option<Ty>, args: &[Ty], type_args: &[Ty]) -> Option<LibraryCallable> {
        let Some(receiver) = receiver else {
            // Receiver-less top-level function (`listOf(…)`): find every static method of this name
            // and pick the overload matching `args` — an exact-arity match (boxing-aware), else a
            // vararg match (the final reference-array parameter absorbs the trailing arguments).
            let cands = self.cp.find_top_level(name);
            let parsed: Vec<(crate::jvm::classpath::ExtCandidate, Vec<Ty>, Ty)> = cands.into_iter().map(|c| {
                let (params, ret) = parse_method_desc(&c.descriptor);
                (c, params, ret)
            }).collect();
            // Exact arity first.
            let pick = parsed.iter().find(|(_, params, _)| {
                params.len() == args.len() && params.iter().zip(args).all(|(p, a)| arg_fits(p, a))
            }).or_else(|| parsed.iter().find(|(_, params, _)| {
                // Vararg: fixed leading params match positionally, the last (array) param's element
                // type absorbs the rest.
                if params.is_empty() { return args.len() == 0; }
                let fixed = params.len() - 1;
                let Some(elem) = params[fixed].array_elem() else { return false };
                args.len() >= fixed
                    && params[..fixed].iter().zip(args).all(|(p, a)| arg_fits(p, a))
                    && args[fixed..].iter().all(|a| arg_fits(&elem, a))
            }));
            let (c, params, ret) = pick?;
            // A reified reflection intrinsic (`typeOf` → `KType`) is implemented by inlining + reified
            // substitution; called as a plain static it throws at runtime. krusty doesn't inline it —
            // leave it unresolved (the file skips) rather than emit a call that fails.
            if ret.obj_internal() == Some("kotlin/reflect/KType") {
                return None;
            }
            // Recover the parameterized return from the generic signature: bind the type variables
            // from the actual arguments (the vararg element unifies with each trailing arg) and
            // substitute into the return node. Falls back to the erased return when absent.
            let ret_ty = c.signature.as_ref().and_then(|sig| parse_method_gsig(sig)).map(|(formals, psigs, rsig)| {
                let mut binds = std::collections::HashMap::new();
                // Explicit type arguments (`emptyList<Int>()`) bind the formals positionally first, so
                // a call with no value arguments still parameterizes the return.
                for (f, t) in formals.iter().zip(type_args) {
                    binds.insert(f.clone(), *t);
                }
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
            return Some(LibraryCallable { owner: c.owner.clone(), name: c.name.clone(), params: params.clone(), ret: ret_ty, physical_ret: *ret, descriptor: c.descriptor.clone(), default_call: false });
        };
        // Try the receiver type and its supertypes, most specific first — the extension's declared
        // receiver may be a supertype (kotlinc's `String.repeat` is a `CharSequence` extension), or a
        // generic `T` erased to `Object` (`fun <T> T.to(…)`). Match by boxing-aware parameter
        // assignability (an `Any` parameter accepts any argument), not exact descriptor prefix.
        for recv_desc in supertype_descriptors(&self.cp, receiver) {
            for c in self.cp.find_extensions(&recv_desc, name) {
                let (params, ret) = parse_method_desc(&c.descriptor);
                // params[0] is the receiver (keyed by `recv_desc`); the rest are the call arguments.
                if params.len() != args.len() + 1 {
                    continue;
                }
                if !params[1..].iter().zip(args).all(|(p, a)| arg_fits(p, a)) {
                    continue;
                }
                // Recover a generic extension's parameterized return (`to` → `Pair<A, B>`): the
                // type variables bind from the receiver (the first parameter) and the arguments.
                let ret_ty = c.signature.as_ref().and_then(|sig| parse_method_gsig(sig)).map(|(formals, psigs, rsig)| {
                    let mut binds = std::collections::HashMap::new();
                    // Explicit type arguments bind any formals the receiver/value args don't determine.
                    for (f, t) in formals.iter().zip(type_args) {
                        binds.insert(f.clone(), *t);
                    }
                    let actuals: Vec<Ty> = std::iter::once(receiver).chain(args.iter().copied()).collect();
                    for (ps, a) in psigs.iter().zip(&actuals) {
                        unify_gsig(ps, *a, &mut binds);
                    }
                    gsig_to_ty(&rsig, &binds)
                }).unwrap_or(ret);
                return Some(LibraryCallable { owner: c.owner.clone(), name: c.name.clone(), params, ret: ret_ty, physical_ret: ret, descriptor: c.descriptor.clone(), default_call: false });
            }
        }
        // No exact-arity match — try the `name$default` synthetic for an extension with default
        // parameters (`list.joinToString(",")` → `joinToString$default(list, ",", …, mask, null)`).
        // Its descriptor is `(recv, real…, int mask, Object marker)ret`; the call fills a prefix of the
        // real parameters, the backend defaults the rest.
        // A trailing lambda binds to the *last* function parameter (not a prefix), which interacts with
        // defaulted middle parameters in a way the prefix-fill below doesn't model — leave those calls
        // unresolved (the file skips) rather than risk a wrong argument placement.
        if args.last().map_or(false, |a| matches!(a, Ty::Fun(_))) {
            return None;
        }
        let default_name = format!("{name}$default");
        for recv_desc in supertype_descriptors(&self.cp, receiver) {
            for c in self.cp.find_extensions(&recv_desc, &default_name) {
                let (params, ret) = parse_method_desc(&c.descriptor);
                if params.len() < 3 {
                    continue; // need at least receiver + mask + marker
                }
                let real_count = params.len() - 3; // exclude receiver, int mask, Object marker
                // The provided arguments fill a prefix of the real parameters; each must fit its
                // parameter (subtype-aware) so a wrong overload (`contains(CharSequence)` for a `Char`
                // argument) is rejected rather than miscompiled.
                if args.len() > real_count {
                    continue;
                }
                if !params[1..1 + args.len()].iter().zip(args).all(|(p, a)| arg_fits_subtype(&self.cp, p, a)) {
                    continue;
                }
                // Keep the receiver + real parameters (drop the trailing mask + marker), like the
                // non-`$default` case — the backend appends the placeholders, mask, and marker.
                let kept: Vec<Ty> = params[..params.len() - 2].to_vec();
                let ret_ty = c.signature.as_ref().and_then(|sig| parse_method_gsig(sig)).map(|(formals, psigs, rsig)| {
                    let mut binds = std::collections::HashMap::new();
                    for (f, t) in formals.iter().zip(type_args) {
                        binds.insert(f.clone(), *t);
                    }
                    // psigs for `$default` are `[recv, real…, int, Object]`; unify the receiver + provided.
                    let actuals: Vec<Ty> = std::iter::once(receiver).chain(args.iter().copied()).collect();
                    for (ps, a) in psigs.iter().zip(&actuals) {
                        unify_gsig(ps, *a, &mut binds);
                    }
                    gsig_to_ty(&rsig, &binds)
                }).unwrap_or(ret);
                return Some(LibraryCallable { owner: c.owner.clone(), name: c.name.clone(), params: kept, ret: ret_ty, physical_ret: ret, descriptor: c.descriptor.clone(), default_call: true });
            }
        }
        None
    }
}

//! The JVM implementation of the [`LibrarySet`] abstraction: resolves symbols from a `.class`-jar
//! classpath (the bytecode target). All classpath reads, JVM method-descriptor parsing, and
//! `java/lang ↔ kotlin` name normalization live here — the front end (`resolve`, `ir_lower`) sees
//! only Kotlin-level `Ty`s and opaque descriptor tokens through the trait.

use super::classpath::Classpath;
use super::jvm_class_map::{
    kotlin_builtin_to_internal, to_jvm_internal, to_kotlin_internal, BUILTIN_MAPPED_NAMES,
};
use crate::call_resolver::{arg_fits, function_input_types, gsig_to_ty, unify_gsig, GSig};
use crate::libraries::{
    FnFlags, FnKind, FunctionInfo, FunctionSet, InlineKind, LibraryCallable, LibraryMember,
    LibrarySeed, LibrarySet, LibraryType,
};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

/// A platform backed by a JVM classpath (dirs + jars + the JDK jimage). The classpath is shared
/// (`Rc`) with the JVM backend/emitter so the bytecode inliner reads inline-function bodies through
/// the same lazily-populated caches — all within the `jvm` module, never through the `LibrarySet`
/// abstraction.
pub struct JvmLibraries {
    cp: std::rc::Rc<Classpath>,
}

impl JvmLibraries {
    pub fn new(cp: std::rc::Rc<Classpath>) -> JvmLibraries {
        JvmLibraries { cp }
    }

    /// Whether reference argument `arg`'s erased class is `param`'s erased class or a classpath subtype
    /// of it — `KSerializer<Foo>` is assignable to a `DeserializationStrategy<…>` parameter.
    fn erased_subtype(&self, arg: &Ty, param: &Ty) -> bool {
        match (arg.obj_internal(), param.obj_internal()) {
            (Some(a), Some(p)) => self.is_cp_subtype(a, p, 0),
            _ => false,
        }
    }

    /// `sub` equals or transitively extends/implements `super_` on the classpath. Depth-bounded to
    /// terminate on a malformed (cyclic) hierarchy.
    fn is_cp_subtype(&self, sub: &str, super_: &str, depth: u32) -> bool {
        if sub == super_ {
            return true;
        }
        if depth > 64 {
            return false;
        }
        match self.cp.find(sub) {
            Some(ci) => ci
                .interfaces
                .iter()
                .chain(ci.super_class.iter())
                .any(|s| self.is_cp_subtype(s, super_, depth + 1)),
            None => false,
        }
    }

    /// The erased JVM descriptor of a classpath value class's underlying (`kotlin/UInt` → `"I"`,
    /// `kotlin/Result` → `"Ljava/lang/Object;"`), or `None` if `internal` is not a value class. Its
    /// mangled extensions are indexed under this descriptor.
    fn value_class_underlying_desc(&self, internal: &str) -> Option<String> {
        let ic = self
            .cp
            .find(internal)
            .and_then(|ci| crate::jvm::metadata::class_inline(&ci))?;
        Some(match ic.underlying_class.as_deref() {
            Some("kotlin/Boolean") => "Z".into(),
            Some("kotlin/Byte") => "B".into(),
            Some("kotlin/Short") => "S".into(),
            Some("kotlin/Int") => "I".into(),
            Some("kotlin/Long") => "J".into(),
            Some("kotlin/Char") => "C".into(),
            Some("kotlin/Float") => "F".into(),
            Some("kotlin/Double") => "D".into(),
            Some(other) => format!("L{other};"),
            None => "Ljava/lang/Object;".into(),
        })
    }

    /// The logical Kotlin return type of a value-class extension identified by its MANGLED JVM method name
    /// (`coerceAtMost-J1ME1BU` → `UInt`), from `@Metadata` (facade parts merged). The descriptor return is
    /// the erased underlying; this recovers the unsigned/value-class type. `None` if not found.
    fn metadata_ext_return_ty(&self, owner: &str, jvm_name: &str) -> Option<Ty> {
        let rc = self
            .cp
            .meta_functions(owner)
            .iter()
            .find(|m| m.jvm_name == jvm_name)?
            .ret_class
            .clone()?;
        Some(match rc.as_str() {
            "kotlin/Boolean" => Ty::Boolean,
            "kotlin/Byte" => Ty::Byte,
            "kotlin/Short" => Ty::Short,
            "kotlin/Int" => Ty::Int,
            "kotlin/Long" => Ty::Long,
            "kotlin/Char" => Ty::Char,
            "kotlin/Float" => Ty::Float,
            "kotlin/Double" => Ty::Double,
            "kotlin/UInt" => Ty::UInt,
            "kotlin/ULong" => Ty::ULong,
            other => Ty::obj(other),
        })
    }

    /// Whether a value-class argument `a` (`3u: UInt`) fits a parameter `p` that is the value class's
    /// ERASED underlying (`Int`) — a mangled value-class extension carries erased params in its descriptor.
    fn value_class_arg_fits(&self, p: &Ty, a: &Ty) -> bool {
        let under_ty = match a {
            Ty::UInt => Ty::Int,
            Ty::ULong => Ty::Long,
            Ty::Obj(internal, _) => {
                let under = self
                    .cp
                    .find(internal)
                    .and_then(|ci| crate::jvm::metadata::class_inline(&ci))
                    .and_then(|ic| ic.underlying_class);
                match under.as_deref() {
                    Some("kotlin/Boolean") => Ty::Boolean,
                    Some("kotlin/Byte") => Ty::Byte,
                    Some("kotlin/Short") => Ty::Short,
                    Some("kotlin/Int") => Ty::Int,
                    Some("kotlin/Long") => Ty::Long,
                    Some("kotlin/Char") => Ty::Char,
                    Some("kotlin/Float") => Ty::Float,
                    Some("kotlin/Double") => Ty::Double,
                    Some(other) => Ty::obj(other),
                    None => return false,
                }
            }
            _ => return false,
        };
        *p == under_ty
    }

    /// Resolve an extension `receiver.name(args)` to a `LibraryCallable` (exact-arity match, generic
    /// return recovered from the signature). `allow_non_public` includes `@InlineOnly` package-private
    /// candidates — used ONLY by the inline route (which splices, emitting no call); normal resolution
    /// passes `false`, so it never resolves a non-callable method (an `IllegalAccessError`).
    fn extension_callable(
        &self,
        name: &str,
        receiver: Ty,
        args: &[Ty],
        type_args: &[Ty],
        allow_non_public: bool,
    ) -> Option<LibraryCallable> {
        // Enumerate candidates through the consolidated `functions` query (the single source of truth for
        // every overload + its metadata) instead of hitting the classpath index directly. Each Extension
        // overload carries the receiver-MRO RUNG (`receiver_rank`) it was found at; walking the rungs
        // most-specific-first and returning the first with a match reproduces the classpath lookup's
        // receiver precedence (a `List` extension before an `Iterable` one) — `recv_desc` for each rung is
        // recovered locally from the same supertype walk, so no JVM descriptor leaks into `FunctionInfo`.
        let descs = supertype_descriptors(&self.cp, receiver);
        let fs = self.functions(name, Some(receiver));
        let exts: Vec<&FunctionInfo> = fs
            .overloads
            .iter()
            .filter(|o| o.kind == FnKind::Extension)
            .collect();
        for (rank, recv_desc) in descs.iter().enumerate() {
            let rank = rank as u32;
            // Collect every candidate at THIS rung that fits the arguments, then pick the MOST SPECIFIC by
            // its parameter types — `Iterable.plus(element: T)` and `Iterable.plus(elements: Iterable<T>)`
            // both accept a `List` argument (the first via the erased `Object` parameter), but Kotlin selects
            // the more specific `Iterable` overload. Without this, first-match would resolve `list + list` to
            // the element overload (a nested list).
            let mut matches: Vec<(&FunctionInfo, Vec<Ty>, Ty)> = Vec::new();
            for o in exts.iter().copied().filter(|o| o.receiver_rank == rank) {
                let c = &o.callable;
                if !o.public && !allow_non_public {
                    continue;
                }
                // A non-public candidate matched via the ERASED `Object` key must have a type-variable
                // receiver (`T.takeIf`) — a concrete value-class receiver (`Result.map`, erased to
                // `Object`) must not match an unrelated receiver this way. (A concrete non-value-class
                // receiver keys under its own descriptor, so this only affects the `Object` key.)
                if !o.public
                    && recv_desc == "Ljava/lang/Object;"
                    && !nonpublic_ext_receiver_is_typevar(c.signature.as_deref())
                {
                    continue;
                }
                // Kotlin-receiver applicability: the candidate matched on the JVM-erased lookup key, but
                // the read-only/mutable distinction survives only in Kotlin types. When the receiver is a
                // Kotlin collection type, consult this name's `@Metadata` receiver types: among those that
                // are themselves Kotlin collection types, the receiver must be a subtype of at least one.
                // A name is overloaded across receivers (`plus` on `Collection`/`Map`/`Set`), so "any" is
                // correct — `list + x` keeps the `Collection.plus` overload, while `MutableCollection.
                // plusAssign` (receivers all `Mutable*`) has NONE applicable to a read-only `List`, so it
                // is rejected and `list += x` falls through to `list = list.plus(x)`. Exactly kotlinc's
                // overload resolution; no erased type makes the decision.
                if let Ty::Obj(recv_internal, _) = &receiver {
                    if self.cp.is_kotlin_collection(recv_internal) {
                        let krs = self.cp.metadata_receiver_types(&c.owner, &c.name);
                        let coll: Vec<&String> = krs
                            .iter()
                            .filter(|kr| self.cp.is_kotlin_collection(kr))
                            .collect();
                        if !coll.is_empty()
                            && !coll
                                .iter()
                                .any(|kr| self.cp.kotlin_subtype(recv_internal, kr))
                        {
                            continue;
                        }
                    }
                }
                let (params, ret) = parse_method_desc(&c.descriptor);
                // params[0] is the receiver (keyed by `recv_desc`); the rest are the call arguments.
                if params.len() != args.len() + 1 {
                    continue;
                }
                // Subtype-aware fit so a `List` argument matches an `Iterable` parameter (`list + list`
                // selects the `Iterable` concat overload); the most-specific pick below then disambiguates
                // against the erased-`Object` element overload. A value-class argument (`3u: UInt`) also fits
                // a parameter of its ERASED underlying (`Int`), since a mangled value-class extension's
                // descriptor carries the erased params (`coerceAtMost-<hash>(II)`).
                if !params[1..].iter().zip(args).all(|(p, a)| {
                    arg_fits_subtype(&self.cp, p, a) || self.value_class_arg_fits(p, a)
                }) {
                    continue;
                }
                // Disambiguate by the receiver's type arguments: reject an overload whose declared
                // receiver type argument conflicts (`Iterable<Double>.maxOrNull` for a `List<Int>`).
                if !receiver.type_args().is_empty() {
                    if let Some((_, psigs, _)) =
                        c.signature.as_ref().and_then(|sig| parse_method_gsig(sig))
                    {
                        if let Some(recv_sig) = psigs.first() {
                            if !sig_compatible(recv_sig, receiver) {
                                continue;
                            }
                        }
                    }
                }
                matches.push((o, params, ret));
            }
            if matches.is_empty() {
                continue;
            }
            // krusty collapses `Byte`/`Short`/`Int` → `Ty::Int`, so numeric overloads differing only in a
            // `Byte`/`Short` vs `Int` parameter (`until(Int,Byte)` vs `until(Int,Int)`) are
            // indistinguishable here. Prefer the WIDEST (fewest narrowing params): kotlinc resolves an
            // `Int` argument to the `Int` overload, and only that one carries the `MIN_VALUE`/`MAX_VALUE`
            // overflow guard (`2 until Int.MIN_VALUE` must be empty, not wrap to `2..MAX_VALUE`).
            matches.sort_by_key(|(o, _, _)| descriptor_narrowing(&o.callable.descriptor));
            // Pick the candidate whose non-receiver parameters are at least as specific as every other's
            // (each parameter a subtype of the corresponding one). When two are incomparable, keep the
            // first — stable, and good enough for the stdlib's overload sets.
            let specific_over = |a: &[Ty], b: &[Ty]| -> bool {
                a.iter()
                    .zip(b)
                    .all(|(pa, pb)| arg_fits_subtype(&self.cp, pb, pa))
            };
            let best = (0..matches.len())
                .find(|&i| {
                    (0..matches.len())
                        .all(|j| j == i || specific_over(&matches[i].1[1..], &matches[j].1[1..]))
                })
                .unwrap_or(0);
            let (o, params, ret) = matches.swap_remove(best);
            let c = &o.callable;
            // Recover a generic extension's parameterized return (`to` → `Pair<A, B>`): the type variables
            // bind from the receiver (the first parameter) and the arguments.
            let ret_ty = c
                .signature
                .as_ref()
                .and_then(|sig| parse_method_gsig(sig))
                .map(|(formals, psigs, rsig)| {
                    let mut binds = std::collections::HashMap::new();
                    for (f, t) in formals.iter().zip(type_args) {
                        binds.insert(f.clone(), *t);
                    }
                    let actuals: Vec<Ty> = std::iter::once(receiver)
                        .chain(args.iter().copied())
                        .collect();
                    for (ps, a) in psigs.iter().zip(&actuals) {
                        unify_gsig(ps, *a, &mut binds);
                    }
                    gsig_to_ty(&rsig, &binds)
                })
                .unwrap_or(ret);
            // A nullable Kotlin return (`takeIf`/`takeUnless`: `T?`) over a PRIMITIVE receiver is the
            // first-class `Ty::Nullable(prim)` (the JVM signature drops nullability; `@Metadata` keeps it).
            // Keeping it nullable preserves a `?:`/null-check on it (a bare primitive `Ty` is never-null and
            // would fold the elvis away, then unbox a possibly-null value → NPE); the emit boxes it.
            let ret_ty =
                if ret_ty.is_primitive() && self.cp.metadata_return_nullable(&c.owner, &c.name) {
                    crate::types::Ty::nullable(ret_ty)
                } else {
                    ret_ty
                };
            // A mangled value-class extension's descriptor return is the ERASED underlying (`coerceAtMost-
            // <hash>(II)I`); recover the LOGICAL value-class return from `@Metadata` (by the mangled JVM
            // name, part-merged) so `b: UInt`, not `Int`. Only override TO a value-class type (leaves an
            // ordinary extension untouched).
            let ret_ty = match self.metadata_ext_return_ty(&c.owner, &c.name) {
                Some(mt)
                    if matches!(mt, Ty::UInt | Ty::ULong)
                        || matches!(&mt, Ty::Obj(i, _) if self.value_class_underlying_desc(i).is_some()) =>
                {
                    mt
                }
                _ => ret_ty,
            };
            return Some(LibraryCallable {
                owner: c.owner.clone(),
                name: c.name.clone(),
                params,
                ret: ret_ty,
                physical_ret: ret,
                descriptor: c.descriptor.clone(),
                // A NON-public (`@InlineOnly`) extension has no callable method, so a failed splice must
                // skip the file (never an `IllegalAccessError`) → `MustInline`; a PUBLIC one can fall back
                // to a real call.
                inline: InlineKind::from_flags(
                    self.cp.is_inline_method(&c.owner, &c.name),
                    !o.public,
                ),
                default_call: false,
                vararg_elem: None,
                signature: c.signature.clone(),
                origin: crate::libraries::Origin::Library,
            });
        }
        None
    }

    /// If `owner.name`'s `@Metadata` return type is a Kotlin collection interface (`mutableListOf` →
    /// `kotlin/collections/MutableList`), rebuild the logical `ret` with that class (keeping the element
    /// type arguments recovered from the JVM signature). The JVM signature erased it to `java/util/List`,
    /// dropping the read-only/mutable distinction; this restores it. Non-collection returns are unchanged.
    fn meta_collection_ret(&self, owner: &str, name: &str, ret: Ty) -> Ty {
        if let Some(meta) = self.cp.metadata_return_type(owner, name) {
            if meta.starts_with("kotlin/collections/") {
                return Ty::obj_args(&meta, ret.type_args());
            }
        }
        ret
    }
}

/// Count the `Byte`/`Short` primitive parameters in a JVM method descriptor — the "narrowing" measure
/// used to prefer the widest among overloads krusty's `Byte`/`Short`/`Int` → `Int` collapse made
/// indistinguishable. Object (`L…;`) and array (`[`) params are skipped (a `B`/`S` inside a class name
/// must not count).
fn descriptor_narrowing(desc: &str) -> usize {
    let end = desc.find(')').unwrap_or(desc.len());
    let params = desc.get(1..end).unwrap_or("");
    let b = params.as_bytes();
    let mut i = 0;
    let mut n = 0;
    while i < b.len() {
        match b[i] {
            b'L' => {
                while i < b.len() && b[i] != b';' {
                    i += 1;
                }
                i += 1;
            }
            b'[' => i += 1,
            b'B' | b'S' => {
                n += 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    n
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
        s if s.starts_with('L') && s.ends_with(';') => {
            Ty::obj(to_kotlin_internal(&s[1..s.len() - 1]))
        }
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

/// Whether a non-public (`@InlineOnly`) extension's generic-signature RECEIVER is a type variable
/// (`<T> T.takeIf(…)`) — the scope-fn family that applies to ANY receiver. A concrete-class receiver
/// (a value class like `Result.map`, erased to `Object`) would otherwise wrongly match an unrelated
/// receiver through the erased lookup key, so only a type-variable receiver may match this way.
/// The Kotlin simple type name of a numeric primitive `Ty` (`Int` → `"Int"`), used to derive the
/// `@OverloadResolutionByLambdaReturnType` `@JvmName` (`sumOf` + `Int` → `sumOfInt`). `None` for unsigned
/// (`UInt`/`ULong`) and non-numeric types — krusty can't model an unsigned `sumOf` result, so it bails.
fn kotlin_simple_name_of_ty(t: Ty) -> Option<&'static str> {
    Some(match t {
        Ty::Int => "Int",
        Ty::Long => "Long",
        Ty::Double => "Double",
        Ty::Float => "Float",
        Ty::Byte => "Byte",
        Ty::Short => "Short",
        _ => return None,
    })
}

fn nonpublic_ext_receiver_is_typevar(signature: Option<&str>) -> bool {
    signature
        .and_then(parse_method_gsig)
        .is_some_and(|(_, psigs, _)| matches!(psigs.first(), Some(GSig::Var(_))))
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
                    let r2 = rest
                        .strip_prefix('+')
                        .or_else(|| rest.strip_prefix('-'))
                        .unwrap_or(rest);
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
    let Some(rest) = s.strip_prefix('<') else {
        return (Vec::new(), s);
    };
    let mut depth = 1;
    let bytes = rest.as_bytes();
    let mut i = 0;
    let mut at_name_start = true;
    let mut formals = Vec::new();
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'<' => {
                depth += 1;
                at_name_start = false;
            }
            b'>' => {
                depth -= 1;
            }
            b':' => {
                at_name_start = false;
            }
            _ if depth == 1 && at_name_start => {
                let start = i;
                while i < bytes.len() && bytes[i] != b':' {
                    i += 1;
                }
                formals.push(rest[start..i].to_string());
                at_name_start = false;
                continue;
            }
            b';' if depth == 1 => {
                at_name_start = true;
            }
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

/// Parameter indices whose descriptor type is a `kotlin/jvm/functions/FunctionN` — the lambda parameters
/// the unified splicer inlines (`require`'s `lazyMessage: () -> Any`, `let`'s `block: (T) -> R`, …).
fn function_param_indices(descriptor: &str) -> Vec<usize> {
    let Some(inner) = descriptor
        .strip_prefix('(')
        .and_then(|s| s.split(')').next())
    else {
        return Vec::new();
    };
    let b = inner.as_bytes();
    let mut i = 0;
    let mut idx = 0;
    let mut out = Vec::new();
    while i < b.len() {
        match b[i] {
            b'L' => {
                let start = i;
                while i < b.len() && b[i] != b';' {
                    i += 1;
                }
                if inner[start + 1..i].starts_with("kotlin/jvm/functions/Function") {
                    out.push(idx);
                }
                i += 1;
                idx += 1;
            }
            b'[' => {
                while i < b.len() && b[i] == b'[' {
                    i += 1;
                }
                if i < b.len() && b[i] == b'L' {
                    while i < b.len() && b[i] != b';' {
                        i += 1;
                    }
                }
                i += 1;
                idx += 1;
            }
            _ => {
                i += 1;
                idx += 1;
            }
        }
    }
    out
}

/// Whether a parameter signature node is compatible with an actual `Ty`, used to disambiguate
/// overloads by the receiver's type arguments — `Iterable<Double>` is rejected for a `List<Int>`
/// receiver while `Iterable<T>` (a type variable) and `Iterable<Int>` are accepted. A type variable
/// accepts anything; a concrete class type-argument must match the actual's (a primitive matches only
/// its boxed wrapper). Conservative: anything it can't compare is accepted.
fn sig_compatible(sig: &GSig, actual: Ty) -> bool {
    match sig {
        GSig::Var(_) => true,
        GSig::Prim(t) => *t == actual,
        GSig::Arr(inner) => actual
            .array_elem()
            .map_or(true, |e| sig_compatible(inner, e)),
        GSig::Class(name, args) => match actual {
            Ty::Obj(_, targs) => args
                .iter()
                .zip(targs.iter())
                .all(|(s, t)| sig_compatible(s, *t)),
            t if t.is_primitive() => {
                name == "kotlin/Any"
                    || super::jvm_class_map::wrapper_internal(t).map_or(false, |w| w == name)
            }
            _ => true,
        },
    }
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
/// The LOGICAL descriptor of a `suspend fun`'s physical CPS method: drop the trailing
/// `kotlin/coroutines/Continuation` parameter kotlinc appends (`(ILkotlin/coroutines/Continuation;)…`
/// → `(I)…`). The return stays erased (`Object`); the *logical* Kotlin return lives in `@Metadata`. A
/// suspend callee is resolved by this logical signature; the coroutine pass re-derives the CPS form for
/// the emitted call. A no-op if the descriptor has no trailing continuation (not a CPS method).
fn strip_continuation_param(desc: &str) -> String {
    const CONT: &str = "Lkotlin/coroutines/Continuation;";
    if let Some(close) = desc.rfind(')') {
        if let Some(stripped) = desc[1..close].strip_suffix(CONT) {
            return format!("({}){}", stripped, &desc[close + 1..]);
        }
    }
    desc.to_string()
}

pub(crate) fn parse_method_desc(desc: &str) -> (Vec<Ty>, Ty) {
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

impl SymbolSource for JvmLibraries {
    fn seed(&self) -> LibrarySeed {
        let (class_names, type_aliases) = self.seed_shared();
        LibrarySeed {
            class_names: (*class_names).clone(),
            type_aliases: (*type_aliases).clone(),
        }
    }

    fn seed_shared(&self) -> crate::symbol_source::SharedSeed {
        // The merged class-name map (classpath index + the ported built-in mapping) is identical for
        // every file compiled against this classpath, so build it ONCE per (thread, classpath) and hand
        // back a shared `Rc`. Cloning this ~40k-entry map per file was the dominant `sigs` cost.
        thread_local! {
            static CACHE: std::cell::RefCell<std::collections::HashMap<u64, crate::symbol_source::SharedSeed>> =
                std::cell::RefCell::new(std::collections::HashMap::new());
        }
        // Key on the classpath's STABLE process-unique id — NOT the `Rc` pointer address, which a
        // freed-then-reallocated `Classpath` can reuse, serving a stale seed for a different classpath
        // (manifested as a cross-module class going unresolved after a prior compile in the same process).
        let key = self.cp.id();
        if let Some(hit) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
            return hit;
        }
        let idx = self.cp.scan_types();
        let mut class_names = idx.class_names.clone();
        // Seed the Kotlin built-in → JVM class mapping (ported `JavaToKotlinClassMap`): intrinsic
        // mapped types (`Comparable`, `Throwable`, `List`, …), not `.class` files. Classpath types
        // above take precedence (`or_insert`).
        for name in BUILTIN_MAPPED_NAMES {
            if let Some(internal) = kotlin_builtin_to_internal(name) {
                if internal.starts_with("kotlin/collections/") {
                    // FORCE the Kotlin collection type (read-only vs mutable) over any classpath
                    // `java/util/List` — the front end must keep the distinction; emit erases it.
                    class_names.insert(name.to_string(), internal.to_string());
                } else {
                    class_names
                        .entry(name.to_string())
                        .or_insert_with(|| internal.to_string());
                }
            }
        }
        // `Pair`/`Triple` are auto-imported `kotlin.*` classes constructed directly (`Pair(a, b)`), but
        // the classpath scan indexes them by FQ name only (they're otherwise reached via `to`), so seed
        // the simple-name → internal mapping (classpath entries above still take precedence).
        for (name, internal) in [("Pair", "kotlin/Pair"), ("Triple", "kotlin/Triple")] {
            class_names
                .entry(name.to_string())
                .or_insert_with(|| internal.to_string());
        }
        let pair = (
            std::rc::Rc::new(class_names),
            std::rc::Rc::new(idx.type_aliases.clone()),
        );
        CACHE.with(|c| c.borrow_mut().insert(key, pair.clone()));
        pair
    }

    fn resolve_type(&self, internal: &str) -> Option<LibraryType> {
        // A Kotlin MAPPED type (`kotlin.collections.List`, `kotlin.CharSequence`, …) has no own JVM
        // `.class` — its *actual* platform declaration IS a JVM type (`java/util/List`), exactly the
        // `expect`/`actual` + `JavaToKotlinClassMap` device kotlinc uses. When the classpath has no class
        // for the Kotlin name, resolve members against that mapped (actual) type — the SAME generic
        // mapping (`to_jvm_internal`) the emitter uses for the call owner, so resolution and codegen stay
        // byte-consistent. Members/return types erase to the JVM forms (`get(int)Object`, etc.).
        let ci = match self.cp.find(internal) {
            Some(ci) => ci,
            None => {
                let mapped = super::jvm_class_map::to_jvm_internal(internal);
                if mapped == internal {
                    return None;
                }
                self.cp.find(mapped)?
            }
        };
        let mut constructors = Vec::new();
        let mut members = Vec::new();
        let mut companion = Vec::new();
        for m in &ci.methods {
            // Only public members are callable from generated code.
            if !m.is_public() {
                continue;
            }
            let (params, ret) = parse_method_desc(&m.descriptor);
            let member = LibraryMember {
                name: m.name.clone(),
                params,
                ret,
                descriptor: m.descriptor.clone(),
            };
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
            constructors.push(LibraryMember {
                name: "<init>".into(),
                params: vec![],
                ret: Ty::Unit,
                descriptor: "()V".into(),
            });
            constructors.push(LibraryMember {
                name: "<init>".into(),
                params: vec![Ty::String],
                ret: Ty::Unit,
                descriptor: format!("({})V", Ty::String.descriptor()),
            });
        }
        let mut supertypes = ci.interfaces.clone();
        if let Some(s) = &ci.super_class {
            supertypes.push(s.clone());
        }
        // A companion object compiles to a `public static final C$Name` field on `C` (default name
        // `Companion`; e.g. `Json.Default: Json$Default`). Detect it by the descriptor pattern
        // `L<this>$<fieldname>;` so a bare `C` reference can resolve to the companion instance.
        let companion_object = ci.fields.iter().find_map(|f| {
            // A Kotlin companion-object instance field is always `public static final`, typed as the
            // nested companion class (`L<this>$<fieldname>;`). Requiring all three flags + the nested-
            // type-name pattern makes a false positive on a hand-authored non-Kotlin static field
            // (a nested-class-typed `public static final` field) vanishingly unlikely.
            let public_static_final =
                f.access & (0x0001 | 0x0008 | 0x0010) == (0x0001 | 0x0008 | 0x0010);
            if !public_static_final {
                return None;
            }
            let nested = format!("{internal}${}", f.name);
            (f.descriptor == format!("L{nested};")).then(|| (f.name.clone(), nested))
        });
        // A Kotlin `object` has a `public static final INSTANCE` field of its own type.
        let self_desc = format!("L{internal};");
        let is_object = ci.fields.iter().any(|f| {
            f.name == "INSTANCE" && f.descriptor == self_desc && f.access & 0x0008 != 0
            // ACC_STATIC
        });
        let kind = if ci.access & 0x2000 != 0 {
            crate::libraries::TypeKind::Annotation
        } else if ci.is_interface() {
            crate::libraries::TypeKind::Interface
        } else if is_object {
            crate::libraries::TypeKind::Object
        } else {
            crate::libraries::TypeKind::Class
        };
        // A classpath `@JvmInline value class` (detected via `@Metadata`): its erased underlying type, so
        // the JVM backend can unbox it like a user value class. `UInt` → `Int`, `Result` → `Any`.
        let value_underlying = crate::jvm::metadata::class_inline(&ci).map(|ic| {
            match ic.underlying_class.as_deref() {
                Some("kotlin/Boolean") => Ty::Boolean,
                Some("kotlin/Byte") => Ty::Byte,
                Some("kotlin/Short") => Ty::Short,
                Some("kotlin/Int") => Ty::Int,
                Some("kotlin/Long") => Ty::Long,
                Some("kotlin/Char") => Ty::Char,
                Some("kotlin/Float") => Ty::Float,
                Some("kotlin/Double") => Ty::Double,
                Some(other) => Ty::obj(other),
                None => Ty::obj("kotlin/Any"),
            }
        });
        Some(LibraryType {
            is_public: ci.is_public(),
            kind,
            supertypes,
            constructors,
            members,
            companion,
            companion_object,
            value_underlying,
        })
    }

    fn functions(&self, name: &str, receiver: Option<Ty>) -> FunctionSet {
        let mut overloads = Vec::new();
        // Slice 1 of the `FunctionSet` consolidation: the `@OverloadResolutionByLambdaReturnType` family
        // (`sumOf` → `sumOfInt`/`sumOfLong`/…). One query returns every numeric-return overload applicable
        // to `receiver`, each as a `FunctionInfo` with its real callable + flags; the caller picks by the
        // lambda's return type. (Plain extensions, members, and top-level functions migrate here next.)
        if let Some(receiver) = receiver {
            // The receiver's ELEMENT type — the selector's `it`. Distinguishes `IntArray.sumOf` (`(Int)->R`)
            // from `UIntArray.sumOf` (`(UInt)->R`), which erase to the same `([I, Function1)` descriptor.
            let want_elem = receiver
                .array_elem()
                .or_else(|| receiver.type_args().first().copied());
            for recv_desc in supertype_descriptors(&self.cp, receiver) {
                for owner in self.cp.find_extension_owners(&recv_desc) {
                    // Gate: `name` genuinely resolves by lambda return type on this facade.
                    if !self.cp.lambda_return_overloads(&owner).contains_key(name) {
                        continue;
                    }
                    // The `@JvmName`-mangled method is `name` + the return type's simple name (`sumOf` +
                    // `Int` → `sumOfInt`); DERIVE it per numeric return and VERIFY against the real method.
                    for ret in [
                        Ty::Int,
                        Ty::Long,
                        Ty::Double,
                        Ty::Float,
                        Ty::Byte,
                        Ty::Short,
                    ] {
                        let Some(simple) = kotlin_simple_name_of_ty(ret) else {
                            continue;
                        };
                        let jname = format!("{name}{simple}");
                        for c in self.cp.find_extensions(&recv_desc, &jname) {
                            let (params, pret) = parse_method_desc(&c.descriptor);
                            // A single-selector overload whose receiver is THIS supertype and whose JVM
                            // return is the wanted primitive.
                            if params.len() != 2
                                || !c.descriptor.contains("Lkotlin/jvm/functions/Function")
                                || params.first().map(|p| p.descriptor()).as_deref()
                                    != Some(recv_desc.as_str())
                                || pret.descriptor() != ret.descriptor()
                            {
                                continue;
                            }
                            // Disambiguate `IntArray.sumOf` from `UIntArray.sumOf` (both erase to
                            // `([I, Function1)I`) by the SELECTOR parameter type from the generic signature
                            // == the receiver's element type — so an `Int` lambda never binds a `UInt` body.
                            if let Some(elem) = want_elem {
                                let Some((_, psigs, _)) =
                                    c.signature.as_deref().and_then(parse_method_gsig)
                                else {
                                    continue;
                                };
                                let mut binds = std::collections::HashMap::new();
                                if let Some(recv_sig) = psigs.first() {
                                    unify_gsig(recv_sig, receiver, &mut binds);
                                }
                                let selector_matches = psigs
                                    .get(1)
                                    .map(|sel| function_input_types(sel, &binds) == vec![elem])
                                    .unwrap_or(false);
                                if !selector_matches {
                                    continue;
                                }
                            }
                            overloads.push(FunctionInfo {
                                kind: FnKind::Extension,
                                receiver: Some(receiver),
                                ret_nullable: false,
                                public: c.public,
                                // The lambda-return family is resolved by return type, never through the
                                // arg-binding extension selector — mark it so it can't preempt a real rung.
                                receiver_rank: u32::MAX,
                                call_sig: crate::libraries::CallSig::default(),
                                flags: FnFlags {
                                    inline: InlineKind::from_flags(true, !c.public),
                                    suspend: self.cp.is_suspend_method(&c.owner, &c.name),
                                },
                                callable: LibraryCallable {
                                    name: c.name.clone(),
                                    owner: c.owner.clone(),
                                    params,
                                    ret,
                                    physical_ret: pret,
                                    descriptor: c.descriptor.clone(),
                                    // Package-private `@InlineOnly` — splice or skip, never `invokestatic`.
                                    inline: InlineKind::from_flags(true, !c.public),
                                    default_call: false,
                                    vararg_elem: None,
                                    signature: c.signature.clone(),
                                    origin: crate::libraries::Origin::Library,
                                },
                            });
                        }
                    }
                }
            }
            // Plain extensions of `name` on the receiver (and supertypes) — `uppercase`, `map`, `let`, … —
            // with their inline/`@InlineOnly` flags and return nullability decoded once. The enumeration
            // index is the receiver-MRO rung (`receiver_rank`) the arg-binding selector orders candidates by.
            for (rank, recv_desc) in supertype_descriptors(&self.cp, receiver)
                .into_iter()
                .enumerate()
            {
                for c in self.cp.find_extensions(&recv_desc, name) {
                    // A value-class receiver erases to a primitive descriptor (`UInt`→`"I"`), so a SIGNED
                    // primitive extension (`Int.coerceAtMost`) matches at the erased rung. Reject a
                    // candidate whose `@Metadata` receivers are concrete and EXCLUDE this value class (it is
                    // not one of them, nor a subtype) — only a `UInt`-declared (or generic, no recorded
                    // receiver) extension applies to a `UInt`. Mirrors the collection applicability check.
                    let recv_vc = match &receiver {
                        Ty::UInt => Some("kotlin/UInt".to_string()),
                        Ty::ULong => Some("kotlin/ULong".to_string()),
                        Ty::Obj(i, _) if self.value_class_underlying_desc(i).is_some() => {
                            Some(i.to_string())
                        }
                        _ => None,
                    };
                    if let Some(vc) = &recv_vc {
                        let recvs = self.cp.metadata_receiver_types(&c.owner, &c.name);
                        if !recvs.is_empty()
                            && !recvs
                                .iter()
                                .any(|r| r == vc || self.cp.kotlin_subtype(vc, r))
                        {
                            continue;
                        }
                    }
                    let (params, pret) = parse_method_desc(&c.descriptor);
                    let inline = self.cp.is_inline_method(&c.owner, &c.name);
                    let ret_nullable = self.cp.metadata_return_nullable(&c.owner, &c.name);
                    // Metadata-primary visibility for a value-class extension. An `inline` extension on a
                    // value class (`Result.getOrThrow`) is PRIVATE in bytecode but PUBLIC per @Metadata —
                    // kotlinc resolves it, then inlines (no legal `invokestatic`). ONLY consider a
                    // bytecode-private candidate here (the public ones already resolve unchanged); among
                    // those, accept only the metadata-public `inline` extension whose @Metadata receiver is
                    // EXACTLY this value class (the candidate was found at the erased Object/underlying
                    // rung, so an unrelated receiver must not bind it). `must_inline` stays on the bytecode
                    // visibility (no callable `invokestatic` → must splice).
                    let mut public = c.public;
                    if !c.public {
                        let meta_fn = self
                            .cp
                            .meta_functions(&c.owner)
                            .iter()
                            .find(|m| m.jvm_name == c.name && m.kotlin_name == name)
                            .cloned();
                        let value_recv_match = meta_fn
                            .as_ref()
                            .filter(|m| m.is_public && m.is_inline)
                            .and_then(|m| m.receiver_class.as_ref())
                            .is_some_and(|rc| {
                                receiver.obj_internal() == Some(rc.as_str())
                                    && self.cp.find(rc).is_some_and(|ci| {
                                        crate::jvm::metadata::class_inline(&ci).is_some()
                                    })
                            });
                        if value_recv_match {
                            public = true;
                        }
                    }
                    // Logical return, recovered RECEIVER-substituted (arg-independent): `<T> T.takeIf(…): T?`
                    // → `receiver`. A type var the receiver doesn't bind (`fold`'s `R`) stays as the erased
                    // physical type — an arg-binding selector (`resolve_callable`) refines that.
                    let ret = c
                        .signature
                        .as_deref()
                        .and_then(parse_method_gsig)
                        .map(|(_, psigs, rsig)| {
                            let mut binds = std::collections::HashMap::new();
                            if let Some(recv_sig) = psigs.first() {
                                unify_gsig(recv_sig, receiver, &mut binds);
                            }
                            gsig_to_ty(&rsig, &binds)
                        })
                        .unwrap_or(pret);
                    // A nullable Kotlin return over a PRIMITIVE receiver is the first-class
                    // `Ty::Nullable(prim)`, so a `?:`/null-check on the result is preserved (see
                    // `extension_callable`); the emit boxes it.
                    let ret = if ret.is_primitive() && ret_nullable {
                        crate::types::Ty::nullable(ret)
                    } else {
                        ret
                    };
                    // Source value-parameter NAMES (from `@Metadata`) for named-argument resolution. An
                    // extension's `callable.params` PREPENDS the receiver, but `CallSig.param_names` is the
                    // LOGICAL list (receiver excluded) — `metadata_param_names` returns exactly that (it
                    // aligns past the receiver via the metadata `has_recv` offset), so the names are
                    // `c.params.len() - 1` long. Defaults aren't recovered (named call supplies all).
                    let call_sig = match self.cp.metadata_param_names(&c.owner, &c.name, &params) {
                        Some(names) if names.len() + 1 == params.len() => {
                            crate::libraries::CallSig {
                                required: names.len(),
                                param_names: names,
                                ..Default::default()
                            }
                        }
                        _ => crate::libraries::CallSig::default(),
                    };
                    overloads.push(FunctionInfo {
                        kind: FnKind::Extension,
                        receiver: Some(receiver),
                        ret_nullable,
                        public,
                        receiver_rank: rank as u32,
                        call_sig,
                        flags: FnFlags {
                            inline: InlineKind::from_flags(inline, inline && !c.public),
                            suspend: self.cp.is_suspend_method(&c.owner, &c.name),
                        },
                        callable: LibraryCallable {
                            name: c.name.clone(),
                            owner: c.owner.clone(),
                            params,
                            ret,
                            physical_ret: pret,
                            descriptor: c.descriptor.clone(),
                            inline: InlineKind::from_flags(inline, inline && !c.public),
                            default_call: false,
                            vararg_elem: None,
                            signature: c.signature.clone(),
                            origin: crate::libraries::Origin::Library,
                        },
                    });
                }
            }
            // Metadata-mangled extensions on a value-class receiver. An extension on a value class
            // (`UInt.coerceAtMost`) has a `@JvmName`-MANGLED bytecode name (`coerceAtMost-5PvTz6A`) indexed
            // under the receiver's ERASED underlying descriptor, so the literal-name `find_extensions` above
            // misses it. kotlinc resolves it from `@Metadata`: the Kotlin name + extension receiver class.
            // For a value-class receiver only (bounding the blast radius), map `name` → the mangled method
            // via `meta_functions` (the facade-merged `@Metadata` decode), then load the real candidate by
            // that JVM name.
            // The receiver's value-class internal name — a dedicated `Ty::UInt`/`ULong` or an `Obj`.
            let recv_value_internal: Option<String> = match &receiver {
                Ty::UInt => Some("kotlin/UInt".to_string()),
                Ty::ULong => Some("kotlin/ULong".to_string()),
                Ty::Obj(i, _) => Some(i.to_string()),
                _ => None,
            };
            if let Some(recv_internal) = recv_value_internal {
                if let Some(recv_desc) = self.value_class_underlying_desc(&recv_internal) {
                    {
                        for owner in self.cp.find_extension_owners(&recv_desc) {
                            // `meta_functions` shares the facade-merged decode — for a multifile FACADE
                            // the functions live in the PART classes named in its `@Metadata` `d1`
                            // (`URangesKt` → `URangesKt___URangesKt`), already merged there.
                            let metafns = self.cp.meta_functions(&owner);
                            for mf in metafns.iter() {
                                // Only a metadata-mangled (jvm_name != kotlin name) public extension whose
                                // `@Metadata` receiver IS this value class.
                                if mf.kotlin_name != name
                                    || mf.jvm_name == name
                                    || !mf.is_public
                                    || mf.receiver_class.as_deref() != Some(recv_internal.as_str())
                                {
                                    continue;
                                }
                                for c in self.cp.find_extensions(&recv_desc, &mf.jvm_name) {
                                    let (params, pret) = parse_method_desc(&c.descriptor);
                                    let ret = self
                                        .cp
                                        .metadata_return_ty(&c.owner, &c.name)
                                        .unwrap_or(pret);
                                    overloads.push(FunctionInfo {
                                        kind: FnKind::Extension,
                                        receiver: Some(receiver),
                                        ret_nullable: self
                                            .cp
                                            .metadata_return_nullable(&c.owner, &c.name),
                                        public: true,
                                        // The value class is the most-specific receiver rung.
                                        receiver_rank: 0,
                                        call_sig: crate::libraries::CallSig::default(),
                                        flags: FnFlags {
                                            inline: InlineKind::from_flags(
                                                mf.is_inline,
                                                mf.is_inline && !c.public,
                                            ),
                                            suspend: mf.is_suspend,
                                        },
                                        callable: LibraryCallable {
                                            name: c.name.clone(),
                                            owner: c.owner.clone(),
                                            params,
                                            ret,
                                            physical_ret: pret,
                                            descriptor: c.descriptor.clone(),
                                            inline: InlineKind::from_flags(
                                                mf.is_inline,
                                                mf.is_inline && !c.public,
                                            ),
                                            default_call: false,
                                            vararg_elem: None,
                                            signature: c.signature.clone(),
                                            origin: crate::libraries::Origin::Library,
                                        },
                                    });
                                }
                            }
                        }
                    }
                }
            }
            // Member functions of the receiver's type (own + inherited) — "functions inside types". A member
            // wins over an extension; the caller uses `FnKind::Member` for that precedence. The inherited-
            // member walk is BREADTH-FIRST (a subtype's override before a supertype's), and each member
            // carries its visit rung in `receiver_rank` so an arg-binding consumer (`resolve_instance`) can
            // pick the closest type's overload — the same most-derived-first precedence the BFS gives.
            if let Ty::Obj(internal, _) = receiver {
                let mut seen = std::collections::HashSet::new();
                let mut queue = std::collections::VecDeque::new();
                queue.push_back(internal.to_string());
                let mut rung: u32 = 0;
                while let Some(cn) = queue.pop_front() {
                    if !seen.insert(cn.clone()) {
                        continue;
                    }
                    let Some(t) = self.resolve_type(&cn) else {
                        continue;
                    };
                    for m in &t.members {
                        if m.name == name {
                            // Source parameter NAMES (from the class's `@Metadata`) for named-argument
                            // resolution. A member's `params` are the logical params (no receiver), so the
                            // names align 1:1 when present. Defaults aren't recovered here (named call
                            // supplies all).
                            let call_sig = match self.cp.metadata_member_param_names(
                                &cn,
                                &m.name,
                                m.params.len(),
                            ) {
                                Some(names) => crate::libraries::CallSig {
                                    required: m.params.len(),
                                    param_names: names,
                                    ..Default::default()
                                },
                                _ => crate::libraries::CallSig::default(),
                            };
                            overloads.push(FunctionInfo {
                                kind: FnKind::Member,
                                receiver: Some(receiver),
                                ret_nullable: false,
                                public: true,
                                receiver_rank: rung,
                                call_sig,
                                flags: FnFlags::default(),
                                callable: LibraryCallable {
                                    name: m.name.clone(),
                                    owner: cn.clone(),
                                    params: m.params.clone(),
                                    ret: m.ret,
                                    physical_ret: m.ret,
                                    descriptor: m.descriptor.clone(),
                                    inline: InlineKind::None,
                                    default_call: false,
                                    vararg_elem: None,
                                    signature: None,
                                    origin: crate::libraries::Origin::Library,
                                },
                            });
                        }
                    }
                    queue.extend(t.supertypes);
                    rung += 1;
                }
            }
        } else {
            // Top-level (receiver-less) functions of this name — `listOf`, `run`, `println`, … — each with
            // its inline/`@InlineOnly` flags in one place.
            for c in self.cp.find_top_level(name) {
                let suspend = self.cp.is_suspend_method(&c.owner, &c.name);
                // A `suspend fun`'s physical method appends a `Continuation` parameter and erases the
                // return to `Object`; present the LOGICAL signature (drop the continuation) so a normal
                // call resolves. The coroutine pass re-derives the CPS form for the emitted call.
                let descriptor = if suspend {
                    strip_continuation_param(&c.descriptor)
                } else {
                    c.descriptor.clone()
                };
                let (mut params, physical_ret) = parse_method_desc(&descriptor);
                // Drop any SYNTHETIC trailing params the JVM descriptor appends beyond the `@Metadata`
                // SOURCE signature — a `@Composable` method's trailing `(Composer, int)` (a `suspend`
                // Continuation is already removed above). `@Metadata` records only the source
                // `value_parameter`s, so its count bounds the source params; keep the descriptor's
                // leading params (their exact types — an extension receiver, a vararg array) and
                // truncate the trailing synthetics. A normal function's metadata count equals the
                // descriptor's param count, so this is a no-op for it (no regression).
                if let Some(keep) = self.cp.metadata_kept_params(&c.owner, &c.name, &params) {
                    if keep < params.len() {
                        params.truncate(keep);
                    }
                }
                // A suspend method's physical return is erased to `Object`; recover the LOGICAL Kotlin
                // return type from `@Metadata` (`helper(): Int`), so the call types correctly. The
                // physical (erased) return stays `Object` for the emit.
                let ret = if suspend {
                    self.cp
                        .metadata_return_ty(&c.owner, &c.name)
                        .unwrap_or(physical_ret)
                } else {
                    physical_ret
                };
                let inline = self.cp.is_inline_method(&c.owner, &c.name);
                // Source value-parameter NAMES (from `@Metadata`) for named-argument resolution, and the
                // REQUIRED arity (non-defaulted param count) so a call may OMIT trailing defaulted args.
                // A top-level function has no receiver, so the logical params equal the (truncated) source
                // params — only wire names when the count aligns.
                let param_defaults = self
                    .cp
                    .metadata_param_defaults(&c.owner, &c.name, &params)
                    .unwrap_or_default();
                let required = if param_defaults.is_empty() {
                    params.len()
                } else {
                    param_defaults.iter().filter(|d| !**d).count()
                };
                let call_sig = match self.cp.metadata_param_names(&c.owner, &c.name, &params) {
                    Some(names) if names.len() == params.len() => crate::libraries::CallSig {
                        required,
                        param_names: names,
                        param_defaults,
                        ..Default::default()
                    },
                    _ => crate::libraries::CallSig {
                        required,
                        param_defaults,
                        ..Default::default()
                    },
                };
                overloads.push(FunctionInfo {
                    kind: FnKind::TopLevel,
                    receiver: None,
                    ret_nullable: false,
                    public: c.public,
                    receiver_rank: 0,
                    call_sig,
                    flags: FnFlags {
                        inline: InlineKind::from_flags(inline, inline && !c.public),
                        suspend,
                    },
                    callable: LibraryCallable {
                        name: c.name.clone(),
                        owner: c.owner.clone(),
                        params,
                        ret,
                        physical_ret,
                        descriptor,
                        inline: InlineKind::from_flags(inline, inline && !c.public),
                        default_call: false,
                        vararg_elem: None,
                        signature: c.signature.clone(),
                        origin: crate::libraries::Origin::Library,
                    },
                });
            }
        }
        FunctionSet { overloads }
    }
}

impl LibrarySet for JvmLibraries {
    fn coroutine_intrinsic(&self, name: &str) -> Option<crate::libraries::CoroutineIntrinsic> {
        crate::jvm::coroutine_intrinsics::recognize_unqualified(name)
    }

    fn value_companion_fn(
        &self,
        class_internal: &str,
        name: &str,
        n_args: usize,
    ) -> Option<crate::libraries::CompanionFn> {
        let ci = self.cp.find(class_internal)?;
        // Only a classpath value class has its companion fns realized this way (`Result.success`).
        crate::jvm::metadata::class_inline(&ci)?;
        let companion_field = crate::jvm::metadata::class_companion_name(&ci)?;
        let companion_internal = format!("{class_internal}${companion_field}");
        let comp_ci = self.cp.find(&companion_internal)?;
        let mf = crate::jvm::metadata::class_functions(&comp_ci)
            .into_iter()
            .find(|m| {
                m.kotlin_name == name
                    && m.is_public
                    && m.jvm_desc
                        .as_deref()
                        .map(|d| parse_method_desc(d).0.len() == n_args)
                        .unwrap_or(false)
            })?;
        let descriptor = mf.jvm_desc?;
        Some(crate::libraries::CompanionFn {
            class_internal: class_internal.to_string(),
            companion_internal,
            companion_field,
            jvm_name: mf.jvm_name,
            // The logical return is the value class itself (`Result`); its type argument stays erased,
            // matching kotlinc (a generic companion result flows as the erased underlying).
            ret: Ty::obj(class_internal),
            descriptor,
        })
    }

    fn prim_companion_const(&self, prim: &str, field: &str) -> Option<crate::libraries::LibConst> {
        use crate::jvm::classreader::ConstVal;
        use crate::libraries::LibConst;
        // The JVM realizes a primitive's companion as `kotlin/jvm/internal/<Prim>CompanionObject`,
        // whose `MAX_VALUE`/`MIN_VALUE`/… are `static final` with a `ConstantValue` (kotlinc inlines it).
        let internal = format!("kotlin/jvm/internal/{prim}CompanionObject");
        let ci = self.cp.find(&internal)?;
        let f = ci.fields.iter().find(|f| f.name == field)?;
        match f.const_value.as_ref()? {
            ConstVal::Int(v) => Some(LibConst::Int(*v)),
            ConstVal::Long(v) => Some(LibConst::Long(*v)),
            ConstVal::Float(v) => Some(LibConst::Float(*v)),
            ConstVal::Double(v) => Some(LibConst::Double(*v)),
            ConstVal::Str(_) => None,
        }
    }

    fn sam_method(&self, internal: &str) -> Option<LibraryMember> {
        let ci = self.cp.find(internal)?;
        if !ci.is_interface() {
            return None;
        }
        // The single public abstract instance method that isn't an `Object` method (`equals`/`hashCode`
        // /`toString`, which a functional interface may redeclare). `default`/`static` methods aren't
        // abstract (0x0400).
        let mut sam = None;
        for m in &ci.methods {
            if m.access & 0x0400 == 0 || m.is_static() || !m.is_public() {
                continue;
            }
            if matches!(m.name.as_str(), "equals" | "hashCode" | "toString") {
                continue;
            }
            if sam.is_some() {
                return None; // more than one abstract method — not a SAM interface
            }
            let (params, ret) = parse_method_desc(&m.descriptor);
            sam = Some(LibraryMember {
                name: m.name.clone(),
                params,
                ret,
                descriptor: m.descriptor.clone(),
            });
        }
        sam
    }

    fn mangled_member(&self, internal: &str, prefix: &str) -> Option<(String, String)> {
        // The first public instance method whose name starts with `prefix` (`getFirst-…`), searching the
        // class and its superclass chain — an inline-range getter is declared on the `…Progression`
        // superclass and inherited by the `…Range`. A mangled member has one such name per logical member,
        // so the prefix is unambiguous.
        let mut cur = Some(internal.to_string());
        let mut seen = std::collections::HashSet::new();
        while let Some(name) = cur {
            if !seen.insert(name.clone()) {
                break;
            }
            let ci = self.cp.find(&name)?;
            if let Some(m) = ci
                .methods
                .iter()
                .find(|m| m.is_public() && !m.is_static() && m.name.starts_with(prefix))
            {
                return Some((m.name.clone(), m.descriptor.clone()));
            }
            cur = ci.super_class.clone();
        }
        None
    }

    fn member_return(&self, recv: Ty, name: &str, args: &[Ty]) -> Option<Ty> {
        let Ty::Obj(start, start_args) = recv else {
            return None;
        };
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
            let Some(ci) = self.cp.find(&internal) else {
                continue;
            };
            let (formals, supers) = ci.signature.as_deref().and_then(parse_class_gsig).unzip();
            let formals = formals.unwrap_or_default();
            let binds: std::collections::HashMap<String, Ty> =
                formals.iter().cloned().zip(targs.iter().copied()).collect();
            // A member declared here whose parameters match the call.
            let found = ci
                .methods
                .iter()
                .filter(|m| m.is_public() && !m.is_static() && m.name == name)
                .find(|m| {
                    let (params, _) = parse_method_desc(&m.descriptor);
                    params.len() == args.len()
                        && params.iter().zip(args).all(|(p, a)| arg_fits(p, a))
                });
            if let Some(m) = found {
                let sig = m.signature.as_deref()?;
                let (m_formals, _, rsig) = parse_method_gsig(sig)?;
                // A method type parameter that SHADOWS a class one (`<T> T m()` inside `class C<T>`) is
                // INDEPENDENT of the receiver's type argument — drop the class binding for every name
                // the method re-declares, so its return erases to the method param's bound instead of
                // mis-substituting the receiver's argument (which would `checkcast` a wrong type → CCE).
                let mut binds = binds;
                for f in &m_formals {
                    binds.remove(f);
                }
                return Some(gsig_to_ty(&rsig, &binds));
            }
            // Propagate type arguments up each supertype edge (substituting this class's bindings).
            if let Some(supers) = supers {
                for sup in supers {
                    if let GSig::Class(sup_internal, sup_args) = sup {
                        let sup_targs: Vec<Ty> =
                            sup_args.iter().map(|a| gsig_to_ty(a, &binds)).collect();
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

    fn instance_call_return(&self, recv: Ty, name: &str, args: &[Ty]) -> Option<Ty> {
        let Ty::Obj(start, _) = recv else {
            return None;
        };
        // Walk `recv`'s class hierarchy for a public instance method `name` whose arity matches and whose
        // every parameter ACCEPTS the actual argument — exact/erased (`arg_fits`) OR a reference subtype
        // (`KSerializer<Foo>` where `DeserializationStrategy<? extends T>` is declared). Then bind the
        // method's type variables by unifying its generic parameter signatures against the argument types
        // (positional type-arg unify ignores the parameter's class name, so the subtype's `<Foo>` still
        // binds `T`), and substitute the generic return.
        let mut seen = std::collections::HashSet::new();
        let mut q = std::collections::VecDeque::new();
        q.push_back(to_jvm_internal(start).to_string());
        while let Some(internal) = q.pop_front() {
            if !seen.insert(internal.clone()) {
                continue;
            }
            let Some(ci) = self.cp.find(&internal) else {
                continue;
            };
            let found = ci
                .methods
                .iter()
                .filter(|m| m.is_public() && !m.is_static() && m.name == name)
                .find(|m| {
                    let (params, _) = parse_method_desc(&m.descriptor);
                    params.len() == args.len()
                        && params
                            .iter()
                            .zip(args)
                            .all(|(p, a)| arg_fits(p, a) || self.erased_subtype(a, p))
                });
            if let Some(m) = found {
                let sig = m.signature.as_deref()?;
                let (_, psigs, rsig) = parse_method_gsig(sig)?;
                let mut binds = std::collections::HashMap::new();
                for (ps, a) in psigs.iter().zip(args) {
                    unify_gsig(ps, *a, &mut binds);
                }
                return Some(gsig_to_ty(&rsig, &binds));
            }
            for s in ci.interfaces.iter().chain(ci.super_class.iter()) {
                q.push_back(s.clone());
            }
        }
        None
    }

    fn builtin_member_ret(&self, internal: &str, name: &str, args: &[Ty]) -> Option<Ty> {
        self.cp
            .builtin_member_ret(internal, name, args)
            .or_else(|| {
                // A Kotlin built-in member over a JVM-mapped receiver (`CharSequence.length`, `Number.toInt`)
                // is declared in `.kotlin_builtins` under the KOTLIN name, while the receiver here may carry
                // the mapped JVM name — retry under the Kotlin built-in identity.
                crate::jvm::jvm_class_map::jvm_to_kotlin_builtin_with_members(internal)
                    .and_then(|kotlin| self.cp.builtin_member_ret(kotlin, name, args))
            })
    }

    fn canonical_internal<'a>(&self, internal: &'a str) -> std::borrow::Cow<'a, str> {
        std::borrow::Cow::Borrowed(crate::jvm::jvm_class_map::to_jvm_internal(internal))
    }

    fn builtin_member_call(
        &self,
        internal: &str,
        name: &str,
        n_args: usize,
    ) -> Option<(String, String, String, Ty, bool)> {
        self.cp.builtin_member_call(internal, name, n_args)
    }

    fn can_inline_lambda(&self, owner: &str, name: &str, descriptor: &str) -> bool {
        // Dry-run the ONE splicer with each `FunctionN` parameter as a lambda site — branchless
        // (`let`/`also`) AND branchy (`takeIf`/`takeUnless`) hosts; `splice_unified` relocates host AND
        // lambda-body frames, so what it accepts here it emits correctly (else it returns `None` and the
        // call falls back / the file skips — never a miscompile).
        self.cp
            .method_code(owner, name, descriptor)
            .map_or(false, |body| {
                let lambdas: Vec<crate::jvm::inline::LambdaSplice> =
                    function_param_indices(descriptor)
                        .into_iter()
                        .map(|param_index| crate::jvm::inline::LambdaSplice {
                            param_index,
                            body: Vec::new(),
                        })
                        .collect();
                let mut dummy =
                    crate::jvm::classfile::ClassWriter::new("Dummy", "java/lang/Object");
                crate::jvm::inline::splice_unified(&body, descriptor, 1, &lambdas, 0, &mut dummy)
                    .is_some()
            })
    }

    fn can_inline_call(&self, owner: &str, name: &str, descriptor: &str) -> bool {
        self.cp
            .method_code(owner, name, descriptor)
            .map_or(false, |body| {
                // Dry-run the ONE splicer the emitter uses (`splice_unified`) into a throwaway
                // `ClassWriter`, with each descriptor `Function0` parameter as a zero-arg lambda site.
                // It covers branchless, branchy, and lambda-bearing hosts, and exercises constant-pool
                // relocation — so an un-relocatable body (`invokedynamic`, a pool entry `relocate_const`
                // rejects, …) fails the gate and stays unresolved rather than falling back to an
                // `invokestatic` on a private method (an `IllegalAccessError`). A branchy body still needs
                // an empty operand-stack baseline at the call site; a non-empty one skips the file
                // (`must_inline`), never miscompiles.
                let lambdas: Vec<crate::jvm::inline::LambdaSplice> =
                    function_param_indices(descriptor)
                        .into_iter()
                        .map(|param_index| crate::jvm::inline::LambdaSplice {
                            param_index,
                            body: Vec::new(),
                        })
                        .collect();
                let mut dummy =
                    crate::jvm::classfile::ClassWriter::new("Dummy", "java/lang/Object");
                crate::jvm::inline::splice_unified(&body, descriptor, 1, &lambdas, 0, &mut dummy)
                    .is_some()
            })
    }

    fn resolve_scope_inline(
        &self,
        name: &str,
        receiver: Ty,
        args: &[Ty],
    ) -> Option<LibraryCallable> {
        // The arg-binding RESOLUTION layer over the same candidate metadata `functions` exposes: it binds a
        // generic return from the ARGUMENTS (`let`'s `R` from the lambda), which the arg-independent
        // `functions` query can't recover — so it stays its own selector (see the redesign layering note).
        self.extension_callable(name, receiver, args, &[], true)
    }

    fn metadata_return_unsigned(&self, owner: &str, name: &str) -> bool {
        matches!(
            self.cp.metadata_return_type(owner, name).as_deref(),
            Some("kotlin/UByte" | "kotlin/UShort" | "kotlin/UInt" | "kotlin/ULong")
        )
    }

    fn lambda_return_overload_param(&self, receiver: Ty, name: &str) -> Option<Vec<Ty>> {
        // `name` resolves by lambda return type iff some facade for the receiver has `@JvmName` overloads
        // under this Kotlin name. The selector's `it` is the receiver's element type.
        let is_overloaded = supertype_descriptors(&self.cp, receiver).iter().any(|d| {
            self.cp
                .find_extension_owners(d)
                .iter()
                .any(|o| self.cp.lambda_return_overloads(o).contains_key(name))
        });
        if !is_overloaded {
            return None;
        }
        let elem = receiver
            .array_elem()
            .or_else(|| receiver.type_args().first().copied())?;
        Some(vec![elem])
    }

    fn toplevel_lambda_param_types(
        &self,
        name: &str,
        arg_tys: &[Option<Ty>],
    ) -> Option<Vec<Vec<Ty>>> {
        for c in self.cp.find_top_level(name) {
            let Some(sig) = c.signature.as_deref() else {
                continue;
            };
            let Some((_, psigs, _)) = parse_method_gsig(sig) else {
                continue;
            };
            if psigs.len() != arg_tys.len() {
                continue;
            }
            let mut binds = std::collections::HashMap::new();
            for (ps, at) in psigs.iter().zip(arg_tys) {
                if let Some(t) = at {
                    unify_gsig(ps, *t, &mut binds);
                }
            }
            let out: Vec<Vec<Ty>> = psigs
                .iter()
                .map(|ps| function_input_types(ps, &binds))
                .collect();
            // Accept this overload only if a *lambda* position (an untyped `None` argument) actually
            // recovered parameter types — so an overload whose lambda is elsewhere isn't mis-picked.
            if out
                .iter()
                .zip(arg_tys)
                .any(|(v, at)| at.is_none() && !v.is_empty())
            {
                return Some(out);
            }
        }
        None
    }

    fn toplevel_lambda_recvs(&self, name: &str, arg_tys: &[Option<Ty>]) -> Option<Vec<Option<Ty>>> {
        // A top-level fn's source params equal its JVM params (no receiver slot), so the per-source-param
        // receiver-function-type receivers from `@Metadata` align positionally with `arg_tys`. Reads ONLY
        // `@Metadata` — no JVM `Signature` attribute needed (a krusty-emitted module omits it).
        for c in self.cp.find_top_level(name) {
            let recvs = self.cp.metadata_param_recv_funs(&c.owner, name);
            if recvs.len() == arg_tys.len() && recvs.iter().any(|o| o.is_some()) {
                return Some(
                    recvs
                        .into_iter()
                        .map(|o| o.map(|internal| Ty::obj(&internal)))
                        .collect(),
                );
            }
        }
        None
    }

    fn extension_lambda_param_types(
        &self,
        recv: Ty,
        name: &str,
        arg_tys: &[Option<Ty>],
    ) -> Option<Vec<Vec<Ty>>> {
        // Find a generic extension named `name` on the receiver (or a supertype) that takes a function
        // argument; bind its type variables from the receiver and the already-typed non-lambda
        // arguments, then report each lambda argument's element-typed parameters (`Function1<? super
        // T, …>` on `List<Int>` → `[Int]`; `fold(0) { acc, x -> }` binds the accumulator from `0`).
        // Pass 1 prefers a PUBLIC candidate (a non-public `@InlineOnly` one must not shadow a real generic
        // overload — it would type `it` as the erased `Any`). Pass 2 falls back to non-public, for a scope
        // fn with NO public overload (`takeIf`/`takeUnless`: `<T> T.takeIf((T) -> Boolean): T?`, inlined
        // from its real body). Either way the lambda's parameter types come from the generic signature.
        for allow_non_public in [false, true] {
            for recv_desc in supertype_descriptors(&self.cp, recv) {
                for c in self.cp.find_extensions(&recv_desc, name) {
                    if !c.public && !allow_non_public {
                        continue;
                    }
                    let Some(sig) = c.signature.as_deref() else {
                        continue;
                    };
                    // A non-public candidate matched via the erased `Object` key must have a type-variable
                    // receiver (the scope-fn family) — never a concrete value-class receiver (`Result.map`).
                    if !c.public
                        && recv_desc == "Ljava/lang/Object;"
                        && !nonpublic_ext_receiver_is_typevar(Some(sig))
                    {
                        continue;
                    }
                    let Some((_, psigs, _)) = parse_method_gsig(sig) else {
                        continue;
                    };
                    if psigs.is_empty() {
                        continue;
                    }
                    let n_real = psigs.len() - 1; // value parameters (psigs[0] is the receiver)
                    let k = arg_tys.len();
                    // Map each ARGUMENT to a value parameter. Exact arity → positional. A TRAILING LAMBDA
                    // with fewer args than params (`list.joinToString { it }` — the lambda fills the LAST
                    // param `transform`, the middle defaulted) → the leading args fill a prefix and the
                    // trailing lambda binds the LAST parameter (`(T) -> CharSequence`).
                    let trailing_lambda = k >= 1 && arg_tys[k - 1].is_none();
                    let mapped: Vec<&_> = if n_real == k {
                        psigs[1..].iter().collect()
                    } else if trailing_lambda && n_real > k && k >= 1 {
                        let mut v: Vec<&_> = psigs[1..k].iter().collect();
                        v.push(&psigs[n_real]); // last value parameter (the trailing-lambda slot)
                        v
                    } else {
                        continue;
                    };
                    let mut binds = std::collections::HashMap::new();
                    unify_gsig(&psigs[0], recv, &mut binds); // bind from the receiver parameter
                    for (ps, at) in mapped.iter().zip(arg_tys) {
                        if let Some(t) = at {
                            unify_gsig(ps, *t, &mut binds); // bind from each typed non-lambda argument
                        }
                    }
                    let out: Vec<Vec<Ty>> = mapped
                        .iter()
                        .map(|ps| function_input_types(ps, &binds))
                        .collect();
                    if out.iter().any(|v| !v.is_empty()) {
                        return Some(out);
                    }
                }
            }
        }
        None
    }

    fn resolve_callable(
        &self,
        name: &str,
        receiver: Option<Ty>,
        args: &[Ty],
        type_args: &[Ty],
    ) -> Option<LibraryCallable> {
        let Some(receiver) = receiver else {
            // Receiver-less top-level function (`listOf(…)`): find every static method of this name
            // and pick the overload matching `args` — an exact-arity match (boxing-aware), else a
            // vararg match (the final reference-array parameter absorbs the trailing arguments).
            // Public-only: normal resolution emits an `invokestatic`, so a non-public (`@InlineOnly`)
            // candidate must never be picked here — it would fault with `IllegalAccessError` at runtime.
            // (The inline route reaches non-public scope fns through `resolve_scope_inline`, not this.)
            // Candidates come from the consolidated `functions` query (one source of truth), not a direct
            // index read; the non-public `@InlineOnly` branch below reuses the same set.
            let fs = self.functions(name, None);
            let parsed: Vec<(&FunctionInfo, Vec<Ty>, Ty)> = fs
                .overloads
                .iter()
                .filter(|o| o.kind == FnKind::TopLevel && o.public)
                // Reuse the params the `functions` query already built — one source of truth. They
                // carry the `@Metadata` source signature (synthetic trailing params dropped); re-parsing
                // the descriptor here would reinstate a `@Composable` callee's `(Composer, int)`.
                .map(|o| (o, o.callable.params.clone(), o.callable.ret))
                .collect();
            // Exact arity first.
            let pick = parsed
                .iter()
                .find(|(_, params, _)| {
                    params.len() == args.len()
                        && params.iter().zip(args).all(|(p, a)| arg_fits(p, a))
                })
                .or_else(|| {
                    parsed.iter().find(|(_, params, _)| {
                        // Vararg: fixed leading params match positionally, the last (array) param's element
                        // type absorbs the rest.
                        if params.is_empty() {
                            return args.len() == 0;
                        }
                        let fixed = params.len() - 1;
                        let Some(elem) = params[fixed].array_elem() else {
                            return false;
                        };
                        args.len() >= fixed
                            && params[..fixed]
                                .iter()
                                .zip(args)
                                .all(|(p, a)| arg_fits(p, a))
                            && args[fixed..].iter().all(|a| arg_fits(&elem, a))
                    })
                });
            // No exact/vararg match — try the `name$default` synthetic for a top-level function with
            // default parameters (`assertEquals(a, b)` → `assertEquals$default(a, b, null, mask, null)`).
            // Its descriptor is `(real…, int mask, Object marker)ret`; the call fills a prefix of the real
            // parameters and the backend defaults the rest. A trailing lambda interacts with defaulted
            // middle parameters in a way the prefix-fill doesn't model — leave those unresolved.
            // A trailing lambda interacts with `$default`'s defaulted middle parameters in a way the
            // prefix-fill doesn't model, so skip the `$default` attempt for it — but still fall through
            // to the non-public `@InlineOnly` branch below (a `require(cond) { lazyMessage }` is spliced).
            let trailing_lambda = args.last().map_or(false, |a| matches!(a, Ty::Fun(_)));
            let fsd = if pick.is_none() && !trailing_lambda {
                self.functions(&format!("{name}$default"), None)
            } else {
                FunctionSet::default()
            };
            if pick.is_none() && !trailing_lambda {
                for o in fsd.overloads.iter().filter(|o| o.kind == FnKind::TopLevel) {
                    let c = &o.callable;
                    if !o.public {
                        continue;
                    }
                    let (params, ret) = parse_method_desc(&c.descriptor);
                    if params.len() < 2 {
                        continue; // need at least int mask + Object marker
                    }
                    let real_count = params.len() - 2;
                    if args.len() > real_count {
                        continue;
                    }
                    if !params[..args.len()]
                        .iter()
                        .zip(args)
                        .all(|(p, a)| arg_fits(p, a))
                    {
                        continue;
                    }
                    let kept: Vec<Ty> = params[..real_count].to_vec();
                    let ret_ty = c
                        .signature
                        .as_ref()
                        .and_then(|sig| parse_method_gsig(sig))
                        .map(|(formals, psigs, rsig)| {
                            let mut binds = std::collections::HashMap::new();
                            for (f, t) in formals.iter().zip(type_args) {
                                binds.insert(f.clone(), *t);
                            }
                            for (ps, a) in psigs.iter().zip(args) {
                                unify_gsig(ps, *a, &mut binds);
                            }
                            gsig_to_ty(&rsig, &binds)
                        })
                        .unwrap_or(ret);
                    return Some(LibraryCallable {
                        owner: c.owner.clone(),
                        name: c.name.clone(),
                        params: kept,
                        ret: ret_ty,
                        physical_ret: ret,
                        descriptor: c.descriptor.clone(),
                        inline: InlineKind::from_flags(
                            self.cp.is_inline_method(&c.owner, &c.name),
                            false,
                        ),
                        default_call: true,
                        vararg_elem: None,
                        signature: c.signature.clone(),
                        origin: crate::libraries::Origin::Library,
                    });
                }
            }
            // No public / `$default` match — try a NON-PUBLIC `@InlineOnly` top-level function
            // (`error`/`require`/`check`/…): kotlinc emits no callable method for these, so they MUST be
            // inlined. Return one as `is_inline` so the backend splices its real body; gated by
            // `can_inline_call` (dry-runs the splice) so an un-spliceable body simply stays unresolved
            // rather than falling back to an `invokestatic` on the private method.
            if pick.is_none() {
                for o in fs.overloads.iter().filter(|o| o.kind == FnKind::TopLevel) {
                    let c = &o.callable;
                    if o.public || !self.cp.is_inline_method(&c.owner, &c.name) {
                        continue;
                    }
                    let (params, ret) = parse_method_desc(&c.descriptor);
                    if params.len() != args.len()
                        || !params.iter().zip(args).all(|(p, a)| arg_fits(p, a))
                    {
                        continue;
                    }
                    if !self.can_inline_call(&c.owner, &c.name, &c.descriptor) {
                        continue;
                    }
                    // Recover the generic logical return (`run`/`with`'s `R` binds from the lambda's return
                    // type) — the JVM descriptor erases it to `Object`. Without this the call types as a
                    // reference and a primitive result (`run { 2 + 3 }: Int`) miscompiles (a boxed value in
                    // a primitive slot). Mirrors the `$default` and extension paths.
                    let recovered = c
                        .signature
                        .as_ref()
                        .and_then(|sig| parse_method_gsig(sig))
                        .map(|(formals, psigs, rsig)| {
                            let mut binds = std::collections::HashMap::new();
                            for (f, t) in formals.iter().zip(type_args) {
                                binds.insert(f.clone(), *t);
                            }
                            for (ps, a) in psigs.iter().zip(args) {
                                unify_gsig(ps, *a, &mut binds);
                            }
                            gsig_to_ty(&rsig, &binds)
                        })
                        .unwrap_or(ret);
                    // A kotlin `Nothing` return compiles to a `java/lang/Void` JVM descriptor; type the
                    // call `Nothing` so the backend treats it as diverging (no value, no post-call pop).
                    let logical_ret = if c.descriptor.ends_with(")Ljava/lang/Void;") {
                        Ty::Nothing
                    } else {
                        recovered
                    };
                    return Some(LibraryCallable {
                        owner: c.owner.clone(),
                        name: c.name.clone(),
                        params,
                        ret: logical_ret,
                        physical_ret: ret,
                        descriptor: c.descriptor.clone(),
                        inline: InlineKind::MustInline,
                        default_call: false,
                        vararg_elem: None,
                        signature: c.signature.clone(),
                        origin: crate::libraries::Origin::Library,
                    });
                }
            }
            let (o, params, ret) = pick?;
            let c = &o.callable;
            // A reified reflection intrinsic (`typeOf` → `KType`) is implemented by inlining + reified
            // substitution; called as a plain static it throws at runtime. krusty doesn't inline it —
            // leave it unresolved (the file skips) rather than emit a call that fails.
            if ret.obj_internal() == Some("kotlin/reflect/KType") {
                return None;
            }
            // Recover the parameterized return from the generic signature: bind the type variables
            // from the actual arguments (the vararg element unifies with each trailing arg) and
            // substitute into the return node. Falls back to the erased return when absent.
            // Bind the type variables from the explicit type arguments and the actuals, then realize the
            // parameterized return — and, for a generic vararg, the bound *element* type the trailing
            // arguments adapt to (`listOf<Long>(…)` → `Long`), which the backend uses for literal
            // adaptation (the JVM array element is erased to `Object`).
            let mut vararg_elem = None;
            let ret_ty = c
                .signature
                .as_ref()
                .and_then(|sig| parse_method_gsig(sig))
                .map(|(formals, psigs, rsig)| {
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
                            vararg_elem = Some(gsig_to_ty(inner, &binds));
                        }
                    } else {
                        for (ps, a) in psigs.iter().zip(args) {
                            unify_gsig(ps, *a, &mut binds);
                        }
                    }
                    gsig_to_ty(&rsig, &binds)
                })
                .unwrap_or(*ret);
            // Restore the Kotlin read-only/mutable collection type from `@Metadata` (the JVM signature
            // erased `mutableListOf`'s `MutableList<T>` to `java/util/List<T>`).
            let ret_ty = self.meta_collection_ret(&c.owner, &c.name, ret_ty);
            // A `suspend fun`'s physical return is erased to `Object`; the overload already carries the
            // LOGICAL return recovered from `@Metadata` (`helper(): Int`) — use it.
            let ret_ty = if o.flags.suspend { c.ret } else { ret_ty };
            return Some(LibraryCallable {
                owner: c.owner.clone(),
                name: c.name.clone(),
                params: params.clone(),
                ret: ret_ty,
                physical_ret: *ret,
                descriptor: c.descriptor.clone(),
                inline: InlineKind::from_flags(self.cp.is_inline_method(&c.owner, &c.name), false),
                default_call: false,
                vararg_elem,
                signature: c.signature.clone(),
                origin: crate::libraries::Origin::Library,
            });
        };
        // Try the receiver type and its supertypes, most specific first — the extension's declared
        // receiver may be a supertype (kotlinc's `String.repeat` is a `CharSequence` extension), or a
        // generic `T` erased to `Object` (`fun <T> T.to(…)`). Match by boxing-aware parameter
        // assignability (an `Any` parameter accepts any argument), not exact descriptor prefix.
        if let Some(lc) = self.extension_callable(name, receiver, args, type_args, false) {
            return Some(lc);
        }
        // No exact-arity match — try the `name$default` synthetic for an extension with default
        // parameters (`list.joinToString(",")` → `joinToString$default(list, ",", …, mask, null)`).
        // Its descriptor is `(recv, real…, int mask, Object marker)ret`; the call fills a prefix of the
        // real parameters, the backend defaults the rest.
        // A trailing lambda binds to the *last* value parameter (`transform`), with the middle parameters
        // defaulted — `list.joinToString { it }` → `joinToString$default(list, …defaults…, transform,
        // mask, null)`. The leading non-lambda args fill a prefix; the lambda fills the last real param.
        let trailing_lambda = args.last().is_some_and(|a| matches!(a, Ty::Fun(_)));
        let default_name = format!("{name}$default");
        for recv_desc in supertype_descriptors(&self.cp, receiver) {
            for c in self.cp.find_extensions(&recv_desc, &default_name) {
                // Public-only, like the exact-arity path: never emit an `invokestatic` to a non-public
                // `$default` synthetic (`IllegalAccessError`).
                if !c.public {
                    continue;
                }
                let (params, ret) = parse_method_desc(&c.descriptor);
                if params.len() < 3 {
                    continue; // need at least receiver + mask + marker
                }
                let real_count = params.len() - 3; // exclude receiver, int mask, Object marker
                let param_is_fun = |t: &Ty| {
                    matches!(t, Ty::Fun(_))
                        || t.obj_internal()
                            .is_some_and(|i| i.starts_with("kotlin/jvm/functions/Function"))
                };
                // Validate the fit. Non-lambda: the args fill a prefix of the real parameters (each fits,
                // subtype-aware, so a wrong overload is rejected). Trailing lambda: the prefix (all but the
                // lambda) fits, AND the LAST real parameter is a function type (the `transform` slot).
                let fits = if trailing_lambda {
                    let prefix_len = args.len() - 1;
                    prefix_len < real_count
                        && param_is_fun(&params[real_count])
                        && params[1..1 + prefix_len]
                            .iter()
                            .zip(&args[..prefix_len])
                            .all(|(p, a)| arg_fits_subtype(&self.cp, p, a))
                } else {
                    args.len() <= real_count
                        && params[1..1 + args.len()]
                            .iter()
                            .zip(args)
                            .all(|(p, a)| arg_fits_subtype(&self.cp, p, a))
                };
                if !fits {
                    continue;
                }
                // Keep the receiver + real parameters (drop the trailing mask + marker), like the
                // non-`$default` case — the backend appends the placeholders, mask, and marker.
                let kept: Vec<Ty> = params[..params.len() - 2].to_vec();
                let ret_ty = c
                    .signature
                    .as_ref()
                    .and_then(|sig| parse_method_gsig(sig))
                    .map(|(formals, psigs, rsig)| {
                        let mut binds = std::collections::HashMap::new();
                        for (f, t) in formals.iter().zip(type_args) {
                            binds.insert(f.clone(), *t);
                        }
                        // psigs for `$default` are `[recv, real…, int, Object]`; unify the receiver + provided.
                        let actuals: Vec<Ty> = std::iter::once(receiver)
                            .chain(args.iter().copied())
                            .collect();
                        for (ps, a) in psigs.iter().zip(&actuals) {
                            unify_gsig(ps, *a, &mut binds);
                        }
                        gsig_to_ty(&rsig, &binds)
                    })
                    .unwrap_or(ret);
                return Some(LibraryCallable {
                    owner: c.owner.clone(),
                    name: c.name.clone(),
                    params: kept,
                    ret: ret_ty,
                    physical_ret: ret,
                    descriptor: c.descriptor.clone(),
                    inline: InlineKind::from_flags(
                        self.cp.is_inline_method(&c.owner, &c.name),
                        false,
                    ),
                    default_call: true,
                    vararg_elem: None,
                    signature: c.signature.clone(),
                    origin: crate::libraries::Origin::Library,
                });
            }
        }
        None
    }
}

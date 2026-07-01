//! The JVM implementation of the [`SymbolSource`] abstraction: resolves symbols from a `.class`-jar
//! classpath (the bytecode target). All classpath reads, JVM method-descriptor parsing, and
//! `java/lang ↔ kotlin` name normalization live here — the front end (`resolve`, `ir_lower`) sees
//! only Kotlin-level `Ty`s and opaque descriptor tokens through the trait.

use super::classpath::Classpath;
use super::jvm_class_map::{
    kotlin_builtin_to_internal, to_jvm_internal, to_kotlin_internal, BUILTIN_MAPPED_NAMES,
};
use crate::call_resolver::{arg_fits, function_input_types, gsig_to_ty, unify_gsig};
use crate::jvm::names::{method_descriptor, property_getter_name, type_descriptor};
use crate::libraries::{
    CountedLoopInfo, FnFlags, FnKind, FunctionInfo, FunctionSet, GSig, GenericSig, InlineKind,
    LibConst, LibraryCallable, LibraryConst, LibraryMember, LibrarySeed, LibraryType, Origin,
    PlatformAccessor, PlatformCtor, PlatformField, PlatformRangeCtor, RangeConstruction,
    RuntimeCtor, RuntimeOp,
};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

/// The JVM platform's contribution to Kotlin's default imports (the LANGUAGE-level `kotlin.*` set lives
/// in [`crate::resolve::KOTLIN_DEFAULT_IMPORT_PACKAGES`]; the two are composed in `import_wildcards` and
/// in the seed filter, so neither list is duplicated).
const PLATFORM_DEFAULT_IMPORT_PACKAGES: &[&str] = &["java.lang", "kotlin.jvm"];

/// The slash-form packages whose classes are reachable by a bare, unqualified, unimported name — the
/// full Kotlin default-import set (language + this platform). Used to filter the classpath seed so a
/// bare name cannot silently bind to an arbitrary classpath class (`Widget` → `jdk/internal/.../Widget`,
/// `plain` → `sun/.../plain` — a silent miscompile).
fn default_import_packages_internal() -> std::collections::HashSet<String> {
    crate::resolve::KOTLIN_DEFAULT_IMPORT_PACKAGES
        .iter()
        .chain(PLATFORM_DEFAULT_IMPORT_PACKAGES)
        .map(|p| p.replace('.', "/"))
        .collect()
}

trait JvmScalarTy {
    fn is_jvm_scalar(&self) -> bool;
}

impl JvmScalarTy for Ty {
    fn is_jvm_scalar(&self) -> bool {
        matches!(
            *self,
            Ty::Int
                | Ty::Byte
                | Ty::Short
                | Ty::Long
                | Ty::Float
                | Ty::Double
                | Ty::Boolean
                | Ty::Char
                | Ty::UInt
                | Ty::ULong
        )
    }
}

/// A platform backed by a JVM classpath (dirs + jars + the JDK jimage). The classpath is shared
/// (`Rc`) with the JVM backend/emitter so the bytecode inliner reads inline-function bodies through
/// the same lazily-populated caches — all within the `jvm` module, never through the `SymbolSource`
/// abstraction.
pub struct JvmLibraries {
    cp: std::rc::Rc<Classpath>,
    functions_cache:
        std::cell::RefCell<std::collections::HashMap<(String, Option<Ty>), FunctionSet>>,
}

impl JvmLibraries {
    pub fn new(cp: std::rc::Rc<Classpath>) -> JvmLibraries {
        JvmLibraries {
            cp,
            functions_cache: std::cell::RefCell::new(std::collections::HashMap::new()),
        }
    }

    fn primitive_companion_consts_for_type(
        &self,
        internal: &str,
    ) -> std::collections::HashMap<String, LibraryConst> {
        use crate::jvm::classreader::ConstVal;

        let prim = match internal {
            "java/lang/Integer" | "kotlin/Int" => "Int",
            "java/lang/Long" | "kotlin/Long" => "Long",
            "java/lang/Short" | "kotlin/Short" => "Short",
            "java/lang/Byte" | "kotlin/Byte" => "Byte",
            "java/lang/Character" | "kotlin/Char" => "Char",
            "java/lang/Double" | "kotlin/Double" => "Double",
            "java/lang/Float" | "kotlin/Float" => "Float",
            "java/lang/Boolean" | "kotlin/Boolean" => "Boolean",
            _ => return std::collections::HashMap::new(),
        };
        let internal = format!("kotlin/jvm/internal/{prim}CompanionObject");
        let Some(ci) = self.cp.find(&internal) else {
            return std::collections::HashMap::new();
        };
        ci.fields
            .iter()
            .filter_map(|f| {
                let value = match f.const_value.as_ref()? {
                    ConstVal::Int(v) => LibConst::Int(*v),
                    ConstVal::Long(v) => LibConst::Long(*v),
                    ConstVal::Float(v) => LibConst::Float(*v),
                    ConstVal::Double(v) => LibConst::Double(*v),
                    ConstVal::Str(_) => return None,
                };
                Some((
                    f.name.clone(),
                    LibraryConst {
                        ty: field_desc_to_ty(&f.descriptor),
                        value,
                    },
                ))
            })
            .collect()
    }

    fn metadata_static_companion_consts_for_type(
        &self,
        internal: &str,
    ) -> std::collections::HashMap<String, LibraryConst> {
        use crate::jvm::classreader::ConstVal;

        let Some(ci) = self.cp.find(internal) else {
            return std::collections::HashMap::new();
        };
        let companion_internal = format!("{internal}$Companion");
        let Some(companion) = self.cp.find(&companion_internal) else {
            return std::collections::HashMap::new();
        };
        let prop_rets = super::metadata::class_property_return_classes(&companion);
        if prop_rets.is_empty() {
            return std::collections::HashMap::new();
        }
        ci.fields
            .iter()
            .filter_map(|f| {
                let ret = prop_rets.get(&f.name)?;
                let ty = metadata_return_ty(Some(ret))?;
                let value = match f.const_value.as_ref()? {
                    ConstVal::Int(v) => LibConst::Int(*v),
                    ConstVal::Long(v) => LibConst::Long(*v),
                    ConstVal::Float(v) => LibConst::Float(*v),
                    ConstVal::Double(v) => LibConst::Double(*v),
                    ConstVal::Str(_) => return None,
                };
                Some((f.name.clone(), LibraryConst { ty, value }))
            })
            .collect()
    }

    fn companion_consts_for_type(
        &self,
        internal: &str,
    ) -> std::collections::HashMap<String, LibraryConst> {
        let mut out = self.primitive_companion_consts_for_type(internal);
        out.extend(self.metadata_static_companion_consts_for_type(internal));
        out
    }

    fn builtin_members_for_type(&self, internal: &str) -> Vec<LibraryMember> {
        let kotlin = crate::jvm::jvm_class_map::jvm_to_kotlin_builtin_with_members(internal)
            .unwrap_or(internal);
        self.cp.builtin_members(kotlin)
    }

    fn range_accessor(name: &str, descriptor: &str) -> PlatformAccessor {
        PlatformAccessor {
            name: name.to_string(),
            descriptor: descriptor.to_string(),
        }
    }

    fn member_accessor_by_prefix(&self, internal: &str, prefix: &str) -> Option<PlatformAccessor> {
        let mut seen = std::collections::HashSet::new();
        let mut q = std::collections::VecDeque::new();
        q.push_back(internal.to_string());
        while let Some(cur) = q.pop_front() {
            if !seen.insert(cur.clone()) {
                continue;
            }
            let t = <Self as SymbolSource>::resolve_type(self, &cur)?;
            if let Some(m) = t.members.iter().find(|m| m.name.starts_with(prefix)) {
                return Some(PlatformAccessor {
                    name: m.name.clone(),
                    descriptor: m.descriptor.clone(),
                });
            }
            q.extend(t.supertypes);
        }
        None
    }

    fn counted_loop_info_for_type(&self, internal: &str) -> Option<CountedLoopInfo> {
        let unit_step = |elem, first_desc, last_desc| CountedLoopInfo {
            elem,
            first: Self::range_accessor("getFirst", first_desc),
            last: Self::range_accessor("getLast", last_desc),
            step: None,
        };
        let progression = |elem, first_desc, last_desc, step_desc, step_ty| CountedLoopInfo {
            elem,
            first: Self::range_accessor("getFirst", first_desc),
            last: Self::range_accessor("getLast", last_desc),
            step: Some((Self::range_accessor("getStep", step_desc), step_ty)),
        };
        Some(match internal {
            "kotlin/ranges/IntRange" => unit_step(Ty::Int, "()I", "()I"),
            "kotlin/ranges/LongRange" => unit_step(Ty::Long, "()J", "()J"),
            "kotlin/ranges/IntProgression" => progression(Ty::Int, "()I", "()I", "()I", Ty::Int),
            "kotlin/ranges/LongProgression" => progression(Ty::Long, "()J", "()J", "()J", Ty::Long),
            "kotlin/ranges/CharProgression" => progression(Ty::Char, "()C", "()C", "()I", Ty::Int),
            "kotlin/ranges/UIntRange" => CountedLoopInfo {
                elem: Ty::UInt,
                first: self.member_accessor_by_prefix(internal, "getFirst-")?,
                last: self.member_accessor_by_prefix(internal, "getLast-")?,
                step: None,
            },
            "kotlin/ranges/ULongRange" => CountedLoopInfo {
                elem: Ty::ULong,
                first: self.member_accessor_by_prefix(internal, "getFirst-")?,
                last: self.member_accessor_by_prefix(internal, "getLast-")?,
                step: None,
            },
            "kotlin/ranges/UIntProgression" => CountedLoopInfo {
                elem: Ty::UInt,
                first: self.member_accessor_by_prefix(internal, "getFirst-")?,
                last: self.member_accessor_by_prefix(internal, "getLast-")?,
                step: Some((Self::range_accessor("getStep", "()I"), Ty::Int)),
            },
            "kotlin/ranges/ULongProgression" => CountedLoopInfo {
                elem: Ty::ULong,
                first: self.member_accessor_by_prefix(internal, "getFirst-")?,
                last: self.member_accessor_by_prefix(internal, "getLast-")?,
                step: Some((Self::range_accessor("getStep", "()J"), Ty::Long)),
            },
            _ => return None,
        })
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
                let gsig = parse_method_gsig(sig)?;
                let mut binds = binds;
                for f in &gsig.formals {
                    binds.remove(f);
                }
                return Some(gsig_to_ty(&gsig.ret, &binds));
            }
            if let Some(supers) = supers {
                for sup in supers {
                    if let GSig::Class(sup_internal, sup_args) = sup {
                        let sup_targs: Vec<Ty> =
                            sup_args.iter().map(|a| gsig_to_ty(a, &binds)).collect();
                        q.push_back((to_jvm_internal(&sup_internal).to_string(), sup_targs));
                    }
                }
            } else {
                for i in ci.interfaces.iter().chain(ci.super_class.iter()) {
                    q.push_back((i.clone(), vec![]));
                }
            }
        }
        None
    }

    fn sam_method_for_class(&self, internal: &str) -> Option<LibraryMember> {
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
                return None;
            }
            let (params, ret) = parse_method_desc(&m.descriptor);
            let mut member = LibraryMember::new(m.name.clone(), params, ret, m.descriptor.clone());
            member.signature = m.signature.clone();
            sam = Some(member);
        }
        sam
    }

    fn value_companion_fns_for_class(&self, internal: &str) -> Vec<crate::libraries::CompanionFn> {
        let Some(ci) = self.cp.find(internal) else {
            return Vec::new();
        };
        if crate::jvm::metadata::class_inline(&ci).is_none() {
            return Vec::new();
        }
        let Some(companion_field) = crate::jvm::metadata::class_companion_name(&ci) else {
            return Vec::new();
        };
        let companion_internal = format!("{internal}${companion_field}");
        let Some(comp_ci) = self.cp.find(&companion_internal) else {
            return Vec::new();
        };
        crate::jvm::metadata::class_functions(&comp_ci)
            .into_iter()
            .filter(|m| m.is_public)
            .filter_map(|m| {
                let descriptor = m.jvm_desc?;
                let (params, _) = parse_method_desc(&descriptor);
                Some(crate::libraries::CompanionFn {
                    class_internal: internal.to_string(),
                    companion_internal: companion_internal.clone(),
                    companion_field: companion_field.clone(),
                    callable: LibraryCallable {
                        owner: companion_internal.clone(),
                        name: m.jvm_name,
                        params,
                        // The logical return is the value class itself (`Result`); its type argument
                        // stays erased, matching kotlinc (a generic companion result flows as the
                        // erased underlying).
                        ret: Ty::obj(internal),
                        physical_ret: Ty::obj("kotlin/Any"),
                        descriptor,
                        inline: InlineKind::MustInline,
                        default_call: false,
                        vararg_elem: None,
                        signature: None,
                        origin: Origin::Library,
                    },
                })
            })
            .collect()
    }

    fn value_class_metadata_members_for_class(
        &self,
        ci: &crate::jvm::classreader::ClassInfo,
    ) -> Vec<LibraryMember> {
        if crate::jvm::metadata::class_inline(ci).is_none() {
            return Vec::new();
        }
        crate::jvm::metadata::class_functions(ci)
            .into_iter()
            .filter(|m| m.is_public && !m.is_extension)
            .filter_map(|m| {
                crate::trace_compiler!(
                    "resolve",
                    "value-class member metadata {}.{} jvm={} desc={:?} ret={:?}",
                    ci.this_class,
                    m.kotlin_name,
                    m.jvm_name,
                    m.jvm_desc,
                    m.ret_class
                );
                let descriptor = m.jvm_desc?;
                let (params, physical_ret) = parse_method_desc(&descriptor);
                // Value-class implementation methods are static and take the erased receiver as their
                // first JVM parameter. Source member resolution sees only the value parameters.
                let logical_params = params.get(1..).unwrap_or(&[]).to_vec();
                let ret = metadata_return_ty(m.ret_class.as_deref()).unwrap_or(physical_ret);
                let mut member = LibraryMember::new(m.kotlin_name, logical_params, ret, descriptor);
                member.owner = Some(ci.this_class.clone());
                member.physical_name = Some(m.jvm_name);
                member.physical_ret = physical_ret;
                member.ret_nullable = m.ret_nullable;
                member.inline = InlineKind::from_flags(m.is_inline, m.is_inline);
                Some(member)
            })
            .collect()
    }
}

/// Parse one JVM generic-signature type off the front of `s`, returning `(node, rest)`.
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
                let node = if internal.starts_with("kotlin/jvm/functions/Function") {
                    let ret = Box::new(gsig_unbox_wrapper(args.pop()?));
                    let params = args.into_iter().map(gsig_unbox_wrapper).collect();
                    GSig::Function { params, ret }
                } else {
                    GSig::Class(internal, args)
                };
                Some((node, after))
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

fn gsig_unbox_wrapper(g: GSig) -> GSig {
    match g {
        GSig::Class(internal, args) => match internal.as_str() {
            "java/lang/Integer" | "kotlin/Int" => GSig::Prim(Ty::Int),
            "java/lang/Long" | "kotlin/Long" => GSig::Prim(Ty::Long),
            "java/lang/Short" | "kotlin/Short" => GSig::Prim(Ty::Short),
            "java/lang/Byte" | "kotlin/Byte" => GSig::Prim(Ty::Byte),
            "java/lang/Character" | "kotlin/Char" => GSig::Prim(Ty::Char),
            "java/lang/Boolean" | "kotlin/Boolean" => GSig::Prim(Ty::Boolean),
            "java/lang/Double" | "kotlin/Double" => GSig::Prim(Ty::Double),
            "java/lang/Float" | "kotlin/Float" => GSig::Prim(Ty::Float),
            _ => GSig::Class(internal, args),
        },
        other => other,
    }
}

/// Parse a leading `<Name:Bound...>` formal-type-parameter block, returning the formal names and the
/// remaining signature. No block means empty names and unchanged input.
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

/// Parse a JVM method generic signature `<formals>(params)ret`.
fn parse_method_gsig(sig: &str) -> Option<GenericSig> {
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
    Some(GenericSig {
        formals,
        params,
        ret,
    })
}

/// A type's simple (unqualified) source name, from its canonical internal name's last segment
/// (`kotlin/Int` → `Int`, `kotlin/UInt` → `UInt`, `app/Foo` → `Foo`). Generic — no per-type list.
fn ty_simple_name(t: Ty) -> Option<&'static str> {
    source_internal_of_ty(t).map(|i| i.rsplit('/').next().unwrap_or(i))
}

/// The canonical element type of a `@JvmName`-mangled reduction extension's RECEIVER — its first
/// generic parameter's sole type argument (`sumOfInt(Iterable<Integer>): int` → `Int`), canonicalized
/// (`java/lang/Integer`/`kotlin/Int` → `Ty::Int`). Used to pick the element-appropriate overload of
/// `sum`/`average`/… among the per-element methods that share a Kotlin source name. `None` if the
/// signature's receiver isn't a single-type-argument container.
fn gsig_receiver_element(sig: Option<&str>) -> Option<Ty> {
    let gsig = sig.and_then(parse_method_gsig)?;
    match gsig.params.first()? {
        GSig::Class(_, args) => {
            // Unbox a boxed-primitive wrapper (`java/lang/Integer`/`kotlin/Int` → `Int`) so the element
            // compares equal to the receiver's primitive element; a reference element stays its class.
            let elem_gsig = gsig_unbox_wrapper(args.first()?.clone());
            Some(crate::call_resolver::gsig_to_ty(
                &elem_gsig,
                &std::collections::HashMap::new(),
            ))
        }
        _ => None,
    }
}

/// A member's return type recovered from its generic signature ONLY when it is fully CONCRETE (carries
/// type arguments, none of which is a free type variable) — `all(): List<Item>` → `List<Item>`. This lets
/// a member of a NON-generic receiver still carry its return's type arguments (which `member_return`
/// skips, as it only propagates the receiver's own arguments). A return naming a type variable
/// (`fun <T> load(): T`, `List<E>.get(): E`) is NOT recovered here — it stays erased / is bound by
/// `member_return` under the receiver's arguments.
fn concrete_generic_ret(gsig: &GenericSig) -> Option<Ty> {
    fn is_concrete(g: &GSig) -> bool {
        match g {
            GSig::Var(_) => false,
            GSig::Prim(_) => true,
            GSig::Arr(inner) => is_concrete(inner),
            GSig::Function { params, ret } => params.iter().all(is_concrete) && is_concrete(ret),
            GSig::Class(_, args) => args.iter().all(is_concrete),
        }
    }
    match &gsig.ret {
        GSig::Class(_, args) if !args.is_empty() && is_concrete(&gsig.ret) => Some(
            crate::call_resolver::gsig_to_ty(&gsig.ret, &std::collections::HashMap::new()),
        ),
        _ => None,
    }
}

/// The LOGICAL return of a `suspend` method, recovered from its generic signature: the last parameter is
/// `Continuation<-T>`, whose type argument `T` is the source return type (`Continuation<-Config>` →
/// `Config`). A `Continuation<-Unit>` maps to `Ty::Unit` (the source `Unit` return).
fn suspend_return_from_gsig(gsig: &GenericSig) -> Option<Ty> {
    match gsig.params.last()? {
        GSig::Class(n, args) if n == "kotlin/coroutines/Continuation" => match args.first()? {
            // A bare class → its CANONICAL `Ty` (`kotlin/String` → `Ty::String`, `kotlin/Int` → `Ty::Int`,
            // `kotlin/Unit` → `Ty::Unit`), so the recovered return unifies with the source-spelled type
            // rather than a non-canonical `Obj("kotlin/String")`. A generic class (`List<Item>`) keeps its
            // arguments via the general converter.
            GSig::Class(name, cargs) if cargs.is_empty() => {
                Some(super::classpath::kotlin_name_to_ty(name))
            }
            other => Some(crate::call_resolver::gsig_to_ty(
                other,
                &std::collections::HashMap::new(),
            )),
        },
        _ => None,
    }
}

/// Count the `Byte`/`Short` primitive parameters in a JVM method descriptor — the "narrowing" measure
/// used to prefer the widest among overloads krusty's `Byte`/`Short`/`Int` → `Int` collapse made
/// indistinguishable. Object (`L…;`) and array (`[`) params are skipped (a `B`/`S` inside a class name
/// must not count).
pub(crate) fn descriptor_narrowing(desc: &str) -> usize {
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
        s if s == type_descriptor(Ty::String) => Ty::String,
        s if s.starts_with('[') => Ty::array(desc_to_ty(&s[1..])),
        s if s.starts_with('L') && s.ends_with(';') => {
            let internal = to_kotlin_internal(&s[1..s.len() - 1]);
            if let Some(n) = function_iface_arity(internal) {
                Ty::fun(vec![Ty::obj("kotlin/Any"); n], Ty::obj("kotlin/Any"))
            } else {
                Ty::obj(internal)
            }
        }
        _ => Ty::Error,
    }
}

fn field_desc_to_ty(d: &str) -> Ty {
    match d {
        "B" => Ty::Byte,
        "S" => Ty::Short,
        s if s.starts_with('[') => Ty::array(field_desc_to_ty(&s[1..])),
        _ => desc_to_ty(d),
    }
}

fn function_iface_arity(internal: &str) -> Option<usize> {
    internal
        .strip_prefix("kotlin/jvm/functions/Function")
        .and_then(|n| n.parse::<usize>().ok())
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

fn source_internal_of_ty(t: Ty) -> Option<&'static str> {
    Some(match t {
        Ty::Boolean => "kotlin/Boolean",
        Ty::Byte => "kotlin/Byte",
        Ty::Short => "kotlin/Short",
        Ty::Int => "kotlin/Int",
        Ty::Long => "kotlin/Long",
        Ty::Char => "kotlin/Char",
        Ty::Float => "kotlin/Float",
        Ty::Double => "kotlin/Double",
        Ty::UInt => "kotlin/UInt",
        Ty::ULong => "kotlin/ULong",
        Ty::String => "kotlin/String",
        Ty::Obj(internal, _) => internal,
        Ty::Nullable(inner) | Ty::TyParam(_, inner) => return source_internal_of_ty(*inner),
        _ => return None,
    })
}

/// Curated JVM ABI for the well-known mapped builtins, used only when the classpath cannot supply the
/// mapped JVM class (a no-classpath compile, e.g. a self-contained snippet with no `-cp`). This keeps
/// the Kotlin↔JVM mapping a *backend* fact: the member's JVM owner/descriptor live here, so the compiler
/// core resolves `kotlin/String.length` generically (through `resolve_type`/`functions`) and never spells
/// `java/lang/String` itself. A real classpath always wins — this is reached only when the class is
/// genuinely unreadable.
fn mapped_builtin_fallback(internal: &str) -> Option<LibraryType> {
    // Each tuple: Kotlin member name, JVM descriptor, logical return type. The owner is left implicit
    // (the receiver's Kotlin internal, e.g. `kotlin/String`); the constant-pool boundary maps it to the
    // JVM name, exactly as for a classpath-resolved member — so this fallback adds no `java/lang/*` name.
    let members: &[(&str, &str, Ty)] = match internal {
        "kotlin/String" => &[("length", "()I", Ty::Int), ("hashCode", "()I", Ty::Int)],
        _ => return None,
    };
    let members = members
        .iter()
        .map(|(name, desc, ret)| {
            LibraryMember::new((*name).to_string(), vec![], *ret, (*desc).to_string())
        })
        .collect();
    Some(LibraryType {
        is_public: true,
        kind: crate::libraries::TypeKind::Class,
        supertypes: Vec::new(),
        constructors: Vec::new(),
        members,
        companion: Vec::new(),
        companion_consts: Default::default(),
        sam_method: None,
        companion_object: None,
        value_companion_fns: Vec::new(),
        value_underlying: None,
    })
}

fn nonpublic_ext_receiver_is_typevar(signature: Option<&str>) -> bool {
    signature
        .and_then(parse_method_gsig)
        .is_some_and(|gsig| matches!(gsig.params.first(), Some(GSig::Var(_))))
}

fn metadata_return_ty(class: Option<&str>) -> Option<Ty> {
    class.map(super::classpath::kotlin_name_to_ty)
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

fn parse_method_desc_with_field_params(desc: &str) -> (Vec<Ty>, Ty) {
    let close = desc.find(')').unwrap_or(0);
    let mut rest = &desc[1..close];
    let mut params = Vec::new();
    while let Some((one, tail)) = split_one(rest) {
        params.push(field_desc_to_ty(one));
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
        _ => return vec![type_descriptor(receiver), object],
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

/// Whether `internal` is, transitively, a subtype of `target` (a superclass or implemented interface,
/// at any depth). Names are normalized to their JVM spelling so a Kotlin built-in (`kotlin/collections/
/// MutableMap`) and its `java/util/Map` realization compare equal.
fn class_implements(cp: &Classpath, internal: &str, target: &str) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut q = std::collections::VecDeque::new();
    q.push_back(to_jvm_internal(internal).to_string());
    while let Some(name) = q.pop_front() {
        if name == target {
            return true;
        }
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(ci) = cp.find(&name) {
            for s in ci.interfaces.iter().chain(ci.super_class.iter()) {
                q.push_back(to_jvm_internal(s).to_string());
            }
        }
    }
    false
}

fn member_matches_query(member_name: &str, query: &str) -> bool {
    member_name == query
        || matches!(
            (member_name, query),
            ("keySet", "keys") | ("entrySet", "entries")
        )
}

impl SymbolSource for JvmLibraries {
    fn platform_default_import_packages(&self) -> &'static [&'static str] {
        PLATFORM_DEFAULT_IMPORT_PACKAGES
    }

    fn physical_property_getter_name(&self, property: &str) -> Option<String> {
        let getter = property_getter_name(property);
        (getter != property).then_some(getter)
    }

    fn constructor_param_names(&self, internal: &str, arity: usize) -> Option<Vec<String>> {
        self.cp.metadata_constructor_param_names(internal, arity)
    }

    fn value_class_ctor_has_default(&self, internal: &str) -> bool {
        // A defaulted primary constructor of a value class surfaces as the synthetic
        // `constructor-impl$default` static method (kotlinc emits it iff some param has a default).
        self.cp.find(internal).is_some_and(|ci| {
            ci.methods
                .iter()
                .any(|m| m.name == "constructor-impl$default")
        })
    }

    fn is_enum_entry(&self, internal: &str, name: &str) -> bool {
        // An enum constant is a `static` field of the enum's OWN type (`descriptor == L<internal>;`),
        // distinguishing it from `$VALUES` (an array `[L<internal>;`) and a companion INSTANCE.
        const ACC_STATIC: u16 = 0x0008;
        let want = format!("L{internal};");
        self.cp.find(internal).is_some_and(|ci| {
            ci.fields
                .iter()
                .any(|f| f.name == name && f.access & ACC_STATIC != 0 && f.descriptor == want)
        })
    }

    fn value_class_property_member(
        &self,
        internal: &str,
        property: &str,
    ) -> Option<crate::libraries::LibraryMember> {
        let ci = self.cp.find(internal)?;
        // The property's LOGICAL (@Metadata) return type — a value class here (`id` → `lib/Vid`), whose
        // physical getter erases the return to the underlying. Only handle a value-class-typed property
        // (guarded by `value_underlying`), so an ordinary property keeps its normal getter path.
        let logical = crate::jvm::metadata::class_property_return_classes(&ci)
            .get(property)?
            .clone();
        self.resolve_type(&logical)
            .and_then(|t| t.value_underlying)?;
        // Getter name = `get` + capitalized property, possibly `@JvmName`-mangled with a `-<hash>` suffix
        // (kotlinc mangles any accessor whose signature mentions a value class).
        let mut chars = property.chars();
        let cap = chars.next()?.to_uppercase().collect::<String>();
        let getter = format!("get{cap}{}", chars.as_str());
        let dashed = format!("{getter}-");
        let m = ci
            .methods
            .iter()
            .find(|mm| mm.name == getter || mm.name.starts_with(&dashed))?;
        crate::trace_compiler!(
            "value_classes",
            "value-class property {internal}.{property} -> getter {} : {logical}",
            m.name
        );
        let mut member = crate::libraries::LibraryMember::new(
            m.name.clone(),
            vec![],
            Ty::obj(&logical),
            m.descriptor.clone(),
        );
        member.owner = Some(internal.to_string());
        Some(member)
    }

    fn infer_constructor_type_args(&self, internal: &str, arg_tys: &[Ty]) -> Option<Vec<Ty>> {
        let ci = self.cp.find(internal)?;
        // The class's formal type parameters, in declaration order (`Pair` → `[A, B]`).
        let (formals, _) = ci.signature.as_deref().and_then(parse_class_gsig)?;
        if formals.is_empty() {
            return None;
        }
        // The primary constructor of matching arity, and its generic parameter signatures (which name the
        // class formals, e.g. `(TA;TB;)V`). Unify each with the actual argument type to bind the formals.
        let mut binds = std::collections::HashMap::new();
        for m in &ci.methods {
            if m.name != "<init>" {
                continue;
            }
            let Some(gsig) = m.signature.as_deref().and_then(parse_method_gsig) else {
                continue;
            };
            if gsig.params.len() != arg_tys.len() {
                continue;
            }
            for (p, a) in gsig.params.iter().zip(arg_tys) {
                crate::call_resolver::unify_gsig(p, *a, &mut binds);
            }
            break;
        }
        if binds.is_empty() {
            return None;
        }
        Some(
            formals
                .iter()
                .map(|f| {
                    binds
                        .get(f)
                        .copied()
                        .unwrap_or_else(|| Ty::obj("kotlin/Any"))
                })
                .collect(),
        )
    }

    fn seed(&self) -> LibrarySeed {
        let (class_names, type_aliases, canonical_names) = self.seed_shared();
        LibrarySeed {
            class_names: (*class_names).clone(),
            type_aliases: (*type_aliases).clone(),
            canonical_names: (*canonical_names).clone(),
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
        // A bare (unqualified, unimported) type name resolves ONLY to a class in a DEFAULT-IMPORT
        // package — kotlinc semantics. NOT to an arbitrary classpath class: a global simple-name seed
        // of the whole classpath silently binds a bare name to whatever shares it (`Widget`→
        // `jdk/internal/.../Widget`, `plain`→`sun/.../plain`, shadowing a user `fun plain()`) — a
        // silent miscompile. Same-package user types and explicit/wildcard imports are added per file
        // by the signature collector; default-import typealiases (`ArrayList`→`java/util/ArrayList`,
        // declared in `kotlin.collections`) are seeded just below from the type-alias index.
        let default_pkgs = default_import_packages_internal();
        let mut class_names: std::collections::HashMap<String, String> = idx
            .class_names
            .iter()
            .filter(|(_, internal)| {
                internal
                    .rfind('/')
                    .is_some_and(|i| default_pkgs.contains(&internal[..i]))
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut canonical_names = std::collections::HashMap::new();
        // Seed the Kotlin built-in → JVM class mapping (ported `JavaToKotlinClassMap`): intrinsic
        // mapped types (`Comparable`, `Throwable`, `List`, …), not `.class` files. Classpath types
        // above take precedence (`or_insert`).
        for name in BUILTIN_MAPPED_NAMES {
            if let Some(internal) = kotlin_builtin_to_internal(name) {
                let canonical = to_jvm_internal(internal);
                if canonical != internal {
                    canonical_names.insert(internal.to_string(), canonical.to_string());
                }
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
        for internal in [
            "kotlin/Boolean",
            "kotlin/Byte",
            "kotlin/Short",
            "kotlin/Int",
            "kotlin/Long",
            "kotlin/Char",
            "kotlin/Float",
            "kotlin/Double",
            "kotlin/Throwable",
        ] {
            let canonical = to_jvm_internal(internal);
            if canonical != internal {
                canonical_names.insert(internal.to_string(), canonical.to_string());
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
            std::rc::Rc::new(canonical_names),
        );
        CACHE.with(|c| c.borrow_mut().insert(key, pair.clone()));
        pair
    }

    fn value_underlying(&self, ty: Ty) -> Option<Ty> {
        match ty {
            Ty::UInt => Some(Ty::Int),
            Ty::ULong => Some(Ty::Long),
            _ => <Self as SymbolSource>::resolve_type(self, ty.obj_internal()?)
                .and_then(|t| t.value_underlying),
        }
    }

    fn is_unsigned_integer_type(&self, ty: Ty) -> bool {
        matches!(ty, Ty::UInt | Ty::ULong)
    }

    fn function_like_arity(&self, ty: Ty) -> Option<usize> {
        ty.fun_arity()
            .map(usize::from)
            .or_else(|| match ty.obj_internal()? {
                "kotlin/reflect/KProperty1" | "kotlin/reflect/KMutableProperty1" => Some(1),
                "kotlin/reflect/KProperty0" | "kotlin/reflect/KMutableProperty0" => Some(0),
                _ => None,
            })
    }

    fn property_reference_type(&self, arity: usize, mutable: bool) -> Option<Ty> {
        let internal = match (arity, mutable) {
            (0, false) => "kotlin/reflect/KProperty0",
            (0, true) => "kotlin/reflect/KMutableProperty0",
            (1, false) => "kotlin/reflect/KProperty1",
            (1, true) => "kotlin/reflect/KMutableProperty1",
            _ => return None,
        };
        Some(Ty::obj(internal))
    }

    fn class_literal_type(&self) -> Option<Ty> {
        Some(Ty::obj("java/lang/Class"))
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
                match self.cp.find(mapped) {
                    Some(ci) => ci,
                    // The classpath has neither the Kotlin name nor the mapped JVM class — a no-classpath
                    // compile (no JDK/stdlib to read). Fall back to the backend's curated minimal ABI for
                    // the well-known mapped builtins so member resolution stays generic and the compiler
                    // core never has to spell a `java/lang/*` name itself.
                    None => return mapped_builtin_fallback(internal),
                }
            }
        };
        let mut constructors = Vec::new();
        let mut members = Vec::new();
        let mut companion = Vec::new();
        // `Map.put` returns the PREVIOUS value (`V?`, null for a fresh key) — Kotlin enhances this Java
        // method's nullability. It applies to ANY `Map` subtype (`HashMap`, `TreeMap`, …), since a call
        // resolves the member on the concrete class, not on `Map` itself.
        let is_map = class_implements(&self.cp, internal, "java/util/Map");
        for m in &ci.methods {
            // Only public members are callable from generated code.
            if !m.is_public() {
                continue;
            }
            let (params, ret) = parse_method_desc(&m.descriptor);
            let mut member = LibraryMember::new(m.name.clone(), params, ret, m.descriptor.clone());
            member.signature = m.signature.clone();
            member.suspend = self.cp.is_suspend_method(internal, &m.name);
            if is_map && member.name == "put" {
                member.ret_nullable = true;
            }
            if m.name == "<init>" {
                constructors.push(member);
            } else if m.is_static() {
                // A Kotlin companion member compiles to a JVM static on the class.
                companion.push(member);
            } else {
                members.push(member);
            }
        }
        // A member whose JVM name is MANGLED by a value-class PARAMETER (`fun get(id: Vid): Cat` →
        // `get-<hash>(String)`): the descriptor-read loop above stored it under the mangled name, so a
        // source-name call `p.get(v)` misses it, and its erased `String` parameter wouldn't accept the
        // `Vid` argument. Recover the SOURCE name + logical (value-class) parameter types from `@Metadata`
        // and expose the member under the source name — keeping the mangled JVM name as `physical_name`
        // and the erased descriptor for emit (the value-classes pass unboxes the `Vid` argument).
        for mf in crate::jvm::metadata::class_functions(&ci) {
            if !mf.is_public || mf.is_extension || mf.jvm_name == mf.kotlin_name {
                continue;
            }
            let Some(desc) = mf.jvm_desc.as_deref() else {
                continue;
            };
            let (params, physical_ret) = parse_method_desc(desc);
            if params.len() != mf.value_param_types.len() {
                continue; // synthetic JVM params (suspend/composer) — not a plain mangled member
            }
            let logical: Vec<Ty> = params
                .iter()
                .zip(&mf.value_param_types)
                .map(|(p, vt)| {
                    vt.as_deref()
                        .map(super::classpath::kotlin_name_to_ty)
                        .unwrap_or(*p)
                })
                .collect();
            let ret = metadata_return_ty(mf.ret_class.as_deref()).unwrap_or(physical_ret);
            let mut member =
                LibraryMember::new(mf.kotlin_name.clone(), logical, ret, desc.to_string());
            member.physical_name = Some(mf.jvm_name.clone());
            member.physical_ret = physical_ret;
            member.ret_nullable = mf.ret_nullable;
            member.suspend = mf.is_suspend;
            crate::trace_compiler!(
                "resolve",
                "mangled member {}.{} jvm={} logical_params={:?}",
                internal,
                mf.kotlin_name,
                mf.jvm_name,
                member.params
            );
            members.push(member);
        }
        // Every JDK `Throwable` has a no-arg and a single-message constructor; synthesize those two
        // shapes when the classpath reader can't surface the jimage constructor descriptors.
        if constructors.is_empty() && super::jvm_class_map::is_throwable_internal(internal) {
            constructors.push(LibraryMember::new(
                "<init>".into(),
                vec![],
                Ty::Unit,
                "()V".into(),
            ));
            constructors.push(LibraryMember::new(
                "<init>".into(),
                vec![Ty::String],
                Ty::Unit,
                format!("({})V", type_descriptor(Ty::String)),
            ));
        }
        let mut supertypes = ci.interfaces.clone();
        if let Some(s) = &ci.super_class {
            supertypes.push(s.clone());
        }
        for s in self.cp.builtin_supertypes(internal) {
            if !supertypes.iter().any(|existing| existing == &s) {
                supertypes.push(s);
            }
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
                // The underlying type was carried in the @Metadata type TABLE (proto field 19), not
                // inlined — `class_inline` can't resolve it there. Recover it from the synthesized
                // `box-impl(U)` parameter descriptor, the authoritative JVM underlying type. (A type
                // PARAMETER underlying — `Result<T>` — has no concrete box-impl param and stays `Any`.)
                None => ci
                    .methods
                    .iter()
                    .find(|m| m.name == "box-impl")
                    .and_then(|m| m.descriptor.strip_prefix('('))
                    .and_then(split_one)
                    .map(|(d, _)| field_desc_to_ty(d))
                    .unwrap_or_else(|| Ty::obj("kotlin/Any")),
            }
        });
        let value_class_metadata_members = self.value_class_metadata_members_for_class(&ci);
        Some(LibraryType {
            is_public: ci.is_public(),
            kind,
            supertypes,
            constructors,
            members: members
                .into_iter()
                .chain(value_class_metadata_members)
                .chain(self.builtin_members_for_type(internal))
                .collect(),
            companion,
            companion_consts: self.companion_consts_for_type(&ci.this_class),
            sam_method: self.sam_method_for_class(&ci.this_class),
            companion_object,
            value_companion_fns: self.value_companion_fns_for_class(&ci.this_class),
            value_underlying,
        })
    }
    fn functions(&self, name: &str, receiver: Option<Ty>) -> FunctionSet {
        let key = (name.to_string(), receiver);
        if let Some(cached) = self.functions_cache.borrow().get(&key) {
            return cached.clone();
        }
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
                                || params.first().map(|p| type_descriptor(*p)).as_deref()
                                    != Some(recv_desc.as_str())
                                || type_descriptor(pret) != type_descriptor(ret)
                            {
                                continue;
                            }
                            // Disambiguate `IntArray.sumOf` from `UIntArray.sumOf` (both erase to
                            // `([I, Function1)I`) by the SELECTOR parameter type from the generic signature
                            // == the receiver's element type — so an `Int` lambda never binds a `UInt` body.
                            if let Some(elem) = want_elem {
                                let Some(gsig) = c.signature.as_deref().and_then(parse_method_gsig)
                                else {
                                    continue;
                                };
                                let mut binds = std::collections::HashMap::new();
                                if let Some(recv_sig) = gsig.params.first() {
                                    unify_gsig(recv_sig, receiver, &mut binds);
                                }
                                let selector_matches = gsig
                                    .params
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
                                ret_class: None,
                                public: c.public,
                                // The lambda-return family is resolved by return type, never through the
                                // arg-binding extension selector — mark it so it can't preempt a real rung.
                                receiver_rank: u32::MAX,
                                overload_rank: descriptor_narrowing(&c.descriptor) as u32,
                                generic_sig: c.signature.as_deref().and_then(parse_method_gsig),
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
                        let value_recv_match = self
                            .cp
                            .meta_functions(&c.owner)
                            .iter()
                            .find(|m| m.jvm_name == c.name && m.kotlin_name == name)
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
                    // A non-public candidate matched via erased `Object` must have a type-variable
                    // receiver (`T.let`/`takeIf`). Concrete value-class receivers such as `Result.map`
                    // also erase to `Object`, but must not become candidates for unrelated receivers like
                    // `List.map`; value-class-specific mangled lookup below handles the real receiver.
                    if !public
                        && recv_desc == "Ljava/lang/Object;"
                        && !nonpublic_ext_receiver_is_typevar(c.signature.as_deref())
                    {
                        continue;
                    }
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
                    if let Ty::Obj(recv_internal, _) = &receiver {
                        if self.cp.is_kotlin_collection(recv_internal) {
                            let recvs = self.cp.metadata_receiver_types(&c.owner, &c.name);
                            let collection_recvs: Vec<&String> = recvs
                                .iter()
                                .filter(|r| self.cp.is_kotlin_collection(r))
                                .collect();
                            if !collection_recvs.is_empty()
                                && !collection_recvs
                                    .iter()
                                    .any(|r| self.cp.kotlin_subtype(recv_internal, r))
                            {
                                continue;
                            }
                        }
                    }
                    let (mut params, pret) = parse_method_desc_with_field_params(&c.descriptor);
                    let is_default = c.name.ends_with("$default");
                    let meta_name = c.name.strip_suffix("$default").unwrap_or(&c.name);
                    if is_default && params.len() >= 2 {
                        params.truncate(params.len() - 2);
                    }
                    let inline =
                        self.cp
                            .is_inline_callable(&c.owner, &c.name, &c.descriptor, &params);
                    let meta_ret = self.cp.metadata_return(&c.owner, meta_name);
                    let ret_nullable = meta_ret.as_ref().is_some_and(|r| r.nullable);
                    let ret_class =
                        metadata_return_ty(meta_ret.as_ref().and_then(|r| r.class.as_deref()));
                    // Logical return, recovered RECEIVER-substituted (arg-independent): `<T> T.takeIf(…): T?`
                    // → `receiver`. A type var the receiver doesn't bind (`fold`'s `R`) stays as the erased
                    // physical type — arg-binding selection in `CallResolver` refines that.
                    let ret = c
                        .signature
                        .as_deref()
                        .and_then(parse_method_gsig)
                        .map(|gsig| {
                            let mut binds = std::collections::HashMap::new();
                            if let Some(recv_sig) = gsig.params.first() {
                                unify_gsig(recv_sig, receiver, &mut binds);
                            }
                            gsig_to_ty(&gsig.ret, &binds)
                        })
                        .unwrap_or(pret);
                    // A nullable Kotlin return over a PRIMITIVE receiver is the first-class
                    // `Ty::Nullable(prim)`, so a `?:`/null-check on the result is preserved (see
                    // `extension_callable`); the emit boxes it.
                    let ret = if ret.is_jvm_scalar() && ret_nullable {
                        crate::types::Ty::nullable(ret)
                    } else {
                        ret
                    };
                    let ret = match ret_class {
                        Some(meta) if self.value_underlying(meta).is_some() => match meta {
                            Ty::Obj(class, _) => Ty::obj_args(class, ret.type_args()),
                            _ => meta,
                        },
                        _ => ret,
                    };
                    // Source value-parameter NAMES (from `@Metadata`) for named-argument resolution. An
                    // extension's `callable.params` PREPENDS the receiver, but `CallSig.param_names` is the
                    // LOGICAL list (receiver excluded) — `metadata_param_names` returns exactly that (it
                    // aligns past the receiver via the metadata `has_recv` offset), so the names are
                    // `c.params.len() - 1` long. Defaults aren't recovered (named call supplies all).
                    let call_sig = match self.cp.metadata_param_names(&c.owner, meta_name, &params)
                    {
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
                        ret_class,
                        public,
                        receiver_rank: rank as u32,
                        overload_rank: descriptor_narrowing(&c.descriptor) as u32,
                        generic_sig: c.signature.as_deref().and_then(parse_method_gsig),
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
                            default_call: is_default,
                            vararg_elem: None,
                            signature: c.signature.clone(),
                            origin: crate::libraries::Origin::Library,
                        },
                    });
                }
            }
            // `@JvmName`-mangled REDUCTION extensions selected by the receiver's ELEMENT type:
            // `List<Int>.sum()` is the bytecode method `sumOfInt(Iterable<Integer>): int` (Kotlin source
            // name `sum`, `@JvmName` `sumOfInt`). The source name is not a JVM method, so the plain
            // `find_extensions(name)` above misses it; map `name` → each mangled `jvm_name` via `@Metadata`,
            // then bind ONLY the candidate whose generic-signature receiver ELEMENT equals the actual
            // receiver's element — that element match is the disambiguator among the per-element overloads
            // (`sumOfInt`/`sumOfLong`/…), so an unrelated mangled extension never over-matches.
            if let Some(jname) = receiver
                .type_args()
                .first()
                .copied()
                .or_else(|| receiver.array_elem())
                .and_then(ty_simple_name)
                // The `@JvmName` follows kotlinc's `<name>Of<Element>` convention (`sum`→`sumOfInt`,
                // `average`→`averageOfInt`) — the same naming the `sumOf`-by-lambda-return path derives.
                .map(|simple| format!("{name}Of{simple}"))
                .filter(|jname| *jname != name)
            {
                let want_elem = receiver
                    .type_args()
                    .first()
                    .copied()
                    .or_else(|| receiver.array_elem());
                for recv_desc in supertype_descriptors(&self.cp, receiver) {
                    for c in self.cp.find_extensions(&recv_desc, &jname) {
                        // Defensive: the candidate's generic-signature receiver element must equal the
                        // actual receiver's element — bind only the element-appropriate reduction.
                        if want_elem.is_some()
                            && gsig_receiver_element(c.signature.as_deref()) != want_elem
                        {
                            continue;
                        }
                        // A no-argument reduction only: the extension descriptor carries just the RECEIVER
                        // (`sum(Iterable): R` → one param). A same-named lambda overload
                        // (`sumOf(Iterable, Function1): R`) has an extra parameter; skip it here.
                        let (params, pret) = parse_method_desc(&c.descriptor);
                        if params.len() != 1 {
                            continue;
                        }
                        crate::trace_compiler!(
                            "resolve",
                            "reduction {name} -> {} on {recv_desc} ret={pret:?}",
                            c.name
                        );
                        overloads.push(FunctionInfo {
                            kind: FnKind::Extension,
                            receiver: Some(receiver),
                            ret_nullable: false,
                            ret_class: None,
                            public: true,
                            receiver_rank: 0,
                            overload_rank: descriptor_narrowing(&c.descriptor) as u32,
                            generic_sig: c.signature.as_deref().and_then(parse_method_gsig),
                            call_sig: crate::libraries::CallSig::default(),
                            flags: FnFlags {
                                inline: InlineKind::from_flags(false, false),
                                suspend: false,
                            },
                            callable: LibraryCallable {
                                name: c.name.clone(),
                                owner: c.owner.clone(),
                                params,
                                ret: pret,
                                physical_ret: pret,
                                descriptor: c.descriptor.clone(),
                                inline: InlineKind::None,
                                default_call: false,
                                vararg_elem: None,
                                signature: c.signature.clone(),
                                origin: crate::libraries::Origin::Library,
                            },
                        });
                    }
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
                                    let ret_nullable = mf.ret_nullable;
                                    let ret_class = metadata_return_ty(mf.ret_class.as_deref());
                                    let ret = ret_class.unwrap_or(pret);
                                    overloads.push(FunctionInfo {
                                        kind: FnKind::Extension,
                                        receiver: Some(receiver),
                                        ret_nullable,
                                        ret_class,
                                        public: true,
                                        // The value class is the most-specific receiver rung.
                                        receiver_rank: 0,
                                        overload_rank: descriptor_narrowing(&c.descriptor) as u32,
                                        generic_sig: c
                                            .signature
                                            .as_deref()
                                            .and_then(parse_method_gsig),
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
            if let Some(internal) = source_internal_of_ty(receiver) {
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
                        if member_matches_query(&m.name, name) {
                            crate::trace_compiler!(
                                "resolve",
                                "member walk {cn}.{} (rung {rung}) desc={} sig={:?}",
                                m.name,
                                m.descriptor,
                                m.signature
                            );
                            let generic_sig = m.signature.as_deref().and_then(parse_method_gsig);
                            // A `suspend fun` member's physical method appends a `Continuation` parameter
                            // and erases its return to `Object`; present the LOGICAL signature (drop the
                            // continuation, recover the real return from the `Continuation<T>` type
                            // argument in the generic signature) so a normal call resolves. The coroutine
                            // pass re-derives the CPS form for the emit.
                            let suspend = self.cp.is_suspend_method(&cn, &m.name);
                            let params: Vec<Ty> = if suspend {
                                m.params
                                    .split_last()
                                    .map(|(_, rest)| rest.to_vec())
                                    .unwrap_or_default()
                            } else {
                                m.params.clone()
                            };
                            let descriptor = if suspend {
                                strip_continuation_param(&m.descriptor)
                            } else {
                                m.descriptor.clone()
                            };
                            // A `suspend` member's `T?` return is erased twice (to `Object`, then via the
                            // `Continuation<T>` type argument which drops nullability), so recover it from
                            // the class `@Metadata` — the only place a member's return nullability survives.
                            let suspend_ret_nullable = suspend
                                && self.cp.metadata_member_return_nullable(
                                    &cn,
                                    &m.name,
                                    params.len(),
                                );
                            let ret = if suspend {
                                let base = generic_sig
                                    .as_ref()
                                    .and_then(suspend_return_from_gsig)
                                    .unwrap_or(m.ret);
                                if suspend_ret_nullable && base != Ty::Unit && !base.is_nullable() {
                                    Ty::nullable(base)
                                } else {
                                    base
                                }
                            } else {
                                let recovered =
                                    self.member_return(receiver, &m.name, &m.params).or_else(
                                        || generic_sig.as_ref().and_then(concrete_generic_ret),
                                    );
                                crate::trace_compiler!(
                                    "resolve",
                                    "member return {}.{}: recovered={:?} erased={:?} (gsig={})",
                                    receiver.name(),
                                    m.name,
                                    recovered,
                                    m.ret,
                                    generic_sig.is_some()
                                );
                                recovered.unwrap_or(m.ret)
                            };
                            // Source parameter NAMES (from the class's `@Metadata`) for named-argument
                            // resolution. A member's `params` are the logical params (no receiver), so the
                            // names align 1:1 when present. Defaults aren't recovered here (named call
                            // supplies all).
                            let call_sig = match self.cp.metadata_member_param_names(
                                &cn,
                                &m.name,
                                params.len(),
                            ) {
                                Some(names) => crate::libraries::CallSig {
                                    required: params.len(),
                                    param_names: names,
                                    ..Default::default()
                                },
                                _ => crate::libraries::CallSig::default(),
                            };
                            overloads.push(FunctionInfo {
                                kind: FnKind::Member,
                                receiver: Some(receiver),
                                ret_nullable: m.ret_nullable || suspend_ret_nullable,
                                ret_class: None,
                                public: true,
                                receiver_rank: rung,
                                overload_rank: descriptor_narrowing(&m.descriptor) as u32,
                                generic_sig,
                                call_sig,
                                flags: FnFlags {
                                    inline: m.inline,
                                    suspend,
                                },
                                callable: LibraryCallable {
                                    name: m.physical_name.clone().unwrap_or_else(|| m.name.clone()),
                                    owner: m.owner.clone().unwrap_or_else(|| cn.clone()),
                                    params,
                                    ret,
                                    physical_ret: m.physical_ret,
                                    descriptor,
                                    inline: m.inline,
                                    default_call: false,
                                    vararg_elem: None,
                                    signature: m.signature.clone(),
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
                let (mut params, physical_ret) = parse_method_desc_with_field_params(&descriptor);
                let is_default = c.name.ends_with("$default");
                let meta_name = c.name.strip_suffix("$default").unwrap_or(&c.name);
                if is_default && params.len() >= 2 {
                    params.truncate(params.len() - 2);
                }
                // Drop any SYNTHETIC trailing params the JVM descriptor appends beyond the `@Metadata`
                // SOURCE signature — a `@Composable` method's trailing `(Composer, int)` (a `suspend`
                // Continuation is already removed above). `@Metadata` records only the source
                // `value_parameter`s, so its count bounds the source params; keep the descriptor's
                // leading params (their exact types — an extension receiver, a vararg array) and
                // truncate the trailing synthetics. A normal function's metadata count equals the
                // descriptor's param count, so this is a no-op for it (no regression).
                if let Some(keep) = self.cp.metadata_kept_params(&c.owner, meta_name, &params) {
                    if keep < params.len() {
                        params.truncate(keep);
                    }
                }
                let inline_desc = if is_default {
                    method_descriptor(&params, physical_ret)
                } else {
                    c.descriptor.clone()
                };
                let inline = self
                    .cp
                    .is_inline_callable(&c.owner, meta_name, &inline_desc, &params);
                // Source value-parameter NAMES (from `@Metadata`) for named-argument resolution, and the
                // REQUIRED arity (non-defaulted param count) so a call may OMIT trailing defaulted args.
                // A top-level function has no receiver, so the logical params equal the (truncated) source
                // params — only wire names when the count aligns.
                let param_defaults = self
                    .cp
                    .metadata_param_defaults(&c.owner, meta_name, &params)
                    .unwrap_or_default();
                let required = if param_defaults.is_empty() {
                    params.len()
                } else {
                    param_defaults.iter().filter(|d| !**d).count()
                };
                let lambda_receivers = {
                    let recvs = self.cp.metadata_param_recv_funs(&c.owner, meta_name);
                    if recvs.len() == params.len() {
                        recvs
                            .into_iter()
                            .map(|o| o.map(|internal| Ty::obj(&internal)))
                            .collect()
                    } else {
                        Vec::new()
                    }
                };
                let lambda_receiver_params = {
                    let flags = self.cp.metadata_param_recv_fun_flags(&c.owner, meta_name);
                    if flags.len() == params.len() {
                        flags
                    } else {
                        Vec::new()
                    }
                };
                let lambda_materialized = {
                    let flags = self.cp.metadata_param_materialized(&c.owner, meta_name);
                    if flags.len() == params.len() {
                        flags
                    } else {
                        Vec::new()
                    }
                };
                let call_sig = match self.cp.metadata_param_names(&c.owner, meta_name, &params) {
                    Some(names) if names.len() == params.len() => crate::libraries::CallSig {
                        required,
                        param_names: names,
                        param_defaults,
                        lambda_receivers,
                        lambda_receiver_params,
                        lambda_materialized,
                        ..Default::default()
                    },
                    _ => crate::libraries::CallSig {
                        required,
                        param_defaults,
                        lambda_receivers,
                        lambda_receiver_params,
                        lambda_materialized,
                        ..Default::default()
                    },
                };
                let meta_ret = self.cp.metadata_return(&c.owner, meta_name);
                let ret_class =
                    metadata_return_ty(meta_ret.as_ref().and_then(|r| r.class.as_deref()));
                let ret_nullable = meta_ret.as_ref().is_some_and(|r| r.nullable);
                // A suspend method's physical return is erased to `Object`; recover the LOGICAL Kotlin
                // return type from the selected metadata return class (`helper(): Int`), so the call types
                // correctly. The physical (erased) return stays `Object` for the emit.
                let ret = if suspend {
                    ret_class
                        .map(|ty| {
                            if ty.is_jvm_scalar() && meta_ret.as_ref().is_some_and(|r| r.nullable) {
                                super::jvm_class_map::wrapper_internal(ty)
                                    .map(Ty::obj)
                                    .unwrap_or(ty)
                            } else {
                                ty
                            }
                        })
                        .unwrap_or(physical_ret)
                } else {
                    physical_ret
                };
                overloads.push(FunctionInfo {
                    kind: FnKind::TopLevel,
                    receiver: None,
                    ret_nullable,
                    ret_class,
                    public: c.public,
                    receiver_rank: 0,
                    overload_rank: descriptor_narrowing(&c.descriptor) as u32,
                    generic_sig: c.signature.as_deref().and_then(parse_method_gsig),
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
                        default_call: is_default,
                        vararg_elem: None,
                        signature: c.signature.clone(),
                        origin: crate::libraries::Origin::Library,
                    },
                });
            }
        }
        let set = FunctionSet { overloads };
        self.functions_cache.borrow_mut().insert(key, set.clone());
        set
    }
}

impl crate::libraries::TargetRuntime for JvmLibraries {
    fn function_type(&self, arity: usize) -> Option<Ty> {
        Some(Ty::obj(&format!("kotlin/jvm/functions/Function{arity}")))
    }

    fn property_reference_impl(&self, arity: usize, mutable: bool) -> Option<PlatformCtor> {
        let internal = match (arity, mutable) {
            (0, false) => "kotlin/jvm/internal/PropertyReference0Impl",
            (0, true) => "kotlin/jvm/internal/MutablePropertyReference0Impl",
            (1, false) => "kotlin/jvm/internal/PropertyReference1Impl",
            _ => return None,
        };
        Some(PlatformCtor {
            internal: internal.to_string(),
            ctor_desc: "(Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V".to_string(),
        })
    }

    fn property_reference_signature(&self, getter_name: &str, ret: Ty) -> Option<String> {
        Some(format!("{getter_name}(){}", type_descriptor(ret)))
    }

    fn type_descriptor(&self, ty: Ty) -> Option<String> {
        Some(type_descriptor(ty))
    }

    fn ir_type_descriptor(&self, ty: Ty) -> Option<String> {
        Some(type_descriptor(crate::jvm::ir_emit::ir_ty_to_jvm(&ty)))
    }

    fn method_descriptor(&self, params: &[Ty], ret: Ty) -> Option<String> {
        Some(method_descriptor(params, ret))
    }

    fn function_reference_impl_type(&self) -> Option<Ty> {
        Some(Ty::obj("kotlin/jvm/internal/FunctionReferenceImpl"))
    }

    fn enum_member_accessor(&self, name: &str) -> Option<PlatformAccessor> {
        match name {
            "ordinal" => Some(PlatformAccessor {
                name: "ordinal".to_string(),
                descriptor: "()I".to_string(),
            }),
            "name" => Some(PlatformAccessor {
                name: "name".to_string(),
                descriptor: "()Ljava/lang/String;".to_string(),
            }),
            _ => None,
        }
    }

    fn object_instance_field(&self, internal: &str) -> Option<PlatformField> {
        Some(PlatformField {
            owner: internal.to_string(),
            name: "INSTANCE".to_string(),
            descriptor: format!("L{internal};"),
        })
    }

    fn companion_instance_field(
        &self,
        class_internal: &str,
        companion_internal: &str,
        field_name: &str,
    ) -> Option<PlatformField> {
        Some(PlatformField {
            owner: class_internal.to_string(),
            name: field_name.to_string(),
            descriptor: format!("L{companion_internal};"),
        })
    }

    fn mutable_local_ref_type(&self, elem: Ty) -> Option<Ty> {
        let internal = match elem {
            Ty::Int | Ty::UInt => "kotlin/jvm/internal/Ref$IntRef",
            Ty::Long | Ty::ULong => "kotlin/jvm/internal/Ref$LongRef",
            Ty::Float => "kotlin/jvm/internal/Ref$FloatRef",
            Ty::Double => "kotlin/jvm/internal/Ref$DoubleRef",
            Ty::Boolean => "kotlin/jvm/internal/Ref$BooleanRef",
            Ty::Char => "kotlin/jvm/internal/Ref$CharRef",
            Ty::Byte => "kotlin/jvm/internal/Ref$ByteRef",
            Ty::Short => "kotlin/jvm/internal/Ref$ShortRef",
            _ => "kotlin/jvm/internal/Ref$ObjectRef",
        };
        Some(Ty::obj(internal))
    }

    fn scalar_value_repr(&self, ty: Ty) -> Option<Ty> {
        Some(match ty {
            Ty::Int
            | Ty::Byte
            | Ty::Short
            | Ty::Long
            | Ty::Float
            | Ty::Double
            | Ty::Boolean
            | Ty::Char => ty,
            Ty::UInt => Ty::Int,
            Ty::ULong => Ty::Long,
            _ => return None,
        })
    }

    fn unsigned_integer_box_type(&self, ty: Ty) -> Option<Ty> {
        Some(Ty::obj(match ty {
            Ty::UInt => "kotlin/UInt",
            Ty::ULong => "kotlin/ULong",
            _ => return None,
        }))
    }

    fn counted_loop_info(&self, internal: &str) -> Option<CountedLoopInfo> {
        self.counted_loop_info_for_type(internal)
    }

    fn range_construction(&self, lo: Ty, hi: Ty) -> Option<RangeConstruction> {
        let (internal, elem, trailing_nulls) = match (lo, hi) {
            (Ty::Char, Ty::Char) => ("kotlin/ranges/CharRange", Ty::Char, 0),
            (Ty::UInt, Ty::UInt) => ("kotlin/ranges/UIntRange", Ty::UInt, 1),
            (Ty::ULong, Ty::ULong) => ("kotlin/ranges/ULongRange", Ty::ULong, 1),
            (l, r) if l.is_int_range_operand() && r.is_int_range_operand() => {
                ("kotlin/ranges/IntRange", Ty::Int, 0)
            }
            (l, r)
                if (l.is_int_range_operand() || l == Ty::Long)
                    && (r.is_int_range_operand() || r == Ty::Long) =>
            {
                ("kotlin/ranges/LongRange", Ty::Long, 0)
            }
            _ => return None,
        };
        let prim = type_descriptor(elem);
        let marker = "Lkotlin/jvm/internal/DefaultConstructorMarker;";
        let marker_suffix = marker.repeat(trailing_nulls);
        let through = PlatformRangeCtor {
            internal: internal.to_string(),
            ctor_desc: format!("({prim}{prim}{marker_suffix})V"),
            trailing_nulls,
        };
        let until = match elem {
            Ty::UInt => Some(LibraryCallable {
                owner: "kotlin/ranges/URangesKt".to_string(),
                name: "until-J1ME1BU".to_string(),
                params: vec![elem, elem],
                ret: Ty::obj(internal),
                physical_ret: Ty::obj(internal),
                descriptor: format!("({prim}{prim})L{internal};"),
                inline: InlineKind::None,
                default_call: false,
                vararg_elem: None,
                signature: None,
                origin: Origin::Library,
            }),
            Ty::ULong => Some(LibraryCallable {
                owner: "kotlin/ranges/URangesKt".to_string(),
                name: "until-eb3DHEI".to_string(),
                params: vec![elem, elem],
                ret: Ty::obj(internal),
                physical_ret: Ty::obj(internal),
                descriptor: format!("({prim}{prim})L{internal};"),
                inline: InlineKind::None,
                default_call: false,
                vararg_elem: None,
                signature: None,
                origin: Origin::Library,
            }),
            _ if trailing_nulls == 0 => Some(LibraryCallable {
                owner: "kotlin/ranges/RangesKt".to_string(),
                name: "until".to_string(),
                params: vec![elem, elem],
                ret: Ty::obj(internal),
                physical_ret: Ty::obj(internal),
                descriptor: format!("({prim}{prim})L{internal};"),
                inline: InlineKind::None,
                default_call: false,
                vararg_elem: None,
                signature: None,
                origin: Origin::Library,
            }),
            _ => None,
        };
        Some(RangeConstruction {
            elem,
            result: Ty::obj(internal),
            through,
            until,
        })
    }

    fn suspend_cps_descriptor(&self, logical_descriptor: &str) -> Option<String> {
        let close = logical_descriptor.rfind(')')?;
        Some(format!(
            "{}Lkotlin/coroutines/Continuation;)Ljava/lang/Object;",
            &logical_descriptor[..close]
        ))
    }

    fn runtime_callable(&self, op: RuntimeOp, ty: Ty) -> Option<LibraryCallable> {
        let callable = |owner: &str,
                        name: &str,
                        params: Vec<Ty>,
                        ret: Ty,
                        physical_ret: Ty,
                        descriptor: String| {
            Some(LibraryCallable {
                owner: owner.to_string(),
                name: name.to_string(),
                params,
                ret,
                physical_ret,
                descriptor,
                inline: InlineKind::None,
                default_call: false,
                vararg_elem: None,
                signature: None,
                origin: crate::libraries::Origin::Library,
            })
        };

        match op {
            RuntimeOp::UnsignedBox | RuntimeOp::UnsignedUnbox => {
                let (owner, prim, repr) = match ty {
                    Ty::UInt => ("kotlin/UInt", "I", Ty::Int),
                    Ty::ULong => ("kotlin/ULong", "J", Ty::Long),
                    _ => return None,
                };
                match op {
                    RuntimeOp::UnsignedBox => callable(
                        owner,
                        "box-impl",
                        vec![ty],
                        Ty::obj(owner),
                        Ty::obj(owner),
                        format!("({prim})L{owner};"),
                    ),
                    RuntimeOp::UnsignedUnbox => callable(
                        owner,
                        "unbox-impl",
                        vec![Ty::obj(owner)],
                        ty,
                        repr,
                        format!("(){prim}"),
                    ),
                    _ => unreachable!(),
                }
            }
            RuntimeOp::UnsignedCompare
            | RuntimeOp::UnsignedDivide
            | RuntimeOp::UnsignedRemainder
            | RuntimeOp::UnsignedToString => {
                let (owner, prim, repr) = match ty {
                    Ty::UInt => ("java/lang/Integer", "I", Ty::Int),
                    Ty::ULong => ("java/lang/Long", "J", Ty::Long),
                    _ => return None,
                };
                let (name, params, ret, descriptor) = match op {
                    RuntimeOp::UnsignedCompare => (
                        "compareUnsigned",
                        vec![ty, ty],
                        Ty::Int,
                        format!("({prim}{prim})I"),
                    ),
                    RuntimeOp::UnsignedDivide => (
                        "divideUnsigned",
                        vec![ty, ty],
                        ty,
                        format!("({prim}{prim}){prim}"),
                    ),
                    RuntimeOp::UnsignedRemainder => (
                        "remainderUnsigned",
                        vec![ty, ty],
                        ty,
                        format!("({prim}{prim}){prim}"),
                    ),
                    RuntimeOp::UnsignedToString => (
                        "toUnsignedString",
                        vec![ty],
                        Ty::String,
                        format!("({prim})Ljava/lang/String;"),
                    ),
                    _ => unreachable!(),
                };
                callable(owner, name, params, ret, repr, descriptor)
            }
            RuntimeOp::UIntToLong if ty == Ty::UInt => callable(
                "java/lang/Integer",
                "toUnsignedLong",
                vec![Ty::UInt],
                Ty::Long,
                Ty::Long,
                "(I)J".to_string(),
            ),
            RuntimeOp::UIntToLong => None,
            RuntimeOp::PrimitiveCompare if ty != Ty::Boolean => {
                let cmp_ty = match ty {
                    Ty::Byte | Ty::Short | Ty::Char => Ty::Int,
                    t => t,
                };
                let (cmp_owner, cmp_prim) = match cmp_ty {
                    Ty::Int => ("java/lang/Integer", "I"),
                    Ty::Long => ("java/lang/Long", "J"),
                    Ty::Float => ("java/lang/Float", "F"),
                    Ty::Double => ("java/lang/Double", "D"),
                    _ => return None,
                };
                callable(
                    cmp_owner,
                    "compare",
                    vec![cmp_ty, cmp_ty],
                    Ty::Int,
                    Ty::Int,
                    format!("({cmp_prim}{cmp_prim})I"),
                )
            }
            RuntimeOp::PrimitiveCompare => None,
            RuntimeOp::HashCode => {
                let (owner, desc, param) = match ty {
                    Ty::Int => ("java/lang/Integer", "(I)I", Ty::Int),
                    Ty::Short => ("java/lang/Short", "(S)I", Ty::Short),
                    Ty::Byte => ("java/lang/Byte", "(B)I", Ty::Byte),
                    Ty::Char => ("java/lang/Character", "(C)I", Ty::Char),
                    Ty::Boolean => ("java/lang/Boolean", "(Z)I", Ty::Boolean),
                    Ty::Long => ("java/lang/Long", "(J)I", Ty::Long),
                    Ty::Double => ("java/lang/Double", "(D)I", Ty::Double),
                    Ty::Float => ("java/lang/Float", "(F)I", Ty::Float),
                    _ => (
                        "java/util/Objects",
                        "(Ljava/lang/Object;)I",
                        Ty::obj("kotlin/Any"),
                    ),
                };
                callable(
                    owner,
                    "hashCode",
                    vec![param],
                    Ty::Int,
                    Ty::Int,
                    desc.to_string(),
                )
            }
            RuntimeOp::ArrayToString => {
                let desc = match ty.non_null().obj_internal()? {
                    "kotlin/BooleanArray" => "([Z)Ljava/lang/String;",
                    "kotlin/CharArray" => "([C)Ljava/lang/String;",
                    "kotlin/ByteArray" => "([B)Ljava/lang/String;",
                    "kotlin/ShortArray" => "([S)Ljava/lang/String;",
                    "kotlin/IntArray" => "([I)Ljava/lang/String;",
                    "kotlin/LongArray" => "([J)Ljava/lang/String;",
                    "kotlin/FloatArray" => "([F)Ljava/lang/String;",
                    "kotlin/DoubleArray" => "([D)Ljava/lang/String;",
                    "kotlin/Array" => "([Ljava/lang/Object;)Ljava/lang/String;",
                    _ => return None,
                };
                callable(
                    "java/util/Arrays",
                    "toString",
                    vec![ty],
                    Ty::String,
                    Ty::String,
                    desc.to_string(),
                )
            }
            RuntimeOp::ArrayCopyOf => {
                let desc = match ty.non_null().obj_internal()? {
                    "kotlin/BooleanArray" => "([ZI)[Z",
                    "kotlin/CharArray" => "([CI)[C",
                    "kotlin/ByteArray" => "([BI)[B",
                    "kotlin/ShortArray" => "([SI)[S",
                    "kotlin/IntArray" => "([II)[I",
                    "kotlin/LongArray" => "([JI)[J",
                    "kotlin/FloatArray" => "([FI)[F",
                    "kotlin/DoubleArray" => "([DI)[D",
                    "kotlin/Array" => "([Ljava/lang/Object;I)[Ljava/lang/Object;",
                    _ => return None,
                };
                callable(
                    "java/util/Arrays",
                    "copyOf",
                    vec![ty, Ty::Int],
                    ty,
                    ty,
                    desc.to_string(),
                )
            }
            RuntimeOp::StartCoroutine => callable(
                "kotlin/coroutines/ContinuationKt",
                "startCoroutine",
                vec![
                    Ty::obj("kotlin/Function1"),
                    Ty::obj("kotlin/coroutines/Continuation"),
                ],
                Ty::Unit,
                Ty::Unit,
                "(Lkotlin/jvm/functions/Function1;Lkotlin/coroutines/Continuation;)V".to_string(),
            ),
            RuntimeOp::ThrowOnFailure => callable(
                "kotlin/ResultKt",
                "throwOnFailure",
                vec![Ty::obj("kotlin/Any")],
                Ty::Unit,
                Ty::Unit,
                "(Ljava/lang/Object;)V".to_string(),
            ),
            RuntimeOp::CoroutineSuspended => callable(
                "kotlin/coroutines/intrinsics/IntrinsicsKt",
                "getCOROUTINE_SUSPENDED",
                vec![],
                Ty::obj("kotlin/Any"),
                Ty::obj("kotlin/Any"),
                "()Ljava/lang/Object;".to_string(),
            ),
        }
    }

    fn runtime_ctor(&self, ctor: RuntimeCtor) -> Option<PlatformCtor> {
        match ctor {
            RuntimeCtor::IllegalStateException => Some(PlatformCtor {
                internal: "java/lang/IllegalStateException".to_string(),
                ctor_desc: "(Ljava/lang/String;)V".to_string(),
            }),
            RuntimeCtor::AssertionError => Some(PlatformCtor {
                internal: "java/lang/AssertionError".to_string(),
                ctor_desc: "(Ljava/lang/String;)V".to_string(),
            }),
        }
    }

    fn is_reified_assert_fails_with_default(&self, callable: &LibraryCallable) -> bool {
        callable.owner == "kotlin/test/AssertionsKt__AssertionsKt"
            && callable.name == "assertFailsWith$default"
    }
}

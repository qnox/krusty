//! The JVM implementation of the [`SymbolSource`] abstraction: resolves symbols from a `.class`-jar
//! classpath (the bytecode target). All classpath reads, JVM method-descriptor parsing, and
//! `java/lang ↔ kotlin` name normalization live here — the front end (`resolve`, `ir_lower`) sees
//! only Kotlin-level `Ty`s and opaque descriptor tokens through the trait.

use super::classpath::{
    kotlin_name_to_ty, kotlin_type_name_to_ty, metadata_return_info, Classpath,
};
use super::classreader::{ConstVal, FieldSig};
use super::jvm_class_map::to_kotlin_internal;
use super::metadata;
use crate::jvm::names::{method_descriptor, property_getter_name, type_descriptor};
use crate::libraries::{
    FnFlags, FnKind, FunctionInfo, FunctionSet, GenericSig, InlineKind, LibConst, LibraryCallable,
    LibraryConst, LibraryMember, LibraryType, PropKind, PropertyInfo, PropertySet, ReturnInfo,
    SemanticPlatform, Visibility,
};
use crate::runtime::{
    CountedLoopInfo, PlatformAccessor, PlatformCtor, PlatformField, PlatformRangeCtor,
    RangeConstruction, RuntimeCtor, RuntimeOp,
};
use crate::symbol_resolver::{arg_fits, ty_subst, ty_subst_all};
use crate::symbol_source::SymbolSource;
use crate::types::{intern, type_name, Ty, TypeName, TypeNameList};

/// The `kotlin/…Array` classifier name for an array `Ty` — a primitive specialized array
/// (`kotlin/IntArray`) or the boxed `Array<T>` (`kotlin/Array`). `None` for a non-array type. Arrays are
/// `Obj` types carrying their class name directly, so this is a straight class-name match.
fn array_kotlin_fq(ty: Ty) -> Option<&'static str> {
    let n = ty.non_null().obj_internal()?;
    if n.matches("kotlin/BooleanArray") {
        Some("kotlin/BooleanArray")
    } else if n.matches("kotlin/CharArray") {
        Some("kotlin/CharArray")
    } else if n.matches("kotlin/ByteArray") {
        Some("kotlin/ByteArray")
    } else if n.matches("kotlin/ShortArray") {
        Some("kotlin/ShortArray")
    } else if n.matches("kotlin/IntArray") {
        Some("kotlin/IntArray")
    } else if n.matches("kotlin/LongArray") {
        Some("kotlin/LongArray")
    } else if n.matches("kotlin/FloatArray") {
        Some("kotlin/FloatArray")
    } else if n.matches("kotlin/DoubleArray") {
        Some("kotlin/DoubleArray")
    } else if n.matches("kotlin/Array") {
        Some("kotlin/Array")
    } else {
        None
    }
}

/// The JVM platform's contribution to Kotlin's default imports. The language-level `kotlin.*` set is
/// composed with this list in `import_wildcards` and in the seed filter, so neither list is duplicated.
const PLATFORM_DEFAULT_IMPORT_PACKAGES: &[&str] = &["java.lang", "kotlin.jvm"];

/// A platform backed by a JVM classpath (dirs + jars + the JDK jimage). The classpath is shared
/// (`Rc`) with the JVM backend/emitter so the bytecode inliner reads inline-function bodies through
/// the same lazily-populated caches — all within the `jvm` module, never through the `SymbolSource`
/// abstraction.
pub struct JvmLibraries {
    cp: std::rc::Rc<Classpath>,
}

impl JvmLibraries {
    /// The TOP-LEVEL (receiver-less) function overloads of `name` — `listOf`/`run`/`println`/…
    /// each with its inline/`@InlineOnly` flags and logical (continuation-stripped) suspend
    /// signature. The building block `resolve_symbols` uses so a top-level name resolves through
    /// the ONE fqn seam without the removed receiver-indexed `functions()` query. `find_top_level`
    /// also surfaces an extension's compiled form; each is classified honestly by its metadata
    /// receiver, so a caller filters by `FnKind` as needed.
    fn top_level_overloads(&self, name: &str) -> Vec<FunctionInfo> {
        let mut overloads = Vec::new();
        // Top-level (receiver-less) functions of this name — `listOf`, `run`, `println`, … — each with
        // its inline/`@InlineOnly` flags in one place.
        for c in self.cp.find_top_level(name) {
            let is_default = c.name.ends_with("$default");
            let meta_name = c.name.strip_suffix("$default").unwrap_or(&c.name);
            // Suspend-ness lives on the SOURCE function's `@Metadata`; a `$default` synthetic is not in
            // metadata, so detect it via the stripped `meta_name` — otherwise a suspend function's
            // `withLock$default` keeps its `Continuation` param and no normal call shape resolves.
            let suspend = self.cp.is_suspend_method_name(c.owner, meta_name);
            // A `suspend fun`'s physical method appends a `Continuation` parameter and erases the
            // return to `Object`; present the LOGICAL signature (drop the continuation) so a normal
            // call resolves. The coroutine pass re-derives the CPS form for the emitted call.
            let descriptor = if suspend {
                strip_continuation_param(&c.descriptor)
            } else {
                c.descriptor.clone()
            };
            let (mut params, physical_ret) = parse_method_desc_with_field_params(&descriptor);
            if is_default && params.len() >= 2 {
                params.truncate(params.len() - 2);
            }
            // The CPS `Continuation` is emit-only — never part of the function signature `@Metadata`
            // records. For a non-`$default` suspend method it is trailing and already gone (stripped
            // from `descriptor` above); a `$default` synthetic spells it before the mask/marker, so it
            // survived into the logical params. Drop it here so the params ARE the source signature and
            // align against metadata; the emit `descriptor` keeps the physical CPS form.
            if suspend
                && params
                    .last()
                    .and_then(|p| p.obj_internal())
                    .is_some_and(|n| n.matches("kotlin/coroutines/Continuation"))
            {
                params.pop();
            }
            // Drop any SYNTHETIC trailing params the JVM descriptor appends beyond the `@Metadata`
            // SOURCE signature — a `@Composable` method's trailing `(Composer, int)` (a `suspend`
            // Continuation is already removed above). `@Metadata` records only the source
            // `value_parameter`s, so its count bounds the source params; keep the descriptor's
            // leading params (their exact types — an extension receiver, a vararg array) and
            // truncate the trailing synthetics. A normal function's metadata count equals the
            // descriptor's param count, so this is a no-op for it (no regression).
            let owner_rendered = c.owner.render();
            let meta = self.cp.metadata_call_facts(
                &owner_rendered,
                meta_name,
                &params,
                &physical_ret,
                false,
            );
            if let Some(keep) = meta.kept_params {
                if keep < params.len() {
                    params.truncate(keep);
                }
            }
            let inline_desc = if is_default {
                method_descriptor(&params, physical_ret)
            } else {
                c.descriptor.clone()
            };
            let inline =
                self.cp
                    .is_inline_callable(&owner_rendered, meta_name, &inline_desc, &params);
            let call_sig = meta.call_sig;
            let ret_metadata = meta.ret;
            let ret = if suspend {
                match ret_metadata.class {
                    Some(ty) if ty.is_jvm_scalar() && ret_metadata.nullable => {
                        super::jvm_class_map::wrapper_internal(ty)
                            .map(Ty::obj)
                            .unwrap_or(ty)
                    }
                    Some(ty) => ty,
                    None => physical_ret,
                }
            } else {
                physical_ret
            };
            let inline_kind = InlineKind::from_flags(inline, inline && !c.public);
            let callable = LibraryCallable {
                inline: inline_kind,
                suspend,
                default_call: is_default,
                signature: c.signature.clone(),
                ..LibraryCallable::library(
                    c.owner,
                    c.name.clone(),
                    params,
                    ret,
                    physical_ret,
                    descriptor,
                )
            };
            // The static-method index (`find_top_level`) also surfaces an EXTENSION's compiled form
            // (`T.run` → `run(receiver, block)`); classify by the metadata signature's receiver so it is
            // an `Extension`, not a receiver-less `TopLevel`. Extension resolution reaches it through the
            // by-receiver query; keeping the kind honest is what lets the top-level queries ignore it
            // without per-call-site receiver checks.
            let generic_sig = self.callable_generic_sig(
                &owner_rendered,
                &c.name,
                &c.descriptor,
                c.signature.as_deref(),
                false,
            );
            let kind = if generic_sig.as_ref().is_some_and(|g| g.receiver.is_some()) {
                FnKind::Extension
            } else {
                FnKind::TopLevel
            };
            overloads.push(FunctionInfo {
                ret: ret_metadata,
                visibility: Visibility::from_public(c.public),
                overload_rank: descriptor_narrowing(&c.descriptor) as u32,
                generic_sig,
                call_sig,
                flags: FnFlags {
                    inline: inline_kind,
                    suspend,
                },
                ..FunctionInfo::plain(kind, None, callable)
            });
        }
        overloads
    }

    pub fn new(cp: std::rc::Rc<Classpath>) -> JvmLibraries {
        JvmLibraries { cp }
    }

    fn const_fields<F>(
        fields: &[FieldSig],
        mut ty: F,
    ) -> std::collections::HashMap<String, LibraryConst>
    where
        F: FnMut(&FieldSig) -> Option<Ty>,
    {
        fields
            .iter()
            .filter_map(|f| {
                let ty = ty(f)?;
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

    fn primitive_companion_consts_for_type(
        &self,
        internal: &str,
    ) -> std::collections::HashMap<String, LibraryConst> {
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
        Self::const_fields(&ci.fields, |f| Some(field_desc_to_ty(&f.descriptor)))
    }

    fn metadata_static_companion_consts_for_class(
        &self,
        ci: &crate::jvm::classreader::ClassInfo,
    ) -> std::collections::HashMap<String, LibraryConst> {
        let internal = ci.this_class();
        let companion_internal = format!("{internal}$Companion");
        let Some(companion) = self.cp.find(&companion_internal) else {
            return std::collections::HashMap::new();
        };
        let prop_rets: std::collections::HashMap<_, _> =
            super::metadata::class_properties(&companion)
                .into_iter()
                .filter_map(|p| p.ret_class.map(|ret| (p.name, ret)))
                .collect();
        Self::const_fields(&ci.fields, |f| {
            prop_rets
                .get(&f.name)
                .map(|&ret| kotlin_type_name_to_ty(ret))
        })
    }

    fn companion_consts_for_class(
        &self,
        ci: &crate::jvm::classreader::ClassInfo,
    ) -> std::collections::HashMap<String, LibraryConst> {
        let internal = ci.this_class();
        let mut out = self.primitive_companion_consts_for_type(&internal);
        out.extend(self.metadata_static_companion_consts_for_class(ci));
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

    fn counted_loop_info_for_name(&self, internal: TypeName) -> Option<CountedLoopInfo> {
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
        Some(if internal.matches("kotlin/ranges/IntRange") {
            unit_step(Ty::Int, "()I", "()I")
        } else if internal.matches("kotlin/ranges/LongRange") {
            unit_step(Ty::Long, "()J", "()J")
        } else if internal.matches("kotlin/ranges/IntProgression") {
            progression(Ty::Int, "()I", "()I", "()I", Ty::Int)
        } else if internal.matches("kotlin/ranges/LongProgression") {
            progression(Ty::Long, "()J", "()J", "()J", Ty::Long)
        } else if internal.matches("kotlin/ranges/CharProgression") {
            progression(Ty::Char, "()C", "()C", "()I", Ty::Int)
        } else if internal.matches("kotlin/ranges/UIntRange") {
            CountedLoopInfo {
                elem: Ty::UInt,
                first: self.member_accessor_by_prefix("kotlin/ranges/UIntRange", "getFirst-")?,
                last: self.member_accessor_by_prefix("kotlin/ranges/UIntRange", "getLast-")?,
                step: None,
            }
        } else if internal.matches("kotlin/ranges/ULongRange") {
            CountedLoopInfo {
                elem: Ty::ULong,
                first: self.member_accessor_by_prefix("kotlin/ranges/ULongRange", "getFirst-")?,
                last: self.member_accessor_by_prefix("kotlin/ranges/ULongRange", "getLast-")?,
                step: None,
            }
        } else if internal.matches("kotlin/ranges/UIntProgression") {
            CountedLoopInfo {
                elem: Ty::UInt,
                first: self
                    .member_accessor_by_prefix("kotlin/ranges/UIntProgression", "getFirst-")?,
                last: self
                    .member_accessor_by_prefix("kotlin/ranges/UIntProgression", "getLast-")?,
                step: Some((Self::range_accessor("getStep", "()I"), Ty::Int)),
            }
        } else if internal.matches("kotlin/ranges/ULongProgression") {
            CountedLoopInfo {
                elem: Ty::ULong,
                first: self
                    .member_accessor_by_prefix("kotlin/ranges/ULongProgression", "getFirst-")?,
                last: self
                    .member_accessor_by_prefix("kotlin/ranges/ULongProgression", "getLast-")?,
                step: Some((Self::range_accessor("getStep", "()J"), Ty::Long)),
            }
        } else {
            return None;
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
        q.push_back((
            super::jvm_class_map::to_jvm_type_name(start),
            start_args.to_vec(),
        ));
        while let Some((internal, targs)) = q.pop_front() {
            if !seen.insert(internal) {
                continue;
            }
            let Some(ci) = self.cp.find_name(internal) else {
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
                return Some(ty_subst(gsig.ret, &binds));
            }
            if let Some(supers) = supers {
                for sup in supers {
                    if let Ty::Obj(sup_internal, sup_args) = sup {
                        let sup_targs = ty_subst_all(sup_args, &binds);
                        q.push_back((
                            super::jvm_class_map::to_jvm_type_name(sup_internal),
                            sup_targs,
                        ));
                    }
                }
            } else {
                for i in ci.interfaces.iter_ids().chain(ci.super_class) {
                    q.push_back((i, vec![]));
                }
            }
        }
        None
    }

    /// The generic signature for `owner.jvm_name`, metadata-primary. When `@Metadata` DESCRIBES this
    /// function (a Kotlin callable), its metadata gsig is authoritative — the JVM-agnostic, Kotlin-faithful
    /// signature (nullability, variance, Kotlin type identities, and no synthetic `suspend` Continuation /
    /// `@Composable` params, which are emit-only). There is NO fallback to the JVM `Signature` attribute
    /// here: a metadata function that fails to decode is a decoder BUG to fix, not something to paper over
    /// with the erased-ish JVM sig. The `Signature` attribute is consulted ONLY when `@Metadata` has no
    /// record for the name — a Java class, a synthetic/bridge method, or a facade part metadata omits.
    fn callable_generic_sig(
        &self,
        owner: &str,
        jvm_name: &str,
        jvm_desc: &str,
        jvm_sig: Option<&str>,
        is_extension: bool,
    ) -> Option<GenericSig> {
        // Prefer the metadata generic signature, disambiguating overloads by aligning the metadata value
        // parameters to this callable's JVM descriptor (kotlinc omits `method_signature` when it equals the
        // computed default, so `MetaFn.jvm_desc` is usually absent — a name-only match would hand one
        // overload the WRONG signature). Only when `@Metadata` has no FUNCTION for the name (a Java method,
        // a synthetic, or a PROPERTY getter — recorded as a property, not a function) do we read the JVM
        // `Signature`, which uses the legacy receiver-in-`params[0]` shape.
        let (desc_params, desc_ret) = parse_method_desc_with_field_params(jvm_desc);
        if let Some(gsig) = self
            .cp
            .aligned_generic_sig(owner, jvm_name, &desc_params, &desc_ret)
        {
            // Metadata DESCRIBES this class's function — it is the authoritative signature and there is NO
            // fallback to the JVM `Signature`. A failure to align/decode here is a bug to fix in the reader.
            return gsig;
        }
        // No `@Metadata` FUNCTION for the name — the JVM `Signature` is the only source. Its extension
        // receiver is the leading value parameter; move it to the `receiver` ATTRIBUTE so the signature has
        // the same shape as a metadata one (consumers bind the receiver separately, not as a value param).
        let gsig = jvm_sig.and_then(parse_method_gsig)?;
        Some(
            if is_extension && gsig.receiver.is_none() && !gsig.params.is_empty() {
                let mut params = gsig.params;
                let receiver = Some(params.remove(0));
                GenericSig {
                    receiver,
                    params,
                    ..gsig
                }
            } else {
                gsig
            },
        )
    }

    /// The type-parameter bindings a receiver induces at `target_internal` (`Repo<Cfg>` at `lib/Repo` →
    /// `{T: Cfg}`), by walking the generic hierarchy from the receiver and propagating each class's type
    /// arguments through every `extends`/`implements` edge — the same walk `member_return` performs, but
    /// exposed so a `suspend` member's return (recovered from `Continuation<T>`, which `member_return`
    /// cannot use because the JVM return is erased to `Object`) can be substituted under the same bindings.
    /// Empty when the receiver carries no type arguments or `target_internal` is not reached.
    fn receiver_type_bindings(
        &self,
        receiver: Ty,
        target_internal: &str,
    ) -> std::collections::HashMap<String, Ty> {
        let Ty::Obj(start, start_args) = receiver else {
            return std::collections::HashMap::new();
        };
        if start_args.is_empty() {
            return std::collections::HashMap::new();
        }
        let target = super::jvm_class_map::to_jvm_type_name(type_name(target_internal));
        let mut seen = std::collections::HashSet::new();
        let mut q = std::collections::VecDeque::new();
        q.push_back((
            super::jvm_class_map::to_jvm_type_name(start),
            start_args.to_vec(),
        ));
        while let Some((internal, targs)) = q.pop_front() {
            if !seen.insert(internal) {
                continue;
            }
            let Some(ci) = self.cp.find_name(internal) else {
                continue;
            };
            let (formals, supers) = ci.signature.as_deref().and_then(parse_class_gsig).unzip();
            let formals = formals.unwrap_or_default();
            let binds: std::collections::HashMap<String, Ty> =
                formals.iter().cloned().zip(targs.iter().copied()).collect();
            if internal == target {
                return binds;
            }
            if let Some(supers) = supers {
                for sup in supers {
                    if let Ty::Obj(sup_internal, sup_args) = sup {
                        let sup_targs = ty_subst_all(sup_args, &binds);
                        q.push_back((
                            super::jvm_class_map::to_jvm_type_name(sup_internal),
                            sup_targs,
                        ));
                    }
                }
            } else {
                for i in ci.interfaces.iter_ids().chain(ci.super_class) {
                    q.push_back((i, vec![]));
                }
            }
        }
        std::collections::HashMap::new()
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

    fn value_companion_fns_for_class(
        &self,
        ci: &crate::jvm::classreader::ClassInfo,
        inline: bool,
    ) -> Vec<crate::libraries::CompanionFn> {
        if !inline {
            return Vec::new();
        }
        let internal = ci.this_class();
        let Some(companion_field) = metadata::class_companion_name(ci) else {
            return Vec::new();
        };
        let companion_internal = format!("{internal}${companion_field}");
        let Some(comp_ci) = self.cp.find(&companion_internal) else {
            return Vec::new();
        };
        metadata::class_functions(&comp_ci)
            .into_iter()
            .filter(|m| m.is_public)
            .filter_map(|m| {
                let descriptor = m.jvm_desc?;
                let (params, _) = parse_method_desc(descriptor);
                Some(crate::libraries::CompanionFn {
                    class_internal: type_name(&internal),
                    companion_internal: type_name(&companion_internal),
                    companion_field: companion_field.clone(),
                    callable: LibraryCallable {
                        // The logical return is the value class itself (`Result`); its type argument
                        // stays erased, matching kotlinc (a generic companion result flows as the
                        // erased underlying).
                        inline: InlineKind::MustInline,
                        ..LibraryCallable::library(
                            type_name(&companion_internal),
                            m.jvm_name,
                            params,
                            Ty::obj(&internal),
                            Ty::obj("kotlin/Any"),
                            descriptor,
                        )
                    },
                })
            })
            .collect()
    }

    fn value_class_metadata_members_for_class(
        &self,
        ci: &crate::jvm::classreader::ClassInfo,
        inline: bool,
        meta_fns: &[metadata::MetaFn],
    ) -> Vec<LibraryMember> {
        if !inline {
            return Vec::new();
        }
        meta_fns
            .iter()
            .filter(|m| m.is_public && !m.is_extension)
            .filter_map(|m| {
                let descriptor = m.jvm_desc?.to_string();
                let (params, physical_ret) = parse_method_desc(&descriptor);
                // Value-class implementation methods are static and take the erased receiver as their
                // first JVM parameter. Source member resolution sees only the value parameters.
                let logical_params = params.get(1..).unwrap_or(&[]).to_vec();
                let ret = metadata_return_info(m.ret_class, m.ret_nullable).apply(physical_ret);
                let mut member =
                    LibraryMember::new(m.kotlin_name.clone(), logical_params, ret, descriptor);
                member.owner = Some(type_name(&ci.this_class()));
                member.physical_name = Some(m.jvm_name.clone());
                member.physical_ret = physical_ret;
                member.ret_nullable = m.ret_nullable;
                member.inline = InlineKind::from_flags(m.is_inline, m.is_inline);
                Some(member)
            })
            .collect()
    }

    /// Every value-class-TYPED property of `ci`, keyed by SOURCE property name. Such a property's getter is
    /// `@JvmName`-mangled (`getId-<hash>`) and its physical return erases to the value class's underlying,
    /// so ordinary getter resolution misses it. Each member carries the mangled getter name + physical
    /// descriptor but the LOGICAL value-class return type from `@Metadata`, so `h.id` types as the value
    /// class. An ordinary (non-value-class) property is skipped — it keeps its normal getter path.
    fn value_class_property_members_for_class(
        &self,
        ci: &crate::jvm::classreader::ClassInfo,
    ) -> Vec<(String, LibraryMember)> {
        let internal = ci.this_class();
        metadata::class_properties(ci)
            .into_iter()
            .filter_map(|p| {
                let logical = p.ret_class?;
                // Only a value-class-typed property (mangled getter); an ordinary property keeps its
                // normal getter path. Test value-class-ness via the `@JvmInline` `@Metadata` flag DIRECTLY
                // (not `value_underlying`, which would call `resolve_type` and recurse mid-build).
                let lci = self.cp.find_name(logical)?;
                metadata::class_inline(&lci)?;
                let mut chars = p.name.chars();
                let cap = chars.next()?.to_uppercase().collect::<String>();
                let getter = format!("get{cap}{}", chars.as_str());
                let dashed = format!("{getter}-");
                let m = ci
                    .methods
                    .iter()
                    .find(|mm| mm.name == getter || mm.name.starts_with(&dashed))?;
                crate::trace_compiler!(
                    "value_classes",
                    "value-class property {}.{} -> getter {} : {}",
                    internal,
                    p.name,
                    m.name,
                    logical
                );
                let mut member = LibraryMember::new(
                    m.name.clone(),
                    vec![],
                    Ty::obj_name(logical),
                    m.descriptor.clone(),
                );
                member.owner = Some(type_name(&internal));
                Some((p.name, member))
            })
            .collect()
    }
}

/// Parse one JVM generic-signature type off the front of `s` into a signature [`Ty`], returning
/// `(node, rest)`. A type variable becomes a [`Ty::TyParam`] (`kotlin/Any` bound).
fn parse_gsig(s: &str) -> Option<(Ty, &str)> {
    let b = s.as_bytes();
    match *b.first()? {
        b'T' => {
            let end = s.find(';')?;
            Some((
                Ty::ty_param(&s[1..end], Ty::obj("kotlin/Any")),
                &s[end + 1..],
            ))
        }
        b'[' => {
            let (inner, rest) = parse_gsig(&s[1..])?;
            Some((Ty::array(inner), rest))
        }
        b'L' => {
            let lt = s.find('<');
            let semi = s.find(';')?;
            let name_end = match lt {
                Some(i) if i < semi => i,
                _ => semi,
            };
            let internal = intern(to_kotlin_internal(&s[1..name_end]));
            if let Some(i) = lt.filter(|&i| i < semi) {
                let mut rest = &s[i + 1..];
                let mut args = Vec::new();
                while !rest.starts_with('>') {
                    if let Some(stripped) = rest.strip_prefix('*') {
                        args.push(Ty::obj("kotlin/Any"));
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
                    let ret = gsig_unbox_wrapper(args.pop()?);
                    let params: Vec<Ty> = args.into_iter().map(gsig_unbox_wrapper).collect();
                    Ty::fun(params, ret)
                } else {
                    Ty::obj_args(internal, &args)
                };
                Some((node, after))
            } else {
                Some((Ty::obj(internal), &s[semi + 1..]))
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
            Some((t, &s[1..]))
        }
    }
}

fn gsig_unbox_wrapper(g: Ty) -> Ty {
    let Ty::Obj(internal, _) = g else {
        return g;
    };
    if internal.matches("java/lang/Integer") || internal.matches("kotlin/Int") {
        Ty::Int
    } else if internal.matches("java/lang/Long") || internal.matches("kotlin/Long") {
        Ty::Long
    } else if internal.matches("java/lang/Short") || internal.matches("kotlin/Short") {
        Ty::Short
    } else if internal.matches("java/lang/Byte") || internal.matches("kotlin/Byte") {
        Ty::Byte
    } else if internal.matches("java/lang/Character") || internal.matches("kotlin/Char") {
        Ty::Char
    } else if internal.matches("java/lang/Boolean") || internal.matches("kotlin/Boolean") {
        Ty::Boolean
    } else if internal.matches("java/lang/Double") || internal.matches("kotlin/Double") {
        Ty::Double
    } else if internal.matches("java/lang/Float") || internal.matches("kotlin/Float") {
        Ty::Float
    } else {
        g
    }
}

/// Parse a leading `<Name:Bound...>` formal-type-parameter block, returning the formal names and the
/// remaining signature. No block means empty names and unchanged input.
/// The generic type-parameter NAMES of a method `Signature` (`<T:…;U:…>(…)…` → `["T", "U"]`), for
/// mapping a call's explicit type arguments onto reified type parameters at an inline splice.
pub(crate) fn signature_formals(sig: &str) -> Vec<String> {
    parse_formals(sig).0
}

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
    // The JVM `Signature` attribute is the fallback for NON-metadata callables (Java methods): an instance
    // method has no receiver parameter and a static none either, so the receiver is not modeled here.
    Some(GenericSig {
        formals,
        receiver: None,
        params,
        ret,
    })
}

/// A member's return type recovered from its generic signature ONLY when it is fully CONCRETE (carries
/// type arguments, none of which is a free type variable) — `all(): List<Item>` → `List<Item>`. This lets
/// a member of a NON-generic receiver still carry its return's type arguments (which `member_return`
/// skips, as it only propagates the receiver's own arguments). A return naming a type variable
/// (`fun <T> load(): T`, `List<E>.get(): E`) is NOT recovered here — it stays erased / is bound by
/// `member_return` under the receiver's arguments.
fn concrete_generic_ret(gsig: &GenericSig) -> Option<Ty> {
    fn is_concrete(g: Ty) -> bool {
        match g {
            Ty::TyParam(..) => false,
            Ty::Fun(fsig) => fsig.params.iter().all(|p| is_concrete(*p)) && is_concrete(fsig.ret),
            Ty::Obj(_, args) => args.iter().all(|a| is_concrete(*a)),
            _ => true,
        }
    }
    match gsig.ret {
        Ty::Obj(_, args) if !args.is_empty() && is_concrete(gsig.ret) => {
            // Canonicalize the recovered type to Kotlin form (`java/util/List<java/lang/Integer>` →
            // `kotlin/collections/List<kotlin/Int>`), so a member/`for`/extension keyed on the Kotlin
            // collection + a primitive element resolves and unboxes — mirroring the suspend path. Without
            // it, a classpath property `items: List<Int>` reads as raw `java/util/List<Integer>`:
            // `xs.sum()` is unresolved and `for (x in xs) { s += x }` compares `Int` vs `java/lang/Integer`.
            Some(canonicalize_jvm_collections(
                crate::symbol_resolver::ty_subst(gsig.ret, &std::collections::HashMap::new()),
            ))
        }
        _ => None,
    }
}

/// The LOGICAL return of a `suspend` method, recovered from its generic signature: the last parameter is
/// `Continuation<-T>`, whose type argument `T` is the source return type (`Continuation<-Config>` →
/// `Config`). A `Continuation<-Unit>` maps to `Ty::Unit` (the source `Unit` return).
fn suspend_return_from_gsig(
    gsig: &GenericSig,
    binds: &std::collections::HashMap<String, Ty>,
) -> Option<Ty> {
    match *gsig.params.last()? {
        Ty::Obj(n, args) if crate::types::same(n, crate::types::wk::continuation()) => {
            match *args.first()? {
                // A bare class → its CANONICAL `Ty` (`kotlin/String` → `Ty::String`, `kotlin/Int` → `Ty::Int`,
                // `kotlin/Unit` → `Ty::Unit`), so the recovered return unifies with the source-spelled type
                // rather than a non-canonical `Obj("kotlin/String")`. A generic class (`List<Item>`) keeps its
                // arguments via the general converter.
                Ty::Obj(name, []) => {
                    // Canonicalize a JVM built-in the generic signature spells in Java terms
                    // (`java/lang/String` → `kotlin/String`, `java/lang/Object` → `kotlin/Any`) so the
                    // recovered return unifies with the source-spelled type rather than a non-canonical
                    // `Obj("java/lang/String")`. A boxed PRIMITIVE (`java/lang/Long`) stays an `Obj` here —
                    // the call site unboxes it to the source primitive only when the return is non-nullable
                    // (a `Long?` return must keep the boxed form).
                    Some(kotlin_name_to_ty(to_kotlin_internal(&name.render())))
                }
                // A generic class (`List<Item>`) keeps its arguments via the general converter, then any JVM
                // collection name the signature spelled in Java terms (`java/util/List`) is canonicalized to
                // its Kotlin type (`kotlin/collections/List`) so a `.map { … }` / `.first()` extension — keyed
                // on the Kotlin collection — resolves on the recovered suspend result (a member such as `.size`
                // already resolved on either form). A BARE type parameter (`Continuation<T>` from a generic
                // `suspend fun byId(): T` on a `Repo<Cfg>` receiver) is substituted under `binds` to the
                // receiver's concrete argument (`T` → `Cfg`) — otherwise it erases to `Any` and every member
                // access on the result fails ("member … on Any").
                other => Some(canonicalize_jvm_collections(
                    crate::symbol_resolver::ty_subst(other, binds),
                )),
            }
        }
        _ => None,
    }
}

/// Recursively rewrite each JVM collection interface in `ty` (`java/util/List<T>` → its Kotlin
/// `kotlin/collections/List<T>`), leaving every other type and the type arguments' own structure intact.
fn canonicalize_jvm_collections(ty: Ty) -> Ty {
    match ty {
        // A boxed primitive wrapper (`java/lang/Integer`) — always a type ARGUMENT here (a top-level
        // return is a collection with args, or handled by the suspend bare-class path). Its Kotlin type is
        // the PRIMITIVE (`kotlin/Int` → `Ty::Int`), not a boxed `Obj("kotlin/Int")`: a signature spells a
        // generic argument's primitive in its boxed JVM form (`List<Integer>`), but `List<Int>`'s element
        // is `Int`, so `for (x in xs) { s += x }` / `xs.sum()` resolve and the element unboxes rather than
        // comparing `Int` against a boxed reference.
        Ty::Obj(name, []) => match super::jvm_class_map::wrapper_to_kotlin_prim_name(name) {
            Some(prim) => super::classpath::kotlin_name_to_ty(prim),
            None => Ty::obj_name(
                super::jvm_class_map::jvm_collection_to_kotlin_type_name(name).unwrap_or(name),
            ),
        },
        Ty::Obj(name, args) => {
            // Canonicalize a JVM collection to its Kotlin form (`java/util/List` →
            // `kotlin/collections/List`), so a member/`for`/extension keyed on the Kotlin collection
            // resolves on the recovered type.
            let kname =
                super::jvm_class_map::jvm_collection_to_kotlin_type_name(name).unwrap_or(name);
            let cargs: Vec<Ty> = args
                .iter()
                .map(|a| canonicalize_jvm_collections(*a))
                .collect();
            Ty::obj_args_name(kname, &cargs)
        }
        other => other,
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
            let raw_internal = &s[1..s.len() - 1];
            if raw_internal == "java/lang/Void" {
                return Ty::Unit;
            }
            let internal = to_kotlin_internal(raw_internal);
            if let Some(n) = internal
                .strip_prefix("kotlin/jvm/functions/Function")
                .and_then(|n| n.parse::<usize>().ok())
            {
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
        supertypes: TypeNameList::new(),
        constructors: Vec::new(),
        members,
        companion: Vec::new(),
        companion_consts: Default::default(),
        sam_method: None,
        companion_object: None,
        value_companion_fns: Vec::new(),
        value_underlying: None,
        alias_target: None,
        type_params: Vec::new(),
        sealed_subclasses: TypeNameList::new(),
        enum_entries: Vec::new(),
        value_ctor_has_default: false,
        ctor_named_params: Vec::new(),
        value_class_properties: Vec::new(),
        retention: None,
    })
}

/// The [`LibraryType`] of a classless Kotlin BUILTIN (`kotlin/Number`, `kotlin/collections/List`, …) whose
/// JVM class is absent from the classpath (a no-JDK compile) — supertypes and members from the
/// `.kotlin_builtins` data, kind from the metadata `is_interface` flag.
fn builtin_library_type(
    is_interface: bool,
    supertypes: Vec<String>,
    members: Vec<LibraryMember>,
) -> LibraryType {
    LibraryType {
        is_public: true,
        kind: if is_interface {
            crate::libraries::TypeKind::Interface
        } else {
            crate::libraries::TypeKind::Class
        },
        supertypes: supertypes.into(),
        constructors: Vec::new(),
        members,
        companion: Vec::new(),
        companion_consts: Default::default(),
        sam_method: None,
        companion_object: None,
        value_companion_fns: Vec::new(),
        value_underlying: None,
        alias_target: None,
        type_params: Vec::new(),
        sealed_subclasses: TypeNameList::new(),
        enum_entries: Vec::new(),
        value_ctor_has_default: false,
        ctor_named_params: Vec::new(),
        value_class_properties: Vec::new(),
        retention: None,
    }
}

/// Parse a class generic signature into its formal type-parameter names and its supertypes (the
/// superclass followed by interfaces) as signature nodes, e.g. `java/util/List`'s
/// `<E:Ljava/lang/Object;>Ljava/lang/Object;Ljava/util/Collection<TE;>;` → (`[E]`, `[Object,
/// Collection<E>]`). The supertypes carry their own type arguments (in terms of this class's formals),
/// which is what lets a type argument propagate up the hierarchy (`List<Int>` → `Collection<Int>`).
fn parse_class_gsig(sig: &str) -> Option<(Vec<String>, Vec<Ty>)> {
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
        // Arrays are `Obj("kotlin/IntArray")`/`Obj("kotlin/Array", [T])` but their extensions are indexed
        // by the JVM ARRAY descriptor (`[I`, `[Ljava/lang/String;`), not a `Lkotlin/…Array;` class name —
        // so key off the array descriptor + `Object`, exactly as the legacy `Ty::Array` spelling did.
        _ if receiver.is_array() => return vec![type_descriptor(receiver), object],
        Ty::Obj(i, _) => super::jvm_class_map::to_jvm_type_name(i),
        Ty::String => super::jvm_class_map::to_jvm_type_name(type_name("kotlin/String")),
        _ => return vec![type_descriptor(receiver), object],
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut q = std::collections::VecDeque::new();
    q.push_back(start);
    while let Some(name) = q.pop_front() {
        if !seen.insert(name) {
            continue;
        }
        out.push(format!("L{};", name.render()));
        if let Some(ci) = cp.find_name(name) {
            q.extend(ci.interfaces.iter_ids());
            if let Some(s) = ci.super_class {
                q.push_back(s);
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
    class_implements_name(cp, type_name(internal), type_name(target))
}

fn class_implements_name(cp: &Classpath, internal: TypeName, target: TypeName) -> bool {
    let target = super::jvm_class_map::to_jvm_type_name(target);
    let mut seen = std::collections::HashSet::new();
    let mut q = std::collections::VecDeque::new();
    q.push_back(super::jvm_class_map::to_jvm_type_name(internal));
    while let Some(name) = q.pop_front() {
        if name == target {
            return true;
        }
        if !seen.insert(name) {
            continue;
        }
        if let Some(ci) = cp.find_name(name) {
            q.extend(ci.interfaces.iter_ids().chain(ci.super_class));
        }
    }
    false
}

impl SymbolSource for JvmLibraries {
    fn property_members(&self, recv: Ty, name: &str) -> PropertySet {
        // Member properties of the receiver's type + its supertypes, most-derived first (rung 0). Each
        // carries the REAL getter/setter from `@Metadata`'s `JvmPropertySignature`, so the caller emits the
        // accessor by name rather than guessing `getX`. Extension properties are surfaced by `resolve_symbols`.
        let Some(internal) = recv.kotlin_class_internal().map(|n| n.render()) else {
            return PropertySet::default();
        };
        let mut overloads = Vec::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(internal);
        let mut seen = std::collections::HashSet::new();
        let mut rung = 0u32;
        while let Some(cn) = queue.pop_front() {
            if !seen.insert(cn.clone()) {
                continue;
            }
            if let Some(ci) = self.cp.find(&cn) {
                for mp in metadata::class_properties(&ci) {
                    if mp.name != name {
                        continue;
                    }
                    // Need the real accessor to emit anything; skip a property whose metadata omits it.
                    let Some(getter) = mp.getter else { continue };
                    let ret_ty = mp
                        .ret_class
                        .map_or(Ty::obj("kotlin/Any"), kotlin_type_name_to_ty);
                    let ty = mp.ret_class.map_or(Ty::obj("kotlin/Any"), Ty::obj_name);
                    let getter = LibraryCallable::library(
                        type_name(&cn),
                        getter.name,
                        vec![],
                        ret_ty,
                        ret_ty,
                        getter.desc,
                    );
                    let setter = mp.setter.map(|s| {
                        LibraryCallable::library(
                            type_name(&cn),
                            s.name,
                            vec![ret_ty],
                            Ty::Unit,
                            Ty::Unit,
                            s.desc,
                        )
                    });
                    overloads.push(PropertyInfo {
                        kind: PropKind::Member,
                        receiver: Some(Ty::obj(&cn)),
                        formals: Vec::new(),
                        ty,
                        getter,
                        setter,
                        is_const: mp.is_const,
                        visibility: mp.visibility,
                        owner: type_name(&cn),
                        receiver_rank: rung,
                    });
                }
            }
            if let Some(t) = self.resolve_type(&cn) {
                queue.extend(t.supertypes.to_vec());
            }
            rung += 1;
        }
        PropertySet { overloads }
    }

    fn member_is_property(&self, recv: Ty, name: &str) -> bool {
        // A classpath `@Metadata` property (walks supertypes) — authoritative for Kotlin `.class` types.
        if !self.property_members(recv, name).overloads.is_empty() {
            return true;
        }
        // A Kotlin BUILTIN property (`CharSequence.length`, `Collection.size`) lives in `.kotlin_builtins`,
        // not `.class` metadata, and is often declared on a SUPERtype — walk the builtin supertype closure.
        let Some(internal) = recv.kotlin_class_internal() else {
            return false;
        };
        let mut queue = vec![internal.to_string()];
        let mut seen = std::collections::HashSet::new();
        while let Some(cn) = queue.pop() {
            if !seen.insert(cn.clone()) {
                continue;
            }
            if self.cp.builtin_member_is_property(&cn, name) {
                return true;
            }
            queue.extend(self.cp.builtin_supertypes(&cn));
        }
        false
    }

    fn class_is_extensible(&self, internal: &str) -> bool {
        // Only a real, concrete (non-final, non-abstract) non-interface `.class` is a safe superclass to
        // emit a `super(…)` to. A mapped/builtin type (no own `.class`) or a final/abstract/interface
        // base is rejected — extending it would either fail JVM verification or need bridge/abstract
        // synthesis the backend does not do.
        self.cp
            .find(internal)
            .is_some_and(|ci| !ci.is_final() && !ci.is_abstract() && !ci.is_interface())
    }

    fn class_is_extensible_name(&self, internal: TypeName) -> bool {
        self.cp
            .find_name(internal)
            .is_some_and(|ci| !ci.is_final() && !ci.is_abstract() && !ci.is_interface())
    }

    fn resolve_type(&self, internal: &str) -> Option<LibraryType> {
        let internal_name = type_name(internal);
        if let Some(hit) = self.cp.cached_library_type_name(internal_name) {
            return hit.as_ref().map(|rc| (**rc).clone());
        }
        let built = (|| -> Option<LibraryType> {
            // A classpath `typealias` (`kotlin/collections/ArrayList` → `java/util/ArrayList`) has no class of
            // its own; resolve the underlying type and tag it with `alias_target` so name resolution records
            // the real internal.
            if let Some(target) = self.cp.type_alias_target(internal) {
                let mut t = self.resolve_type(&target)?;
                t.alias_target = Some(type_name(&target));
                return Some(t);
            }
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
                        // The mapped JVM class is absent (a no-JDK compile). If `internal` is a Kotlin builtin,
                        // report it from the `.kotlin_builtins` data (present in the stdlib) — its Kotlin
                        // identity, members, supertypes, and class-vs-interface kind — so `List`/`Number`/… still
                        // resolve without the JDK on the classpath.
                        None => {
                            if let Some(is_iface) = self.cp.builtin_is_interface(internal) {
                                return Some(builtin_library_type(
                                    is_iface,
                                    self.cp.builtin_supertypes(internal),
                                    self.builtin_members_for_type(internal),
                                ));
                            }
                            // Otherwise the backend's curated minimal ABI for the well-known mapped builtins.
                            return mapped_builtin_fallback(internal);
                        }
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
            // The class's `@Metadata` function records — carry each member's SOURCE parameter names and
            // default flags, which the erased JVM descriptor loses. Populate every member's `call_sig` from
            // its record so a named-argument / omitted-`$default` member call resolves through the ONE
            // `resolve_type` member seam (the `instance_members` query), not a separate `functions()` walk.
            let meta_fns = metadata::class_functions(&ci);
            let member_meta = |jvm_name: &str, value_arity: usize| {
                meta_fns
                    .iter()
                    .find(|f| f.jvm_name == jvm_name && f.value_params.len() == value_arity)
            };
            let member_call_sig = |member: &LibraryMember, jvm_name: &str| {
                let value_arity = if member.suspend && !member.params.is_empty() {
                    member.params.len() - 1
                } else {
                    member.params.len()
                };
                member_meta(jvm_name, value_arity)
                    .map(metadata::MetaFn::member_call_sig)
                    .unwrap_or_default()
            };
            for m in &ci.methods {
                // Public members are callable from anywhere; a `protected` member is surfaced too so a
                // subclass can reach it through the supertype walk (a compiling program only reaches it
                // from a legal subclass, which kotlinc already checked). Private/package members stay
                // dropped: no legal call site.
                if !m.is_public() && !m.is_protected() {
                    continue;
                }
                let (params, ret) = parse_method_desc(&m.descriptor);
                let mut member =
                    LibraryMember::new(m.name.clone(), params, ret, m.descriptor.clone());
                member.visibility = if m.is_public() {
                    Visibility::Public
                } else {
                    Visibility::Protected
                };
                member.signature = m.signature.clone();
                // The member's parsed generic signature — carries type-variable binding facts so a caller can
                // infer a generic return from the receiver's type arguments (`Repo<Config>.load(): Config`).
                member.generic_sig = m.signature.as_deref().and_then(parse_method_gsig);
                member.suspend = self.cp.is_suspend_method(internal, &m.name);
                // A `suspend` member's descriptor erases its return to `Object` (the CPS convention). Recover
                // the LOGICAL return from `@Metadata` (`Int`, not `Object`) so a caller unboxes the suspension
                // result — keeping the erased type as `physical_ret` for the emitter.
                if member.suspend {
                    let value_arity = member.params.len().saturating_sub(1);
                    if let Some(f) = member_meta(&m.name, value_arity) {
                        member.physical_ret = member.ret;
                        let logical = metadata_return_info(f.ret_class, f.ret_nullable)
                            .apply(member.physical_ret);
                        // krusty erases REFERENCE nullability in `Ty` (`String?` is modeled as `String`);
                        // `ret_nullable` tracks only PRIMITIVE nullability (a boxed suspension result). So a
                        // reference return keeps its plain non-null type; only a primitive carries the flag.
                        if logical.non_null().is_reference() {
                            member.ret = logical.non_null();
                        } else {
                            member.ret = logical;
                            member.ret_nullable = f.ret_nullable;
                        }
                    }
                    // The EMIT descriptor is the LOGICAL (continuation-stripped) form — the coroutine pass
                    // re-threads the CPS `Continuation` at the call. `params` stay RAW (they carry the
                    // continuation), which the member consumers that count physical params rely on; only the
                    // arg-matching `member_overloads` converter drops the trailing continuation value param.
                    member.descriptor = strip_continuation_param(&member.descriptor);
                }
                if is_map && member.name == "put" {
                    member.ret_nullable = true;
                }
                if m.name == "<init>" {
                    // Parse the ctor's generic signature so the resolver can infer a construction's type
                    // arguments (`Pair(1, 2)` → `<Int, Int>`) without spelling backend signature strings.
                    member.generic_sig = m.signature.as_deref().and_then(parse_method_gsig);
                    constructors.push(member);
                } else if m.is_static() {
                    // A Kotlin companion member compiles to a JVM static on the class.
                    member.call_sig = member_call_sig(&member, &m.name);
                    companion.push(member);
                } else {
                    member.call_sig = member_call_sig(&member, &m.name);
                    members.push(member);
                }
            }
            // A member whose JVM name is MANGLED by a value-class PARAMETER (`fun get(id: Vid): Cat` →
            // `get-<hash>(String)`): the descriptor-read loop above stored it under the mangled name, so a
            // source-name call `p.get(v)` misses it, and its erased `String` parameter wouldn't accept the
            // `Vid` argument. Recover the SOURCE name + logical (value-class) parameter types from `@Metadata`
            // and expose the member under the source name — keeping the mangled JVM name as `physical_name`
            // and the erased descriptor for emit (the value-classes pass unboxes the `Vid` argument).
            for mf in &meta_fns {
                if !mf.is_public || mf.is_extension || mf.jvm_name == mf.kotlin_name {
                    continue;
                }
                let Some(desc) = mf.jvm_desc else {
                    continue;
                };
                let (params, physical_ret) = parse_method_desc(desc);
                // A `suspend` member appends a `Continuation` JVM parameter the SOURCE signature
                // (`value_params`) excludes — drop it (the CPS pass re-threads it) and match the leading
                // value parameters. Any OTHER count mismatch (`@Composable`, …) isn't a plain mangled member.
                let value_params = if mf.is_suspend && !params.is_empty() {
                    &params[..params.len() - 1]
                } else {
                    &params[..]
                };
                if value_params.len() != mf.value_params.len() {
                    continue;
                }
                let mut logical: Vec<Ty> = value_params
                    .iter()
                    .zip(&mf.value_params)
                    .map(|(p, vp)| vp.ty.map(kotlin_type_name_to_ty).unwrap_or(*p))
                    .collect();
                // A `suspend` member's `params` carry the trailing `Continuation` (the resolver strips it when
                // matching a call, exactly as for a non-mangled suspend member); the logical value parameters
                // are the leading ones.
                if mf.is_suspend {
                    logical.push(Ty::obj("kotlin/coroutines/Continuation"));
                }
                // The bare `ret_class` recovery erases a parameterized return to its raw class
                // (`List<Ws>` → `List`, whose element is `Any`), so a member access on an element of a
                // value-class-param member's result (`repo.findByOrg(id).firstOrNull { it.name }`) fails to
                // resolve. The metadata generic signature carries the full return (`List<Ws>`); prefer it
                // when it has type arguments, applying return nullability, else keep the `ret_class` return.
                let ret = match mf.generic_sig.as_ref() {
                    Some(g) if !g.ret.type_args().is_empty() => {
                        if mf.ret_nullable {
                            Ty::nullable(g.ret)
                        } else {
                            g.ret
                        }
                    }
                    _ => metadata_return_info(mf.ret_class, mf.ret_nullable).apply(physical_ret),
                };
                let mut member =
                    LibraryMember::new(mf.kotlin_name.clone(), logical, ret, desc.to_string());
                member.physical_name = Some(mf.jvm_name.clone());
                member.physical_ret = physical_ret;
                member.ret_nullable = mf.ret_nullable;
                member.suspend = mf.is_suspend;
                member.call_sig = mf.member_call_sig();
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
            let mut supertypes = ci.interfaces.to_vec();
            if let Some(s) = ci.super_class() {
                supertypes.push(s);
            }
            for s in self.cp.builtin_supertypes(internal) {
                if !supertypes.iter().any(|existing| existing == &s) {
                    supertypes.push(s);
                }
            }
            // A JVM collection type (`java/util/Set`) and its JVM supertypes ARE their Kotlin mapped types
            // (`kotlin/collections/Set`) at the source level — an extension declared on
            // `kotlin/collections/Iterable` applies to a `java/util/Set` receiver (`entries.first()`). Add
            // each Kotlin equivalent as a supertype so the source-type receiver walk bridges the
            // java.util ↔ kotlin.collections platform mapping instead of dead-ending in the JDK hierarchy.
            // The read-only Kotlin face of every JVM collection interface in the hierarchy — an extension on
            // `kotlin/collections/Iterable`/`List`/… applies to a `java/util/*` receiver.
            let mut mapped: Vec<String> = std::iter::once(internal)
                .chain(supertypes.iter().map(String::as_str))
                .filter_map(super::jvm_class_map::jvm_collection_to_kotlin)
                .map(str::to_string)
                .collect();
            // The MUTABLE face — but ONLY for a CONCRETE class (`java/util/ArrayList`, `HashMap`), never for
            // the JVM collection INTERFACES: `java/util/List` is the shared realization of BOTH the read-only
            // `kotlin/collections/List` and its mutable sibling, so tagging the interface mutable would make a
            // read-only `List` (which reaches `java/util/List` in its supertypes) spuriously satisfy a
            // `MutableCollection.plusAssign`. A concrete class is genuinely mutable, so its `MutableList`/
            // `MutableSet`/`MutableMap` face is sound (derived from the java.util interface it implements).
            if !ci.is_interface() {
                for m in std::iter::once(internal)
                    .chain(supertypes.iter().map(String::as_str))
                    .filter_map(super::jvm_class_map::jvm_collection_to_kotlin_mutable)
                {
                    if !mapped.iter().any(|x| x == m) {
                        mapped.push(m.to_string());
                    }
                }
            }
            for k in mapped {
                if !supertypes.iter().any(|existing| existing == &k) {
                    supertypes.push(k);
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
                (f.descriptor == format!("L{nested};"))
                    .then(|| (f.name.clone(), type_name(&nested)))
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
            let inline = metadata::class_inline(&ci);
            let value_underlying = inline.as_ref().map(|ic| {
                let u = match ic.underlying_class.as_deref() {
                    Some(other) => kotlin_name_to_ty(other),
                    // The underlying type couldn't be resolved from `@Metadata` (an unparsed shape).
                    // Recover it from the synthesized `box-impl(U)` parameter descriptor, the
                    // authoritative JVM underlying type. (A type PARAMETER underlying — `Result<T>` —
                    // has no concrete box-impl param and stays `Any`.)
                    None => ci
                        .methods
                        .iter()
                        .find(|m| m.name == "box-impl")
                        .and_then(|m| m.descriptor.strip_prefix('('))
                        .and_then(split_one)
                        .map(|(d, _)| field_desc_to_ty(d))
                        .unwrap_or_else(|| Ty::obj("kotlin/Any")),
                };
                // Carry the underlying's declared nullability — it decides the null-representation
                // (`X?` unboxed over a NON-NULL reference underlying; boxed otherwise). Unknown
                // (metadata shape not parsed) stays nullable: the conservative boxed treatment.
                crate::trace_compiler!(
                    "resolve",
                    "value_underlying {}: class={:?} nullable={:?} u={:?}",
                    ci.this_class(),
                    ic.underlying_class,
                    ic.underlying_nullable,
                    u
                );
                if ic.underlying_nullable == Some(false) {
                    u
                } else {
                    crate::types::Ty::nullable(u)
                }
            });
            let value_class_metadata_members =
                self.value_class_metadata_members_for_class(&ci, inline.is_some(), &meta_fns);
            // The class's own formal type parameters (`Pair` → `[A, B]`), for constructor type-argument
            // inference; empty for a non-generic type.
            let type_params = ci
                .signature
                .as_deref()
                .and_then(parse_class_gsig)
                .map(|(formals, _)| formals)
                .unwrap_or_default();
            // An enum entry is a `static` field of the enum's OWN type (`descriptor == L<internal>;`).
            const ACC_STATIC: u16 = 0x0008;
            let enum_entries: Vec<String> = ci
                .fields
                .iter()
                .filter(|f| f.access & ACC_STATIC != 0 && f.descriptor == self_desc)
                .map(|f| f.name.clone())
                .collect();
            // A defaulted value-class primary constructor surfaces as the `constructor-impl$default` synthetic.
            let value_ctor_has_default = ci
                .methods
                .iter()
                .any(|m| m.name == "constructor-impl$default");
            Some(LibraryType {
                is_public: ci.is_public(),
                kind,
                supertypes: supertypes.into(),
                constructors,
                members: members
                    .into_iter()
                    .chain(value_class_metadata_members)
                    .chain(self.builtin_members_for_type(internal))
                    .collect(),
                companion,
                companion_consts: self.companion_consts_for_class(&ci),
                sam_method: self.sam_method_for_class(&ci.this_class()),
                companion_object,
                value_companion_fns: self.value_companion_fns_for_class(&ci, inline.is_some()),
                value_underlying,
                alias_target: None,
                type_params,
                sealed_subclasses: metadata::class_sealed_subclasses(&ci).into(),
                enum_entries,
                value_ctor_has_default,
                ctor_named_params: metadata::class_constructor_params(&ci),
                value_class_properties: self.value_class_property_members_for_class(&ci),
                retention: ci.retention.clone(),
            })
        })();
        self.cp
            .cache_library_type_name(internal_name, built.clone().map(std::rc::Rc::new));
        built
    }

    fn resolve_type_name(&self, internal: TypeName) -> Option<LibraryType> {
        if let Some(hit) = self.cp.cached_library_type_name(internal) {
            return hit.as_ref().map(|rc| (**rc).clone());
        }
        self.resolve_type(&internal.render())
    }

    fn resolve_symbols(&self, fqn: &str) -> crate::libraries::ResolvedSymbols {
        use crate::libraries::{Callables, ResolvedSymbols};
        // The spec's top-level memo: this classpath `SymbolSource` composes the namespace record for `fqn`
        // once and reuses it across the compile. A `JvmLibraries` wrapper is rebuilt per snippet, but the
        // `Classpath` (which owns the memo) is reused on the worker thread, so hot stdlib names resolve
        // without re-walking metadata/extension indexes.
        if let Some(cached) = self.cp.cached_symbols(fqn) {
            return (*cached).clone();
        }
        // Classifier namespace: the class/interface/object (or a typealias's target) at the fqn.
        let classifier = self.resolve_type(fqn);
        // Callable namespace, receiver-AGNOSTIC (resolution is by fqn; the receiver binds later, in the
        // consumer). Top-level functions of the source name declared in the fqn's package, plus the
        // package's extensions (source-keyed via the tree, so a `@JvmName`-mangled extension `sum` →
        // `sumOfInt` is found under its SOURCE name; the JVM name stays on the callable for emit).
        let (pkg, name) = fqn.rsplit_once('/').unwrap_or(("", fqn));
        // TOP-LEVEL functions of the package (receiver-less). The receiver-less `functions(name, None)`
        // query classifies each candidate by its metadata receiver, so an EXTENSION compiled into the same
        // facade surfaces here too — but with no receiver populated (`FnKind::Extension`, `receiver: None`),
        // a malformed shape for extension selection. Take only genuine `TopLevel` here; extensions come
        // from the receiver-carrying tree loop below, so the namespace has ONE clean source per kind.
        let mut overloads: Vec<_> = self
            .top_level_overloads(name)
            .into_iter()
            .filter(|o| o.kind == FnKind::TopLevel && o.callable.owner_package_matches(pkg))
            .collect();
        // Extension PROPERTIES of the source name live in the CALLABLE namespace's property half. A name is
        // functions XOR a property, so these are surfaced separately and chosen when there are no functions.
        let mut props: Vec<PropertyInfo> = Vec::new();
        // Extension discovery is @Metadata-driven (the source of truth), NOT a scan of JVM statics: the
        // package's PUBLIC facades' metadata carry each extension's SOURCE receiver, parameters, return
        // (with nullability), visibility, and generic signature. The JVM method (`@JvmName`-mangled name +
        // descriptor) is only the emit handle, rooted at the PUBLIC facade — kotlinc's `invokestatic`
        // target — so a package-private multifile PART never leaks a `false` visibility. Receiver-coupled
        // JVM specifics (element-variant `sumOfInt`, value-class mangling) are the emitter's job.
        for facade in self.cp.package_facades(pkg) {
            let facade_rendered = facade.render();
            for mf in self.cp.meta_functions_name(facade).iter() {
                if mf.kotlin_name != name || !mf.is_extension {
                    continue;
                }
                // SOURCE receiver, PRECISE: the metadata generic signature's receiver carries type
                // arguments and value-class identity (`Iterable<Int>` vs `Iterable<Long>`, `UInt` vs `Int`)
                // that the erased `receiver_class` loses — the consumer selects the element/value-class
                // -appropriate `@JvmName` variant from these. Fall back to the bare receiver class, then to
                // a universal `Any` for a type-variable receiver (`<T> T.let`).
                let receiver = mf
                    .generic_sig
                    .as_ref()
                    .and_then(|g| g.receiver)
                    .map(|r| ty_subst(r, &std::collections::HashMap::new()))
                    .or_else(|| mf.receiver_class.map(kotlin_type_name_to_ty))
                    .unwrap_or_else(|| Ty::obj("kotlin/Any"));
                // Emit handle: the JVM method + descriptor on the public facade. Prefer the metadata
                // `method_signature`. When it is absent, resolve the real bytecode method by name against the
                // facade's super chain, trying: the metadata name, then the ELEMENT-materialized name kotlinc
                // gives an `@OverloadResolutionByLambdaReturnType` reduction (`sum` on `Iterable<Byte>` →
                // `sumOfByte`) — derived from the receiver's element type and VERIFIED against the bytecode,
                // no hardcoded name list. The discovered method's real name becomes the emit `jvm_name`.
                let elem_mangled = receiver
                    .type_args()
                    .first()
                    .copied()
                    .and_then(Ty::kotlin_class_internal)
                    .map(|i| i.render())
                    .map(|i| i.rsplit('/').next().unwrap_or(&i).to_string())
                    .map(|s| format!("{}Of{s}", mf.jvm_name));
                // The receiver's erased descriptor disambiguates same-named overloads on the facade
                // (`maxOrNull([I)` vs `maxOrNull([D)`); the return descriptor disambiguates same-receiver
                // overloads (`maxOrNull(Iterable)Double` vs `…Comparable`). A type-var return has no class,
                // so `None` selects the generic-bound overload in the fallback.
                let recv_desc = type_descriptor(
                    <Self as crate::libraries::SemanticPlatform>::library_value_form(
                        self, receiver,
                    ),
                );
                let ret_desc = mf.ret_class.map(|r| {
                    type_descriptor(
                        <Self as crate::libraries::SemanticPlatform>::library_value_form(
                            self,
                            kotlin_type_name_to_ty(r),
                        ),
                    )
                });
                // The value parameters' ERASED JVM descriptors, from the metadata generic signature, so a
                // bytecode lookup disambiguates same-receiver/same-return overloads by their VALUE param —
                // both a concrete type (`appendLine(StringBuilder, int)` vs `appendLine(StringBuilder)`) and
                // a FUNCTION type (`any(Iterable, Function1)` vs `any(Iterable)`). A type-variable param
                // erases to `Object`; a function type to `FunctionN`. `None` (match by receiver alone) only
                // when there is no generic signature.
                let value_param_descs: Option<Vec<String>> = mf.generic_sig.as_ref().map(|g| {
                    g.params
                        .iter()
                        .map(|p| {
                            let ty = ty_subst(*p, &std::collections::HashMap::new());
                            type_descriptor(
                                <Self as crate::libraries::SemanticPlatform>::library_value_form(
                                    self, ty,
                                ),
                            )
                        })
                        .collect()
                });
                let by_name = |n: &str| {
                    self.cp.facade_method(
                        &facade_rendered,
                        n,
                        Some(&recv_desc),
                        ret_desc.as_deref(),
                        value_param_descs.as_deref(),
                    )
                };
                // The real bytecode method (public flag + descriptor), matched to the CHOSEN name so the
                // visibility is that method's own — an `@InlineOnly` extension (`let`/`run`) is source-public
                // but bytecode-non-public and MUST be inlined, never called. When metadata supplies the
                // descriptor the candidate is still looked up (by that same name) only for its visibility.
                let (jvm_name, descriptor, cand) = if let Some(d) = mf.jvm_desc {
                    (mf.jvm_name.clone(), d.to_string(), by_name(&mf.jvm_name))
                } else if let Some(c) = by_name(&mf.jvm_name) {
                    (c.name.clone(), c.descriptor.clone(), Some(c))
                } else if let Some(c) = elem_mangled.as_ref().and_then(|n| by_name(n)) {
                    (c.name.clone(), c.descriptor.clone(), Some(c))
                } else {
                    continue;
                };
                let bytecode_public = cand.as_ref().map_or(mf.is_public, |c| c.public);
                let (params, pret) = parse_method_desc(&descriptor);
                if params.is_empty() {
                    continue;
                }
                let call_sig = mf.extension_call_sig();
                let ret_class = mf.ret_class.map(kotlin_type_name_to_ty);
                let ret = match ret_class {
                    Some(t) if mf.ret_nullable && t.is_jvm_scalar() => Ty::nullable(t),
                    Some(t) => t,
                    None if mf.ret_nullable && pret.is_jvm_scalar() => Ty::nullable(pret),
                    None => pret,
                };
                // The metadata-primary generic signature drives lambda-parameter and return binding.
                let generic_sig = mf.generic_sig.clone();
                // The extension's DECLARED receiver source type — carried so the value-class pass can unbox
                // a boxed receiver (`fun Result<T>.getOrThrow` on a boxed `kotlin.Result`). `None` for a
                // type-VARIABLE receiver (`fun <T> T.let` — erases to `Object`, no value-class identity).
                let source_receiver = match generic_sig.as_ref().and_then(|g| g.receiver) {
                    Some(r) if r.is_ty_param() => None,
                    None => None,
                    _ => Some(receiver),
                };
                // `@InlineOnly` (`inline` + bytecode-non-public) MUST be spliced; a plain `inline` MAY be.
                let inline = InlineKind::from_flags(mf.is_inline, mf.is_inline && !bytecode_public);
                let callable = LibraryCallable {
                    inline,
                    suspend: mf.is_suspend,
                    source_receiver,
                    // Carry the resolved bytecode method's generic `Signature` — a `<reified T>` extension's
                    // splice reads its formal-type-parameter NAMES from here to bind the call's explicit
                    // type arguments. Without it the reified body cannot be specialized and the call falls
                    // back to a (throwing) direct invoke of the inline-only method.
                    signature: cand.as_ref().and_then(|c| c.signature.clone()),
                    ..LibraryCallable::library(facade, jvm_name, params, ret, pret, descriptor)
                };
                overloads.push(FunctionInfo {
                    ret: ReturnInfo::new(mf.ret_nullable, ret_class),
                    // Public if EITHER the metadata OR the bytecode method is public: a value-class inline
                    // extension is metadata-public but bytecode-private (resolved, then spliced), while some
                    // metadata (a user library's top-level extension) under-reports visibility though the
                    // emitted `invokestatic` target is plainly public. Either sense makes it callable.
                    visibility: crate::libraries::Visibility::from_public(
                        mf.is_public || bytecode_public,
                    ),
                    generic_sig,
                    flags: FnFlags {
                        inline,
                        suspend: mf.is_suspend,
                    },
                    call_sig,
                    ..FunctionInfo::plain(FnKind::Extension, Some(receiver), callable)
                });
            }
            // Extension PROPERTIES declared by the facade — `arr.lastIndex`, `list.indices`. Metadata
            // records the property with its REAL accessor names (`JvmPropertySignature`), so the getter
            // name is authoritative, never a `getX` guess. A multifile FACADE holds no property metadata of
            // its own — its `d1` names the PART classes that do; merge them (mirrors the `meta_functions`
            // part merge). A single-file facade carries its properties directly.
            let Some(fci) = self.cp.find_name(facade) else {
                continue;
            };
            let mut mprops = metadata::package_properties(&fci);
            if mprops.is_empty() {
                for part in &fci.kotlin_d1 {
                    if let Some(pci) = self.cp.find(part) {
                        mprops.extend(metadata::package_properties(&pci));
                    }
                }
            }
            for mp in mprops {
                if mp.name != name || mp.receiver_class.is_none() {
                    continue; // this property name, extension only
                }
                let Some(getter_sig) = mp.getter else {
                    continue;
                };
                let (gparams, gret) = parse_method_desc(&getter_sig.desc);
                if gparams.is_empty() {
                    continue;
                }
                let ret_ty = mp.ret_class.map_or(gret, kotlin_type_name_to_ty);
                let getter = LibraryCallable::library(
                    facade,
                    getter_sig.name,
                    gparams,
                    ret_ty,
                    gret,
                    getter_sig.desc,
                );
                let setter = mp.setter.map(|setter_sig| {
                    let (sparams, sret) = parse_method_desc(&setter_sig.desc);
                    LibraryCallable::library(
                        facade,
                        setter_sig.name,
                        sparams,
                        sret,
                        sret,
                        setter_sig.desc,
                    )
                });
                props.push(PropertyInfo {
                    kind: PropKind::Extension,
                    receiver: Some(
                        mp.receiver_class
                            .map_or(Ty::obj("kotlin/Any"), Ty::obj_name),
                    ),
                    formals: Vec::new(),
                    ty: mp.ret_class.map_or(Ty::obj("kotlin/Any"), Ty::obj_name),
                    getter,
                    setter,
                    is_const: mp.is_const,
                    visibility: mp.visibility,
                    owner: facade,
                    receiver_rank: 0,
                });
            }
        }
        // A name is functions XOR a property (never both) — prefer functions when present.
        let callables = if !overloads.is_empty() {
            Callables::Functions(FunctionSet { overloads })
        } else if !props.is_empty() {
            Callables::Properties(PropertySet { overloads: props })
        } else {
            Callables::None
        };
        (*self.cp.memoize_symbols(
            fqn,
            ResolvedSymbols {
                classifier,
                callables,
            },
        ))
        .clone()
    }

    fn member_overloads(&self, receiver: Ty, name: &str) -> FunctionSet {
        // Instance members of the receiver's type (own + inherited), BREADTH-FIRST (a subtype's
        // override before a supertype's), each tagged with its visit rung in `receiver_rank`. The
        // return is recovered receiver-COUPLED (a generic/`suspend` `Continuation<T>` bound from the
        // receiver's type argument) — the reason members are their own receiver-parameterized query.
        let mut overloads = Vec::new();
        if let Some(internal) = receiver.kotlin_class_internal() {
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
                    if m.name == name
                        || matches!(
                            (m.name.as_str(), name),
                            ("keySet", "keys") | ("entrySet", "entries")
                        )
                    {
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
                        // Member metadata is keyed by the physical JVM name for value-class-param
                        // mangled members (`copy` → `copy-<hash>`), and by the source/JVM name for
                        // ordinary members.
                        let meta_name = m.physical_name.as_deref().unwrap_or(&m.name);
                        let member_facts =
                            self.cp
                                .metadata_member_call_facts(&cn, meta_name, params.len());
                        // A `suspend` member's return facts are erased twice (to `Object`, then via the
                        // `Continuation<T>` type argument), so recover nullability and exact source
                        // classifier from the same class `@Metadata` member record.
                        let member_ret_metadata = suspend.then_some(member_facts.ret);
                        let suspend_ret_nullable =
                            suspend && member_ret_metadata.is_some_and(|m| m.nullable);
                        let ret = if suspend {
                            // A generic `suspend` member returns a type parameter (`byId(): T`) via
                            // `Continuation<T>`; bind `T` to the receiver's concrete argument
                            // (`Repo<Cfg>` → `T = Cfg`) so the return isn't erased to `Any`.
                            let recv_binds = self.receiver_type_bindings(receiver, &cn);
                            let base = generic_sig
                                .as_ref()
                                .and_then(|g| suspend_return_from_gsig(g, &recv_binds))
                                .unwrap_or(m.ret);
                            // `suspend_return_from_gsig` canonicalized a collection return to its
                            // READ-ONLY Kotlin form (the JVM signature erases read-only vs mutable).
                            // Recover the EXACT source form (`List` vs `MutableList`, …) from the
                            // member's `@Metadata` return classifier — which preserves it — keeping the
                            // gsig's (already-canonicalized) type arguments, so `.add(…)` on a declared
                            // `MutableList` return still resolves.
                            let base = match (base, member_ret_metadata.and_then(|m| m.class)) {
                                // Override the outer name ONLY when the metadata classifier is the SAME
                                // JVM collection as the gsig-recovered base — i.e. its read-only/mutable
                                // sibling (`List`/`MutableList` both erase to `java/util/List`). This
                                // guarantees the same collection family and arity, so keeping the gsig's
                                // type arguments is sound; a divergent classifier (stale metadata) is
                                // ignored rather than forming an arity-mismatched type.
                                (Ty::Obj(base_name, args), Some(Ty::Obj(meta_cls, _)))
                                    if meta_cls.starts_with("kotlin/collections/")
                                        && super::jvm_class_map::type_names_map_to_same_jvm_internal(
                                            meta_cls,
                                            base_name,
                                        ) =>
                                {
                                    Ty::obj_args_name(meta_cls, args)
                                }
                                (Ty::Obj(base_name, args), Some(Ty::Obj(_, _))) => {
                                    Ty::obj_args_name(base_name, args)
                                }
                                (b, _) => b,
                            };
                            crate::trace_compiler!(
                                "suspend",
                                "suspend return {cn}.{}: gsig={:?} base={:?} nullable={}",
                                m.name,
                                generic_sig,
                                base,
                                suspend_ret_nullable
                            );
                            // The `Continuation<T>` generic argument carries a PRIMITIVE return
                            // BOXED (generics erase primitives to their wrapper); unbox it to the
                            // source Kotlin primitive (`java/lang/Long` → `Ty::Long`) so
                            // `val n: Long = r.count()` type-checks. Nullability is applied by
                            // `ret_nullable` below (which mirrors the non-suspend member path).
                            base.obj_internal()
                                .and_then(super::jvm_class_map::wrapper_to_kotlin_prim_name)
                                .map(kotlin_name_to_ty)
                                .unwrap_or(base)
                        } else {
                            let recovered = self
                                .member_return(receiver, &m.name, &m.params)
                                .or_else(|| generic_sig.as_ref().and_then(concrete_generic_ret));
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
                        let call_sig = member_facts.call_sig;
                        // A generic-return builtin member (`Map.get(K): V?`) resolves to the erased
                        // classpath method (`java/util/Map.get` → `Object`), which carries no Kotlin
                        // nullability. Recover the source `V?` from the builtin `@Metadata`. Applied
                        // only for a PRIMITIVE return (a nullable primitive is a distinct BOXED type,
                        // so `m[k] ?: d` must null-check before unboxing); a nullable REFERENCE already
                        // null-checks regardless, and keeps its plain erased `Ty` (see below).
                        // `cn` may already be the front-end `kotlin/collections/…` name or the erased
                        // JVM form (`java/util/Map`, when the member is found on a classpath supertype);
                        // map both to the builtin whose `@Metadata` declares the nullability.
                        let builtin_cn = super::jvm_class_map::jvm_collection_to_kotlin(&cn)
                            .or_else(|| {
                                super::jvm_class_map::jvm_to_kotlin_builtin_with_members(&cn)
                            })
                            .unwrap_or(cn.as_str());
                        let builtin_ret_nullable = !ret.is_reference()
                            && self.cp.builtin_member_ret_nullable(
                                builtin_cn,
                                &m.name,
                                params.len(),
                            );
                        let callable = LibraryCallable {
                            inline: m.inline,
                            suspend,
                            signature: m.signature.clone(),
                            ..LibraryCallable::library(
                                m.owner.as_ref().cloned().unwrap_or_else(|| type_name(&cn)),
                                m.physical_name.clone().unwrap_or_else(|| m.name.clone()),
                                params,
                                ret,
                                m.physical_ret,
                                descriptor,
                            )
                        };
                        overloads.push(FunctionInfo {
                            ret: ReturnInfo::new(
                                m.ret_nullable
                                    || builtin_ret_nullable
                                    || (suspend_ret_nullable && !ret.is_reference()),
                                None,
                            ),
                            visibility: m.visibility,
                            receiver_rank: rung,
                            overload_rank: descriptor_narrowing(&m.descriptor) as u32,
                            generic_sig,
                            call_sig,
                            flags: FnFlags {
                                inline: m.inline,
                                suspend,
                            },
                            ..FunctionInfo::plain(FnKind::Member, Some(receiver), callable)
                        });
                    }
                }
                queue.extend(t.supertypes.to_vec());
                rung += 1;
            }
        }
        FunctionSet { overloads }
    }
}

impl crate::libraries::SemanticPlatform for JvmLibraries {
    fn function_type(&self, arity: usize) -> Option<Ty> {
        Some(Ty::obj(&format!("kotlin/jvm/functions/Function{arity}")))
    }

    fn static_field(&self, internal: &str, name: &str) -> Option<crate::libraries::StaticFieldRef> {
        let internal = crate::types::existing_type_name(internal)?;
        self.static_field_name(internal, name)
    }

    fn static_field_name(
        &self,
        internal: TypeName,
        name: &str,
    ) -> Option<crate::libraries::StaticFieldRef> {
        let mut stack = vec![internal];
        let mut seen = std::collections::HashSet::new();
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            let Some(ci) = self.cp.find_name(cur) else {
                continue;
            };
            if let Some(f) = ci.fields.iter().find(|f| {
                f.name == name
                    && f.access & 0x0008 != 0
                    && f.access & 0x0001 != 0
                    && f.const_value.is_none()
            }) {
                return Some(crate::libraries::StaticFieldRef {
                    owner: cur,
                    name: name.to_string(),
                    descriptor: f.descriptor.clone(),
                    ty: field_desc_to_ty(&f.descriptor),
                });
            }
            if let Some(s) = ci.super_class {
                stack.push(s);
            }
            stack.extend(ci.interfaces.iter_ids());
        }
        None
    }

    fn extension_receiver_rank(&self, recv: Ty, decl_recv: Ty) -> Option<u32> {
        // VALUE-CLASS receivers match by IDENTITY, never by erasing to the underlying: a `UInt` receiver
        // binds only a `UInt` extension (`UInt.downTo` → `UIntProgression`), never `Int`'s — they share the
        // `I` descriptor, so the erased MRO below would tie them. Reject a value-class mismatch on either
        // side up front; a genuine `UInt.downTo` matches at rung 0.
        let recv_vc = SemanticPlatform::value_underlying(self, recv).is_some();
        let decl_vc = SemanticPlatform::value_underlying(self, decl_recv).is_some();
        if recv_vc || decl_vc {
            return (recv.obj_internal() == decl_recv.obj_internal() && recv == decl_recv)
                .then_some(0);
        }
        // Index the extension's declared-receiver descriptor into the receiver's most-specific-first MRO —
        // the same supertype/widening order the classpath extension lookup ranks by (`Int → Number →
        // Comparable → Any`, `List → Collection → Iterable`), so most-specific selection is preserved. The
        // MRO carries JVM-form descriptors (`Ljava/lang/Object;`), so normalize the declared receiver to the
        // same form first (`kotlin/Any` → `java/lang/Object`, a Kotlin collection → its `java/util/*` face).
        let want = type_descriptor(
            <Self as crate::libraries::SemanticPlatform>::library_value_form(self, decl_recv),
        );
        let rank = supertype_descriptors(&self.cp, recv)
            .iter()
            .position(|d| *d == want)
            .map(|i| i as u32)
            .or_else(|| {
                // A universal receiver (`<T> T.let`, erased to `Any`/`Object`) applies to every receiver at
                // the lowest precedence when the MRO does not list the implicit root explicitly.
                matches!(want.as_str(), "Ljava/lang/Object;").then_some(u32::MAX - 1)
            })?;
        // ELEMENT (type-argument) compatibility: the erased receivers matched, but a concrete element must
        // agree — `Iterable<Int>.sum` (`@JvmName sumOfInt`) must not bind an `Iterable<Long>` receiver. A
        // type-VARIABLE decl element (a generic extension, decoded to `Any`) matches any element.
        let any = Ty::obj("kotlin/Any");
        if let (Some(re), Some(de)) = (
            recv.type_args().first().copied(),
            decl_recv.type_args().first().copied(),
        ) {
            if re != any && de != any && self.library_value_form(re) != self.library_value_form(de)
            {
                return None;
            }
        }
        Some(rank)
    }

    fn value_underlying(&self, ty: Ty) -> Option<Ty> {
        match ty {
            Ty::UInt => Some(Ty::Int),
            Ty::ULong => Some(Ty::Long),
            _ => self.value_underlying_name(ty.obj_internal()?),
        }
    }

    fn library_value_form(&self, ty: Ty) -> Ty {
        // A reference type erases to its JVM internal name (a Kotlin collection → its single
        // `java/util/*` interface) with type arguments dropped — exactly what a descriptor-read
        // constructor/method parameter carries. Arrays recurse into their element (`Array<Set<String>>`
        // → `[Ljava/util/Set;` on the descriptor side), so a nested collection element normalizes too.
        // Other kinds (primitives, `String`, function types) already compare exactly across the sides.
        match ty {
            // A boxed `Array<T>` keeps its kind and recurses into the element (erasing the element's own
            // generic args), so `Array<Set<String>>` → `Array<Set>` (`[Ljava/util/Set;`). Mapping it
            // through the generic `Obj` arm would drop the element entirely and collapse a reference
            // `Array<Int>` (`[Ljava/lang/Integer;`) to a primitive `IntArray` (`[I`).
            _ if ty.is_reference_array() => {
                let e = ty.array_elem().unwrap_or_else(|| Ty::obj("kotlin/Any"));
                Ty::obj_args("kotlin/Array", &[self.library_value_form(e)])
            }
            Ty::Obj(internal, _) => Ty::obj_name(super::jvm_class_map::to_jvm_type_name(internal)),
            _ => ty,
        }
    }

    fn library_value_form_name(&self, internal: TypeName) -> TypeName {
        super::jvm_class_map::to_jvm_type_name(internal)
    }

    fn function_like_arity(&self, ty: Ty) -> Option<usize> {
        ty.fun_arity().map(usize::from).or_else(|| {
            let internal = ty.obj_internal()?;
            if internal.matches("kotlin/reflect/KProperty1")
                || internal.matches("kotlin/reflect/KMutableProperty1")
            {
                Some(1)
            } else if internal.matches("kotlin/reflect/KProperty0")
                || internal.matches("kotlin/reflect/KMutableProperty0")
            {
                Some(0)
            } else {
                None
            }
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

    fn platform_default_import_packages(&self) -> &'static [&'static str] {
        PLATFORM_DEFAULT_IMPORT_PACKAGES
    }

    fn physical_property_getter_name(&self, property: &str) -> Option<String> {
        let getter = property_getter_name(property);
        (getter != property).then_some(getter)
    }

    fn builtin_type_internal(&self, simple_name: &str) -> Option<String> {
        // Collection built-ins keep their `kotlin/collections/…` identity (read-only vs mutable);
        // normalize any JVM spelling the mapping returns back to the Kotlin identity so the core IR
        // stays in Kotlin names (the emitter re-maps at its boundary).
        let internal = crate::jvm::jvm_class_map::kotlin_builtin_to_internal(simple_name)?;
        Some(crate::jvm::jvm_class_map::to_kotlin_internal(internal).to_string())
    }

    fn is_collection_interface(&self, supertype_internal: &str) -> bool {
        crate::jvm::jvm_class_map::to_jvm_internal(supertype_internal).starts_with("java/util/")
    }

    fn is_collection_interface_name(&self, supertype_internal: TypeName) -> bool {
        crate::jvm::jvm_class_map::type_name_maps_to_jvm_collection_interface(supertype_internal)
    }

    fn collection_property_accessor(&self, property: &str) -> Option<String> {
        crate::jvm::names::collection_property_stub_name(property).map(str::to_string)
    }

    fn signature_formal_names(&self, signature: &str) -> Vec<String> {
        signature_formals(signature)
    }

    fn iterable_element_type(&self, internal: &str) -> Option<Ty> {
        self.counted_loop_info_for_type(internal)
            .map(|info| info.elem)
    }

    fn iterable_element_type_name(&self, internal: TypeName) -> Option<Ty> {
        self.counted_loop_info_for_name(internal)
            .map(|info| info.elem)
    }
}

impl crate::runtime::TargetRuntime for JvmLibraries {
    fn property_reference_impl(&self, arity: usize, mutable: bool) -> Option<PlatformCtor> {
        let internal = match (arity, mutable) {
            (0, false) => "kotlin/jvm/internal/PropertyReference0Impl",
            (0, true) => "kotlin/jvm/internal/MutablePropertyReference0Impl",
            (1, false) => "kotlin/jvm/internal/PropertyReference1Impl",
            (1, true) => "kotlin/jvm/internal/MutablePropertyReference1Impl",
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

    fn ir_value_type(&self, ty: Ty) -> Ty {
        crate::jvm::ir_emit::ir_ty_to_jvm(&ty)
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
        ty.scalar_value_repr()
    }

    fn unsigned_integer_box_type(&self, ty: Ty) -> Option<Ty> {
        ty.boxed_ref().filter(|_| ty.is_unsigned())
    }

    fn counted_loop_info(&self, internal: &str) -> Option<CountedLoopInfo> {
        self.counted_loop_info_for_type(internal)
    }

    fn counted_loop_info_name(&self, internal: TypeName) -> Option<CountedLoopInfo> {
        self.counted_loop_info_for_name(internal)
    }

    fn range_construction(&self, lo: Ty, hi: Ty) -> Option<RangeConstruction> {
        let (internal, elem, trailing_nulls) = match (lo, hi) {
            (Ty::Char, Ty::Char) => ("kotlin/ranges/CharRange", Ty::Char, 0),
            (Ty::UInt, Ty::UInt) => ("kotlin/ranges/UIntRange", Ty::UInt, 1),
            (Ty::ULong, Ty::ULong) => ("kotlin/ranges/ULongRange", Ty::ULong, 1),
            (Ty::Double, Ty::Double) => ("kotlin/ranges/ClosedDoubleRange", Ty::Double, 0),
            (Ty::Float, Ty::Float) => ("kotlin/ranges/ClosedFloatRange", Ty::Float, 0),
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
            Ty::Double | Ty::Float => None,
            Ty::UInt => Some(LibraryCallable::library(
                type_name("kotlin/ranges/URangesKt"),
                "until-J1ME1BU",
                vec![elem, elem],
                Ty::obj(internal),
                Ty::obj(internal),
                format!("({prim}{prim})L{internal};"),
            )),
            Ty::ULong => Some(LibraryCallable::library(
                type_name("kotlin/ranges/URangesKt"),
                "until-eb3DHEI",
                vec![elem, elem],
                Ty::obj(internal),
                Ty::obj(internal),
                format!("({prim}{prim})L{internal};"),
            )),
            _ if trailing_nulls == 0 => Some(LibraryCallable::library(
                type_name("kotlin/ranges/RangesKt"),
                "until",
                vec![elem, elem],
                Ty::obj(internal),
                Ty::obj(internal),
                format!("({prim}{prim})L{internal};"),
            )),
            _ => None,
        };
        let (result, through_static) = match elem {
            Ty::Double | Ty::Float => {
                let iface = "kotlin/ranges/ClosedFloatingPointRange";
                (
                    Ty::obj(iface),
                    Some(LibraryCallable::library(
                        type_name("kotlin/ranges/RangesKt"),
                        "rangeTo",
                        vec![elem, elem],
                        Ty::obj(iface),
                        Ty::obj(iface),
                        format!("({prim}{prim})L{iface};"),
                    )),
                )
            }
            _ => (Ty::obj(internal), None),
        };
        Some(RangeConstruction {
            elem,
            result,
            through,
            until,
            through_static,
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
            Some(LibraryCallable::library(
                type_name(owner),
                name,
                params,
                ret,
                physical_ret,
                descriptor,
            ))
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
                let cmp_ty = ty.int_arithmetic_repr();
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
                let desc = match array_kotlin_fq(ty.non_null())? {
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
                let desc = match array_kotlin_fq(ty.non_null())? {
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
        callable.owner_matches("kotlin/test/AssertionsKt__AssertionsKt")
            && callable.name == "assertFailsWith$default"
    }
}

#[cfg(test)]
mod tests {
    use super::desc_to_ty;
    use crate::types::Ty;

    #[test]
    fn descriptor_void_reference_normalizes_before_core() {
        assert_eq!(desc_to_ty("Ljava/lang/Void;"), Ty::Unit);
        assert_eq!(desc_to_ty("V"), Ty::Unit);
    }
}

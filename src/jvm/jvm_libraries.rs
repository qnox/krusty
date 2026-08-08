//! The JVM implementation of the [`SymbolSource`] abstraction: resolves symbols from a `.class`-jar
//! classpath (the bytecode target). All classpath reads, JVM method-descriptor parsing, and
//! `java/lang ↔ kotlin` name normalization live here — the front end (`resolve`, `ir_lower`) sees
//! only Kotlin-level `Ty`s and opaque descriptor tokens through the trait.

use super::classpath::{
    kotlin_name_to_ty, kotlin_type_name_to_ty, metadata_return_info, Classpath,
};
use super::classreader::{ConstVal, FieldSig, JavaNullability};
use super::jvm_class_map::to_kotlin_internal;
use super::metadata;
use crate::jvm::names::{method_descriptor, property_getter_name, type_descriptor};
use crate::libraries::{
    CallSig, FnFlags, FnKind, FunctionInfo, FunctionSet, GenericReturnPolicy, GenericSig,
    InlineKind, LibConst, LibraryCallable, LibraryConst, LibraryField, LibraryMember, LibraryType,
    PropKind, PropertyInfo, PropertySet, ReturnInfo, SemanticPlatform, Visibility,
};
use crate::runtime::{
    CountedLoopInfo, PlatformAccessor, PlatformCtor, PlatformField, PlatformRangeCtor,
    RangeConstruction, RuntimeCtor, RuntimeOp,
};
use crate::symbol_resolver::{ty_subst, ty_subst_all, ty_subst_keep_unbound};
use crate::symbol_source::{InheritanceShape, SymbolSource};
use crate::types::{type_name, Ty, TypeName, TypeNameList};

fn effective_class_access(class: &super::classreader::ClassInfo) -> u16 {
    class
        .inner_class_self()
        .map(|entry| entry.access)
        .unwrap_or(class.access)
}

fn internal_package(internal: TypeName) -> String {
    let rendered = super::jvm_class_map::to_jvm_type_name(internal).render();
    rendered
        .rsplit_once('/')
        .map_or_else(String::new, |(package, _)| package.to_string())
}

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

/// The declaration-level return fact shared by both classpath member construction loops. Keep this
/// normalization at the metadata boundary: ordinary descriptor members and source-name aliases for
/// mangled members must not disagree about whether the later value-class representation pass may see
/// a declared return.
///
/// Nullable returns are genuine boxes, and suspend descriptors return the CPS `Object` regardless of
/// the declared type (including primitive-underlying value classes, which the callee boxes). In both
/// cases recording the classifier as an erased carrier would be unsound, so neither is handed off.
/// Value-class identification itself intentionally remains downstream; probing it while a classpath
/// type is being built can recursively re-enter type resolution on cyclic class graphs.
fn metadata_declared_nonnull_nonsuspend_return(function: &super::metadata::MetaFn) -> Option<Ty> {
    function
        .ret_class
        .filter(|_| !function.ret_nullable() && !function.is_suspend())
        .map(Ty::obj_name)
}

impl JvmLibraries {
    /// The TOP-LEVEL (receiver-less) function overloads of `name` declared in package `pkg` —
    /// `listOf`/`run`/`println`/… each with its inline/`@InlineOnly` flags and logical
    /// (continuation-stripped) suspend signature. The building block `resolve_symbols` uses so a
    /// top-level name resolves through the ONE fqn seam without the removed receiver-indexed
    /// `functions()` query. Candidates come from the per-(jar, package) SOURCE-keyed member index
    /// (`functions_in_scope`) — only the queried package's facades are consulted (never a whole-
    /// classpath name index), the source-name keying finds a `@JvmName`-mangled declaration under
    /// the name a caller writes, and a `$default`/synthetic static (absent from `@Metadata`) is
    /// keyed by its literal JVM name. The index also surfaces an extension's compiled form; each
    /// candidate is classified honestly by its metadata receiver, so the caller filters by
    /// `FnKind` as needed.
    fn top_level_overloads(&self, name: &str, pkg: TypeName) -> Vec<FunctionInfo> {
        let mut overloads = Vec::new();
        for c in self.cp.functions_in_scope(name, &[pkg]) {
            // Accessors and functions share the bytecode static-method index.
            if self
                .cp
                .meta_properties_name(c.owner)
                .iter()
                .any(|property| {
                    property
                        .getter
                        .as_ref()
                        .is_some_and(|getter| getter.name == c.name && getter.desc == c.descriptor)
                        || property.setter.as_ref().is_some_and(|setter| {
                            setter.name == c.name && setter.desc == c.descriptor
                        })
                })
            {
                continue;
            }
            let is_default = c.name.ends_with("$default");
            let meta_name = c.name.strip_suffix("$default").unwrap_or(&c.name);
            let Some((mut params, physical_ret)) =
                parse_method_desc_with_field_params(&c.descriptor)
            else {
                continue;
            };
            if is_default && params.len() >= 2 {
                // A `$default` synthetic appends mask/marker slots that do not identify the source
                // overload. Remove that ABI tail BEFORE metadata alignment: otherwise the mask of a
                // two-parameter overload can prefix-match a real third `Int` parameter and make the
                // longer sibling win. A suspend Continuation precedes this tail and deliberately
                // remains for the selected metadata declaration to classify and trim generically.
                params.truncate(params.len() - 2);
            }
            // Select metadata ONCE from the JVM name and descriptor-derived parameter shape. A
            // `$default` synthetic has no entry of its own, so its suffix and mask/marker ABI tail are
            // normalized first; a value-class-mangled name stays intact and matches `MetaFn.jvm_name`.
            // Suspend-ness is one fact on that exact callable, beside arity/default/return facts —
            // never a name-wide lookup that can leak to an ordinary same-named sibling.
            let meta = self.cp.metadata_call_facts_name(
                c.owner,
                meta_name,
                &params,
                &physical_ret,
                false,
                &|name| self.value_underlying_name(name),
            );
            let suspend = meta.suspend;
            // A `suspend fun`'s physical method appends a `Continuation` parameter and erases the
            // return to `Object`; present the LOGICAL signature (drop the continuation) so a normal
            // call resolves. The coroutine pass re-derives the CPS form for the emitted call.
            let descriptor = if suspend {
                strip_continuation_param(&c.descriptor)
            } else {
                c.descriptor.clone()
            };
            // Drop any SYNTHETIC trailing params the JVM descriptor appends beyond the `@Metadata`
            // SOURCE signature — a CPS Continuation, `$default` mask/marker, or `@Composable`
            // `(Composer, int)` tail. The descriptor-aligned fact reports the source prefix from the
            // same declaration that supplied `suspend`; ordinary methods with a Continuation parameter
            // keep it because it is part of THEIR aligned source arity.
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
            let inline = self.cp.is_inline_callable_name(
                c.owner,
                meta_name,
                &inline_desc,
                &params,
                &|name| self.value_underlying_name(name),
            );
            let call_sig = meta.call_sig;
            let contract = meta.contract.clone();
            let context_count = meta.context_count;
            // Context parameters' metadata nullability rides `platform_nullable_params`; matching
            // applies it arg-driven (Java semantics), but the checker ALSO needs it on the stored
            // callable (context source resolution reads the callable's context param types
            // directly). Apply the metadata-declared flags unconditionally — they are Kotlin
            // declarations, not platform types.
            if context_count > 0 {
                for (p, &nullable) in params
                    .iter_mut()
                    .zip(call_sig.platform_nullable_params.iter())
                {
                    if nullable && p.is_reference() {
                        *p = Ty::nullable(*p);
                    }
                }
            }
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
                // A value-class RETURN erases to its underlying in the descriptor exactly like a
                // parameter does, and unlike `suspend` it has no continuation type argument to recover
                // it from — so without this a non-suspend top-level function declared `(): Duration`
                // reads back as `Long` and every member access on its result fails to resolve. The
                // callable keeps `physical_ret`/`descriptor` erased below, which is what tells the
                // value-class pass the result is ALREADY the carrier and must not be unboxed again.
                meta.value_class_ret.unwrap_or(physical_ret)
            };
            // A value-class parameter erases to its underlying in the descriptor. Resolution compares
            // the ARGUMENT's Kotlin type against these, so a `Duration`/`Tag` argument matches nothing
            // while they stay erased. Restore the declared type — LAST, after every metadata/bytecode
            // alignment above has matched against the erased form the class file actually spells. The
            // emit `descriptor` keeps the physical form and the value-classes pass unboxes at the call,
            // exactly how a mangled MEMBER with a value-class parameter is already exposed.
            for (parameter, declared) in params.iter_mut().zip(&meta.value_class_params) {
                if let Some(declared) = declared {
                    *parameter = *declared;
                }
            }
            let inline_kind = InlineKind::from_flags(inline, inline && !c.public);
            let generic_sig_for_callable = self.callable_generic_sig(
                c.owner,
                &c.name,
                &c.descriptor,
                c.signature.as_deref(),
                false,
            );
            let callable = LibraryCallable {
                inline: inline_kind,
                suspend,
                default_call: is_default,
                signature: c.signature.clone(),
                context_count,
                contract,
                generic_sig: generic_sig_for_callable.clone().map(Box::new),
                // The DECLARED value-class return, when `@Metadata` says the descriptor return is that
                // class's erased carrier. This is the fact the value-class pass needs and the
                // descriptor cannot supply; it is already computed for `ret` above.
                declared_ret: meta.value_class_ret,
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
            let generic_sig = generic_sig_for_callable;
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
                context_count,
                flags: FnFlags {
                    inline: inline_kind,
                    suspend,
                    operator: meta.is_operator,
                },
                ..FunctionInfo::plain(kind, None, callable)
            });
        }
        overloads
    }

    /// The OBJECT an import path's parent denotes, as a class internal name — or `None` when it denotes
    /// a package (the common case) or a non-object type.
    ///
    /// An import path separates every segment alike (`kotlin/time/Duration/Companion`), but a NESTED
    /// class is spelled with `$` (`kotlin/time/Duration$Companion`). Which trailing segments are nesting
    /// is not knowable from the path, so each split point is tried outward-in until one names an object.
    fn object_owner_internal(
        &self,
        path: TypeName,
    ) -> Option<(TypeName, crate::libraries::StaticFieldRef)> {
        let rendered = path.render();
        // A plain `object` is `TypeKind::Object` — it carries its own `INSTANCE`. A COMPANION object does
        // not: its singleton is a static field on the OUTER class. Both facts already belong to the
        // backend-neutral `LibraryType` contract (`kind` and `companion_object`), so consume that one
        // semantic view rather than re-reading raw classfile fields here. Besides avoiding two object
        // classifiers, this keeps named companions and non-JVM symbol providers on the same boundary.
        let singleton =
            |candidate: TypeName| -> Option<(TypeName, crate::libraries::StaticFieldRef)> {
                let name = candidate.render();
                let descriptor = format!("L{name};");
                let field = |owner: TypeName, field_name: &str| crate::libraries::StaticFieldRef {
                    owner,
                    name: field_name.to_string(),
                    descriptor: Some(descriptor.clone()),
                    ty: Ty::obj_name(candidate),
                    constant: None,
                };
                let classifier = self.resolve_type_name(candidate)?;
                if classifier.is_object() {
                    return Some((candidate, field(candidate, "INSTANCE")));
                }
                let (outer, simple) = name.rsplit_once('$')?;
                let outer = type_name(outer);
                let outer_type = self.resolve_type_name(outer)?;
                let (holder_name, companion_type) = outer_type.companion_object.as_ref()?;
                // Requiring both the semantic companion type and its declared field name prevents an
                // ordinary nested class held in some unrelated static field from masquerading as the
                // import's singleton parent.
                if *companion_type != candidate || holder_name != simple {
                    return None;
                }
                Some((candidate, field(outer, holder_name)))
            };
        if let Some(hit) = singleton(path) {
            return Some(hit);
        }
        // Nesting is a SUFFIX property, so convert trailing separators one at a time, keeping the ones
        // already converted: `a/b/Outer/Inner` → `a/b/Outer$Inner` → `a/b$Outer$Inner`.
        let mut candidate = rendered.clone();
        for (at, _) in rendered.match_indices('/').rev() {
            candidate.replace_range(at..=at, "$");
            if let Some(hit) = singleton(type_name(&candidate)) {
                return Some(hit);
            }
        }
        None
    }

    /// Member EXTENSIONS named `name` declared inside the `object` / `companion object` `owner`, as
    /// extension callables dispatched on that object's singleton.
    ///
    /// `import Duration.Companion.minutes` puts `minutes` in scope as an extension on `Int`, with
    /// `Duration.Companion` supplying the dispatch receiver. Everything about selection is then ordinary
    /// extension resolution — only the EMIT differs, and that difference rides on the callable as
    /// [`LibraryCallable::singleton_dispatch`]. Nothing is contributed when `owner` is not an object
    /// (the overwhelmingly common case: the parent really is a package).
    fn object_member_extensions(
        &self,
        owner: TypeName,
        name: &str,
        overloads: &mut Vec<FunctionInfo>,
        props: &mut Vec<PropertyInfo>,
    ) {
        let Some((owner, singleton)) = self.object_owner_internal(owner) else {
            return;
        };
        let Some(class) = self.cp.find_name(owner) else {
            return;
        };
        // A member's emit handle is the class's OWN method — resolved from `@Metadata`'s recorded JVM
        // name + descriptor, never a `getX` guess, so `@JvmName` and value-class mangling
        // (`getMinutes-UwyO8pc`) are carried verbatim.
        //
        // A NON-public one is `@InlineOnly` (`Duration.Companion`'s `minutes` accessor is `private`):
        // there is no legal call site for it, so it must be SPLICED, which is what `MustInline` tells
        // the backend. Returning the inline kind rather than a yes/no keeps that distinction where the
        // bytecode fact is read.
        let declared = |jvm_name: &str, desc: &str| {
            class
                .methods
                .iter()
                .find(|method| method.name == jvm_name && method.descriptor == desc)
                .map(|method| InlineKind::from_flags(!method.is_public(), !method.is_public()))
        };
        for function in super::metadata::class_functions(&class) {
            if function.kotlin_name != name || !function.is_extension() || !function.is_public() {
                continue;
            }
            // A `suspend` member extension appends a `Continuation` parameter and erases its return,
            // and the singleton dispatch does not yet thread the CPS form. Surfacing it would let the
            // front end accept a call the backend then drops — a clean compile that emits nothing,
            // which is exactly the silent-green shape this work set out to remove. Leave it
            // unresolved (a diagnostic) until the CPS shape is threaded here.
            if function.is_suspend() {
                continue;
            }
            let Some(desc) = function.jvm_desc else {
                continue;
            };
            let Some(function_inline) = declared(&function.jvm_name, desc) else {
                continue;
            };
            let Some((params, physical_ret)) = parse_method_desc(desc) else {
                continue;
            };
            if params.is_empty() {
                continue; // an extension's first parameter IS its receiver
            }
            let generic_sig = function.generic_sig.clone();
            let receiver = generic_sig
                .as_ref()
                .and_then(|gsig| gsig.receiver)
                .or_else(|| function.receiver_class.map(kotlin_type_name_to_ty))
                .unwrap_or(params[0]);
            let ret = metadata_return_info(function.ret_class, function.ret_nullable())
                .apply(physical_ret);
            let callable = LibraryCallable {
                suspend: function.is_suspend(),
                inline: function_inline,
                context_count: function.context_count,
                generic_sig: generic_sig.clone().map(Box::new),
                singleton_dispatch: Some(Box::new(singleton.clone())),
                ..LibraryCallable::library(
                    owner,
                    function.jvm_name.clone(),
                    params,
                    ret,
                    physical_ret,
                    desc.to_string(),
                )
            };
            overloads.push(FunctionInfo {
                ret: metadata_return_info(function.ret_class, function.ret_nullable()),
                visibility: function.visibility,
                generic_sig,
                context_count: function.context_count,
                call_sig: function.member_call_sig(),
                flags: FnFlags {
                    inline: InlineKind::None,
                    suspend: function.is_suspend(),
                    operator: function.is_operator(),
                },
                ..FunctionInfo::plain(FnKind::Extension, Some(receiver), callable)
            });
        }
        for property in super::metadata::class_properties(&class) {
            if property.name != name || !property.is_extension {
                continue;
            }
            let Some(getter_sig) = property.getter.as_ref() else {
                continue;
            };
            let Some(getter_inline) = declared(&getter_sig.name, &getter_sig.desc) else {
                continue;
            };
            let Some((getter_params, getter_ret)) = parse_method_desc(&getter_sig.desc) else {
                continue;
            };
            if getter_params.len() != 1 {
                continue; // exactly the extension receiver
            }
            let property_gsig = property.generic_sig.clone();
            let fallback = property
                .ret_class
                .map_or(getter_ret, kotlin_type_name_to_ty);
            let property_ty = property_gsig.as_ref().map_or_else(
                || {
                    if property.ret_nullable {
                        Ty::nullable(fallback)
                    } else {
                        fallback
                    }
                },
                |gsig| gsig.ret,
            );
            let accessor = |jvm_name: &str,
                            desc: &str,
                            params: Vec<Ty>,
                            ret: Ty,
                            physical: Ty,
                            inline: InlineKind| {
                LibraryCallable {
                    inline,
                    singleton_dispatch: Some(Box::new(singleton.clone())),
                    ..LibraryCallable::library(
                        owner,
                        jvm_name.to_string(),
                        params,
                        ret,
                        physical,
                        desc.to_string(),
                    )
                }
            };
            let setter = property.setter.as_ref().and_then(|setter_sig| {
                let setter_inline = declared(&setter_sig.name, &setter_sig.desc)?;
                let (params, ret) = parse_method_desc(&setter_sig.desc)?;
                (params.len() == 2 && ret == Ty::Unit).then(|| {
                    accessor(
                        &setter_sig.name,
                        &setter_sig.desc,
                        params,
                        Ty::Unit,
                        ret,
                        setter_inline,
                    )
                })
            });
            props.push(PropertyInfo {
                kind: PropKind::Extension,
                receiver: property_gsig
                    .as_ref()
                    .and_then(|gsig| gsig.receiver)
                    .or_else(|| {
                        Some(
                            property
                                .receiver_class
                                .map_or(getter_params[0], Ty::obj_name),
                        )
                    }),
                formals: property_gsig
                    .as_ref()
                    .map(|gsig| gsig.formals.clone())
                    .unwrap_or_default(),
                ty: property_ty,
                context_count: 0,
                getter: accessor(
                    &getter_sig.name,
                    &getter_sig.desc,
                    getter_params,
                    property_ty,
                    getter_ret,
                    getter_inline,
                ),
                setter,
                is_const: property.is_const,
                visibility: property.visibility,
                owner,
                receiver_rank: 0,
                source_key: None,
            });
        }
    }

    pub fn new(cp: std::rc::Rc<Classpath>) -> JvmLibraries {
        JvmLibraries { cp }
    }

    fn library_const(value: &ConstVal) -> LibConst {
        match value {
            ConstVal::Int(value) => LibConst::Int(*value),
            ConstVal::Long(value) => LibConst::Long(*value),
            ConstVal::Float(value) => LibConst::Float(*value),
            ConstVal::Double(value) => LibConst::Double(*value),
            ConstVal::Str(value) => LibConst::Str(value.clone()),
        }
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
                let value = Self::library_const(f.const_value.as_ref()?);
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
                .iter()
                .filter_map(|p| p.ret_class.map(|ret| (p.name.clone(), ret)))
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

    fn builtin_members_for_type_name(&self, internal: TypeName) -> Vec<LibraryMember> {
        let kotlin = crate::jvm::jvm_class_map::jvm_to_kotlin_builtin_with_members_name(internal)
            .unwrap_or(internal);
        self.cp.builtin_members_name(kotlin)
    }

    /// Function renames declared by the mapped collection interfaces a concrete JVM class realizes.
    /// This is the read-side counterpart of [`SymbolSource::mapped_interface_members`], which already
    /// owns the source name, physical name, and erased callable shape used for bridge emission. Derive
    /// Java member visibility from that semantic handoff instead of maintaining a second reverse table:
    /// a raw `ArrayList.remove(I)Object` therefore appears as `removeAt`, while `remove(Object)Boolean`
    /// remains `remove`, and a same-shaped method outside the `java.util.List` hierarchy is untouched.
    fn mapped_collection_function_renames(
        &self,
        internal: TypeName,
    ) -> Vec<crate::libraries::MappedInterfaceMember> {
        let mut renames = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut pending = std::collections::VecDeque::new();
        pending.push_back(super::jvm_class_map::to_jvm_type_name(internal));
        while let Some(owner) = pending.pop_front() {
            if !seen.insert(owner) {
                continue;
            }
            if let Some(kotlin) =
                super::jvm_class_map::jvm_collection_to_kotlin_mutable_type_name(owner)
            {
                for mapping in
                    <Self as SemanticPlatform>::mapped_interface_members(self, Ty::obj_name(kotlin))
                {
                    if !mapping.is_property
                        && mapping.source_name != mapping.physical_name
                        && !renames.iter().any(
                            |existing: &crate::libraries::MappedInterfaceMember| {
                                existing.source_name == mapping.source_name
                                    && existing.physical_name == mapping.physical_name
                                    && existing.params == mapping.params
                                    && existing.ret == mapping.ret
                            },
                        )
                    {
                        renames.push(mapping);
                    }
                }
            }
            if let Some(class) = self.cp.find_name(owner) {
                pending.extend(class.interfaces.iter_ids().chain(class.super_class));
            }
        }
        renames
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

    /// The generic signature for `owner.jvm_name`, metadata-primary. When `@Metadata` DESCRIBES this
    /// function (a Kotlin callable), its metadata gsig is authoritative — the JVM-agnostic, Kotlin-faithful
    /// signature (nullability, variance, Kotlin type identities, and no synthetic `suspend` Continuation /
    /// `@Composable` params, which are emit-only). There is NO fallback to the JVM `Signature` attribute
    /// here: a metadata function that fails to decode is a decoder BUG to fix, not something to paper over
    /// with the erased-ish JVM sig. The `Signature` attribute is consulted ONLY when `@Metadata` has no
    /// record for the name — a Java class, a synthetic/bridge method, or a facade part metadata omits.
    fn callable_generic_sig(
        &self,
        owner: TypeName,
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
        let (desc_params, desc_ret) = parse_method_desc_with_field_params(jvm_desc)?;
        if let Some(gsig) =
            self.cp
                .aligned_generic_sig_name(owner, jvm_name, &desc_params, &desc_ret, &|name| {
                    self.value_underlying_name(name)
                })
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

    /// Receiver type-parameter bindings at a classpath hierarchy node.
    fn receiver_type_bindings_name(
        &self,
        receiver: Ty,
        target_internal: TypeName,
    ) -> std::collections::HashMap<String, Ty> {
        let Ty::Obj(start, start_args) = receiver else {
            return std::collections::HashMap::new();
        };
        let target = super::jvm_class_map::to_jvm_type_name(target_internal);
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
            let ci = self.cp.find_name(internal);
            // A Kotlin builtin whose mapped JVM class is absent (a no-JDK compile) has no `.class` and
            // so no class `Signature`; its `.kotlin_builtins` declaration carries the same two facts —
            // the formals and the argument-carrying supertypes — so bind through that instead.
            let (formals, supers) = match &ci {
                Some(ci) => ci.signature.as_deref().and_then(parse_class_gsig).unzip(),
                None => self.cp.builtin_class_gsig_name(internal).unzip(),
            };
            if ci.is_none() && formals.is_none() {
                continue;
            }
            let formals = formals.unwrap_or_default();
            let binds: std::collections::HashMap<String, Ty> =
                formals.iter().cloned().zip(targs.iter().copied()).collect();
            if internal == target {
                return binds;
            }
            match (supers, &ci) {
                (Some(supers), _) => {
                    for sup in supers {
                        if let Ty::Obj(sup_internal, sup_args) = sup {
                            let sup_targs = ty_subst_all(sup_args, &binds);
                            q.push_back((
                                super::jvm_class_map::to_jvm_type_name(sup_internal),
                                sup_targs,
                            ));
                        }
                    }
                }
                (None, Some(ci)) => {
                    for i in ci.interfaces.iter_ids().chain(ci.super_class) {
                        q.push_back((i, vec![]));
                    }
                }
                (None, None) => {}
            }
        }
        std::collections::HashMap::new()
    }

    /// Class bindings excluding method parameters that shadow them.
    fn member_receiver_bindings_name(
        &self,
        receiver: Ty,
        target_internal: TypeName,
        method_formals: &[String],
    ) -> std::collections::HashMap<String, Ty> {
        let mut bindings = self.receiver_type_bindings_name(receiver, target_internal);
        for formal in method_formals {
            bindings.remove(formal);
        }
        bindings
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
            let Some((params, ret)) = parse_method_desc(&m.descriptor) else {
                continue;
            };
            let mut member = LibraryMember::new(m.name.clone(), params, ret, m.descriptor.clone());
            member.signature = m.signature.clone();
            member.generic_sig = m.signature.as_deref().and_then(parse_method_gsig);
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
            .iter()
            .filter(|m| m.is_public())
            .filter_map(|m| {
                let descriptor = m.jvm_desc?;
                let (params, _) = parse_method_desc(descriptor)?;
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
                            m.jvm_name.clone(),
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
            .filter(|m| m.is_public() && !m.is_extension())
            .filter_map(|m| {
                let descriptor = m.jvm_desc?.to_string();
                let (params, physical_ret) = parse_method_desc(&descriptor)?;
                // Value-class implementation methods are static and take the erased receiver as their
                // first JVM parameter. Source member resolution sees only the value parameters.
                let logical_params = params.get(1..).unwrap_or(&[]).to_vec();
                let ret = metadata_return_info(m.ret_class, m.ret_nullable()).apply(physical_ret);
                let mut member =
                    LibraryMember::new(m.kotlin_name.clone(), logical_params, ret, descriptor);
                member.owner = Some(type_name(&ci.this_class()));
                member.physical_name = Some(m.jvm_name.clone());
                member.physical_ret = physical_ret;
                member.set_ret_nullable(m.ret_nullable());
                member.inline = InlineKind::from_flags(m.is_inline(), m.is_inline());
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
            .iter()
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
                Some((p.name.clone(), member))
            })
            .collect()
    }

    /// The one-time (per memo miss) composition of a classpath type's shape. A Kotlin MAPPED type
    /// (`kotlin.collections.List`, `kotlin.CharSequence`, …) has no own JVM
    /// `.class` — its *actual* platform declaration IS a JVM type (`java/util/List`), exactly the
    /// `expect`/`actual` + `JavaToKotlinClassMap` device kotlinc uses. When the classpath has no class
    /// for the Kotlin name, resolve members against that mapped (actual) type — the SAME generic
    /// mapping (`to_jvm_internal`) the emitter uses for the call owner, so resolution and codegen stay
    /// byte-consistent. Members/return types erase to the JVM forms (`get(int)Object`, etc.).
    fn build_library_type(&self, internal_name: TypeName) -> Option<LibraryType> {
        // `kotlin.reflect.KFunction0` … `KFunction22` are COMPILER-SYNTHESIZED, exactly like
        // `kotlin.FunctionN`: they exist in no jar, and the `kotlin/reflect` builtins declare only the
        // arity-less `KFunction`. Build the shape kotlinc gives them — `KFunction<R>` for the reflection
        // members (`returnType`, `name`, …) plus `FunctionN` so the value is invocable — instead of
        // reporting `import kotlin.reflect.KFunction0` as an unresolved reference. A declaration typed
        // with one erases to `Lkotlin/reflect/KFunction;` (see `jvm_class_map::to_jvm_internal`).
        if let Some(arity) = crate::types::kfunction_arity(internal_name) {
            let mut shape =
                (*self.resolve_type_name(type_name(crate::types::KFUNCTION_INTERNAL))?).clone();
            let mut supertypes = crate::types::TypeNameList::new();
            supertypes.push(crate::types::KFUNCTION_INTERNAL);
            supertypes.push(&format!("kotlin/Function{arity}"));
            shape.supertypes = supertypes;
            shape.type_params = (0..arity)
                .map(|index| format!("P{}", index + 1))
                .chain(std::iter::once("R".to_string()))
                .collect();
            return Some(shape);
        }
        {
            let internal = &internal_name.render();
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
                                    self.cp.builtin_supertypes_name(internal_name),
                                    self.builtin_members_for_type_name(internal_name),
                                    self.cp
                                        .builtin_class_gsig_name(internal_name)
                                        .map(|(formals, _)| formals)
                                        .unwrap_or_default(),
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
            let mut enum_entries_accessor = None;
            let is_java = !ci.meta.is_present();
            // Identify the generated accessor while decoding JVM declarations, where metadata and
            // class flags are authoritative. The backend-neutral checker then consumes an explicit
            // capability instead of inferring declaration origin from a same-named static method.
            let owns_enum_entries_accessor =
                ci.meta.is_present() && ci.access & crate::jvm::classreader::ACC_ENUM != 0;
            // `Map.put` returns the PREVIOUS value (`V?`, null for a fresh key) — Kotlin enhances this Java
            // method's nullability. It applies to ANY `Map` subtype (`HashMap`, `TreeMap`, …), since a call
            // resolves the member on the concrete class, not on `Map` itself.
            let is_map = class_implements_name(&self.cp, internal_name, type_name("java/util/Map"));
            // The class's `@Metadata` function records — carry each member's SOURCE parameter names and
            // default flags, which the erased JVM descriptor loses. Populate every member's `call_sig` from
            // its record so a named-argument / omitted-`$default` member call resolves through the ONE
            // `resolve_type` member seam (the `instance_members` query), not a separate `functions()` walk.
            let meta_fns = metadata::class_functions(&ci);
            // The class's `@Metadata` PROPERTY records — the only carrier of a fun-typed property's
            // full Kotlin shape (receiver mark, nullability), which both the field descriptor and the
            // accessor `Signature` erase.
            let meta_props = metadata::class_properties(&ci);
            // The class's `@Metadata` CONSTRUCTOR records — the only place a constructor parameter's
            // source-level shape survives (a receiver function type erases to `FunctionN` in both the
            // descriptor and the `Signature`).
            let ctor_param_lists = metadata::class_constructor_params(&ci);
            for m in &ci.methods {
                if m.is_bridge()
                    && ci.methods.iter().any(|target| {
                        !target.is_bridge()
                            && target.name == m.name
                            && target.has_same_parameter_descriptor(m)
                    })
                {
                    continue;
                }
                // Public members are callable from anywhere; a `protected` member is surfaced too so a
                // subclass can reach it through the supertype walk (a compiling program only reaches it
                // from a legal subclass, which kotlinc already checked). Private/package members stay
                // dropped: no legal call site.
                if !m.is_public() && !m.is_protected() {
                    continue;
                }
                let Some((params, ret)) = parse_method_desc(&m.descriptor) else {
                    continue;
                };
                // NO value-class probing here: this runs INSIDE `resolve_type_name`'s type build,
                // so querying `value_underlying_name` (which itself resolves types) recurses
                // unboundedly on cyclic class graphs. The exact `jvm_desc` match needs no probing,
                // and resolution-time member alignment (`metadata_member_call_facts_name`) supplies
                // the closure where it is safe.
                let member_metadata = super::classpath::aligned_member_metadata(
                    meta_fns,
                    &m.name,
                    &m.descriptor,
                    &|_| None,
                );
                let platform_nullable_params = is_java.then(|| {
                    params
                        .iter()
                        .enumerate()
                        .map(|(index, parameter)| {
                            parameter.is_reference()
                                && m.parameter_nullability.get(index).copied().flatten()
                                    != Some(JavaNullability::NotNull)
                        })
                        .collect::<Vec<_>>()
                });
                let mut member =
                    LibraryMember::new(m.name.clone(), params, ret, m.descriptor.clone());
                // The DECLARED return classifier, verbatim (see `LibraryMember::declared_ret`). Taken
                // from the metadata this member was already aligned against, so no extra decode; a
                // NULLABLE declared return stays `None` because it is genuinely boxed.
                //
                // A `suspend` member is excluded: CPS makes its descriptor return `Object` whatever it
                // declares, so the descriptor no longer witnesses that the result is the erased
                // carrier — and for a PRIMITIVE-underlying value class it is not (kotlinc's
                // `make-<hash>(Continuation)Ljava/lang/Object;` hands back `M.box-impl(I)LM;`, a BOX).
                // Claiming the fact there would repr a boxed value as unboxed. Without it the member
                // falls back to the descriptor comparison, which classifies this case correctly.
                member.declared_ret =
                    member_metadata.and_then(metadata_declared_nonnull_nonsuspend_return);
                if let Some(java_nullable) = platform_nullable_params.clone() {
                    member.call_sig.platform_nullable_params = java_nullable;
                }
                member.visibility = if m.is_public() {
                    Visibility::Public
                } else {
                    Visibility::Protected
                };
                member.signature = m.signature.clone();
                // The member's parsed generic signature — carries type-variable binding facts so a caller can
                // infer a generic return from the receiver's type arguments (`Repo<Config>.load(): Config`).
                member.generic_sig = m.signature.as_deref().and_then(parse_method_gsig);
                // The JVM `Signature` attribute cannot spell a RECEIVER function type — `Cfg.() -> Unit`
                // and `(Cfg) -> Unit` share the `Function1` erasure — so restore the distinction from
                // `@Metadata`'s per-parameter `@ExtensionFunctionType` mark. Without it a member's
                // receiver-lambda parameter reads as an ordinary function type and no lambda matches it.
                if let (Some(gsig), Some(metadata)) = (member.generic_sig.as_mut(), member_metadata)
                {
                    mark_receiver_fun_params(
                        gsig,
                        &metadata
                            .value_params
                            .iter()
                            .map(metadata::MetaValueParam::recv_fun)
                            .collect::<Vec<_>>(),
                        metadata.is_suspend(),
                    );
                }
                // The same limitation applies to a FUNCTION-typed PROPERTY's accessor: its `Signature`
                // spells the raw `FunctionN` (no receiver mark), so `var handler: (Scope.(Req) -> Resp)?`
                // read back as an ordinary `(Scope, Req) -> Resp` and a lambda assigned to the property
                // bound no `this`. The property's `@Metadata` type keeps the full Kotlin shape; publish
                // it as the getter's logical return. Only a fun-typed property is overlaid — every other
                // accessor keeps its descriptor/`Signature` reading.
                if let Some(property_gsig) = meta_props
                    .iter()
                    .filter(|property| {
                        // A member EXTENSION property's getter takes the extension receiver as a JVM
                        // parameter its metadata gsig models as `receiver` instead — replacing the
                        // whole gsig would desync `params` from the method's. It has its own path.
                        !property.is_extension
                            && property.getter.as_ref().is_some_and(|getter| {
                                getter.name == m.name && getter.desc == m.descriptor
                            })
                    })
                    .find_map(|property| property.generic_sig.as_ref())
                    .filter(|gsig| matches!(gsig.ret.non_null(), Ty::Fun(_)))
                {
                    member.generic_sig = Some(property_gsig.clone());
                }
                // A member EXTENSION is deliberately excluded from `aligned_member_metadata`: its
                // metadata parameter list omits the receiver the JVM method leads with, so the shared
                // alignment cannot line the two up. Match it separately, by the exact descriptor it
                // records. The two facts recovered are ones no descriptor can carry — that the first
                // parameter is a RECEIVER rather than a value, and whether the member is `operator`.
                let member_extension_metadata = meta_fns.iter().find(|function| {
                    function.is_extension()
                        && function.jvm_name == m.name
                        && function.jvm_desc == Some(m.descriptor.as_str())
                });
                let semantic_metadata = member_metadata.or(member_extension_metadata);
                if let Some(metadata) = member_extension_metadata {
                    // Normalize the callable's SOURCE identity at the provider boundary. A value-class
                    // parameter may mangle the JVM method name, but downstream overload selection must
                    // see the Kotlin declaration name and must not grow a classpath-only name branch.
                    // The physical spelling remains on the opaque member handle for emission.
                    if metadata.jvm_name != metadata.kotlin_name {
                        member.physical_name = Some(metadata.jvm_name.clone());
                        member.name = metadata.kotlin_name.clone();
                    }
                    // JVM `Signature` sees the extension receiver as an ordinary leading parameter;
                    // Kotlin metadata represents it as the receiver attribute. Prefer that normalized
                    // semantic shape so generic inference is identical to a module declaration.
                    if metadata.generic_sig.is_some() {
                        member.generic_sig = metadata.generic_sig.clone();
                    }
                }
                member.set_suspend(semantic_metadata.is_some_and(metadata::MetaFn::is_suspend));
                member.set_is_member_extension(member_extension_metadata.is_some());
                member.set_is_operator(
                    member_metadata.is_some_and(metadata::MetaFn::is_operator)
                        || member_extension_metadata.is_some_and(metadata::MetaFn::is_operator),
                );
                // Publish each FUNCTION-TYPE parameter's recovered shape into the logical `params` —
                // the descriptor erases `Scope.(Req) -> Resp` to a raw `Function2` (all-`Any`
                // parameters), so overload/lambda matching against `params` could never see the real
                // inner types the `Signature` attribute carries. A parameter `@Metadata` marks
                // SUSPEND_TYPE is further CPS-erased (`FunctionN+1<…, Continuation<T>, Object>`);
                // recover its logical `suspend` form — the flag is the only witness, since a
                // source-level `(…, Continuation<T>) -> Any` parameter has the identical `Signature`.
                // The gsig is rewritten in the same pass so overload candidates (which derive their
                // logical params from it) agree with `params`. Only function-typed parameters are
                // touched — every other parameter keeps its descriptor-derived erasure, so member
                // selection is unchanged for them.
                // The SUSPEND_TYPE flags are per-VALUE-parameter and align positionally only when
                // the physical list is exactly the value params (plus a suspend member's own
                // trailing Continuation) — the same alignment contract `mark_receiver_fun_params`
                // enforces. A misaligned member (contexts, extensions, failed alignment) has no
                // trustworthy per-slot facts.
                let aligned_metadata = member_metadata.filter(|metadata| {
                    metadata.value_params.len() + usize::from(member.suspend())
                        == member.params.len()
                });
                if let Some(gsig) = member
                    .generic_sig
                    .as_mut()
                    // A constructor's receiver-function marks are restored by the dedicated
                    // `<init>` pass below, which has its own unique-record attribution guard —
                    // publishing here would bypass it.
                    .filter(|g| g.params.len() == member.params.len() && m.name != "<init>")
                {
                    for (i, (param, shape)) in member
                        .params
                        .iter_mut()
                        .zip(gsig.params.iter_mut())
                        .enumerate()
                    {
                        // A GENERIC function type (`T.() -> String`) stays erased: its consumers
                        // substitute through the gsig at the call site, and a raw `TyParam`
                        // published here would short-circuit that substitution (a named lambda
                        // argument then binds `this` to the unsubstituted variable).
                        if !matches!(shape.non_null(), Ty::Fun(_)) || shape.mentions_ty_param() {
                            continue;
                        }
                        // Per-slot facts need both positional alignment and a decoded declared type.
                        // The metadata reader treats inline and table-backed types uniformly; if
                        // neither representation resolves, absence remains unknown rather than a
                        // false claim that the Continuation-tailed function is non-suspend.
                        let slot_facts = aligned_metadata
                            .and_then(|metadata| metadata.value_params.get(i))
                            .filter(|parameter| parameter.has_type_facts());
                        if slot_facts.is_some_and(metadata::MetaValueParam::suspend_fun) {
                            *shape = recover_suspend_fun_shape(*shape);
                        } else if slot_facts.is_none() && continuation_tailed_fun(*shape) {
                            // A `Continuation`-tailed shape without declared-type facts is ambiguous —
                            // it may be a suspend function type this pass cannot disclaim.
                            // Publishing it as a concrete non-suspend signature would be an
                            // authoritative-looking wrong answer; keep the erased leniency.
                            continue;
                        }
                        *param = *shape;
                    }
                }
                let value_arity = member
                    .params
                    .len()
                    .saturating_sub(usize::from(member.suspend()));
                if let Some(metadata) = semantic_metadata {
                    member.visibility = metadata.visibility;
                    member.set_ret_nullable(metadata.ret_nullable());
                }
                // A `suspend` member's descriptor erases its return to `Object` (the CPS convention). Recover
                // the LOGICAL return from `@Metadata` (`Int`, not `Object`) so a caller unboxes the suspension
                // result — keeping the erased type as `physical_ret` for the emitter.
                if member.suspend() {
                    if let Some(f) = semantic_metadata {
                        member.physical_ret = member.ret;
                        let logical = metadata_return_info(f.ret_class, f.ret_nullable())
                            .apply(member.physical_ret);
                        // krusty erases REFERENCE nullability in `Ty` (`String?` is modeled as `String`);
                        // `ret_nullable` tracks only PRIMITIVE nullability (a boxed suspension result). So a
                        // reference return keeps its plain non-null type; only a primitive carries the flag.
                        if logical.non_null().is_reference() {
                            member.ret = logical.non_null();
                        } else {
                            member.ret = logical;
                            member.set_ret_nullable(f.ret_nullable());
                        }
                    }
                    // The EMIT descriptor is the LOGICAL (continuation-stripped) form — the coroutine pass
                    // re-threads the CPS `Continuation` at the call. `params` stay RAW (they carry the
                    // continuation), which the member consumers that count physical params rely on; only the
                    // arg-matching `member_overloads` converter drops the trailing continuation value param.
                    member.descriptor = strip_continuation_param(&member.descriptor);
                }
                if is_map && member.name == "put" {
                    member.set_ret_nullable(true);
                }
                member.call_sig = member_metadata
                    .or(member_extension_metadata)
                    .map(metadata::MetaFn::member_call_sig)
                    .unwrap_or_else(|| {
                        let vararg_index = m
                            .is_vararg()
                            .then_some(value_arity)
                            .and_then(|arity| arity.checked_sub(1));
                        CallSig::metadata_member(value_arity, Vec::new(), Vec::new(), vararg_index)
                    });
                if let Some(java_nullable) = platform_nullable_params {
                    member.call_sig.platform_nullable_params = java_nullable;
                }
                if m.name == "<init>" {
                    // A constructor is NOT in `class_functions`, so the shared member alignment above
                    // left its receiver-function marks unrestored: `DslBase(init: Scope.() -> Unit)`
                    // read as a plain `(Scope) -> Unit` and no lambda argument bound `this`. Restore
                    // them from the class's `@Metadata` CONSTRUCTOR records, matched by source arity
                    // (the same evidence `ctor_named_params` uses for names/defaults).
                    //
                    // Arity is the ONLY alignment available here, so it must also be the test of
                    // whether the record belongs to THIS `<init>`. With two constructors of the same
                    // arity the record cannot be attributed, and stamping one's marks on both rewrites
                    // an ordinary `(Cfg) -> Unit` parameter into a receiver function type — which makes
                    // a valid call to the OTHER constructor unresolvable. Mark only when the arity
                    // identifies exactly one record; an ambiguous class keeps the erased reading.
                    let unique_record = {
                        let mut same_arity = ctor_param_lists
                            .iter()
                            .filter(|params| params.recv_fun.len() == member.params.len());
                        match (same_arity.next(), same_arity.next()) {
                            (Some(params), None) => Some(params),
                            _ => None,
                        }
                    };
                    if let (Some(gsig), Some(recv_fun)) = (
                        member.generic_sig.as_mut(),
                        unique_record
                            .map(|params| params.recv_fun.clone())
                            .filter(|marks| marks.iter().any(|&is_receiver| is_receiver)),
                    ) {
                        mark_receiver_fun_params(gsig, &recv_fun, false);
                        // Publish the recovered shape the two ways constructor resolution reads it:
                        // the parameter TYPE (overload/lambda matching walks `params`) and the call
                        // sig's per-parameter receiver flags. Only a receiver-function parameter is
                        // replaced — every other parameter keeps its descriptor-derived erasure, so
                        // constructor selection is unchanged for them.
                        let generic = member.generic_sig.as_ref().map(|g| g.params.clone());
                        if let Some(generic) = generic.filter(|g| g.len() == member.params.len()) {
                            for (param, shape) in member.params.iter_mut().zip(&generic) {
                                if matches!(shape.non_null(), Ty::Fun(sig) if sig.has_receiver) {
                                    *param = *shape;
                                }
                            }
                        }
                        member.call_sig.lambda_receiver_params = recv_fun;
                        member.call_sig.lambda_receivers = member
                            .params
                            .iter()
                            .map(|param| match param.non_null() {
                                Ty::Fun(sig) if sig.has_receiver => sig.params.first().copied(),
                                _ => None,
                            })
                            .collect();
                    }
                    // The ctor's generic signature (decoded above, marks restored) lets the resolver infer a
                    // construction's type arguments (`Pair(1, 2)` → `<Int, Int>`) without spelling backend
                    // signature strings. Re-parsing the raw attribute here would drop the receiver marks.
                    constructors.push(member);
                } else if m.is_static()
                    && owns_enum_entries_accessor
                    && m.name == "getEntries"
                    && member.params.is_empty()
                    && member.ret.obj_internal()
                        == Some(crate::types::type_name("kotlin/enums/EnumEntries"))
                {
                    enum_entries_accessor = Some(member);
                } else if m.is_static() {
                    // A Kotlin companion member compiles to a JVM static on the class.
                    companion.push(member);
                } else {
                    let source_name =
                        super::names::mapped_builtin_virtual_source_name(&ci.this_class(), &m.name);
                    if source_name != m.name {
                        let mut alias = member.clone();
                        alias.name = source_name.to_string();
                        alias.physical_name = Some(m.name.clone());
                        members.push(alias);
                    }
                    members.push(member);
                }
            }
            // A member whose JVM name is MANGLED by a value-class PARAMETER (`fun get(id: Vid): Cat` →
            // `get-<hash>(String)`): the descriptor-read loop above stored it under the mangled name, so a
            // source-name call `p.get(v)` misses it, and its erased `String` parameter wouldn't accept the
            // `Vid` argument. Recover the SOURCE name + logical (value-class) parameter types from `@Metadata`
            // and expose the member under the source name — keeping the mangled JVM name as `physical_name`
            // and the erased descriptor for emit (the value-classes pass unboxes the `Vid` argument).
            for mf in meta_fns {
                if !mf.is_public() || mf.is_extension() || mf.jvm_name == mf.kotlin_name {
                    continue;
                }
                let Some(desc) = mf.jvm_desc else {
                    continue;
                };
                let Some((params, physical_ret)) = parse_method_desc(desc) else {
                    continue;
                };
                // A `suspend` member appends a `Continuation` JVM parameter the SOURCE signature
                // (`value_params`) excludes — drop it (the CPS pass re-threads it) and match the leading
                // value parameters. Any OTHER count mismatch (`@Composable`, …) isn't a plain mangled member.
                let value_params = if mf.is_suspend() && !params.is_empty() {
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
                if mf.is_suspend() {
                    logical.push(Ty::obj("kotlin/coroutines/Continuation"));
                }
                // The bare `ret_class` recovery erases a parameterized return to its raw class
                // (`List<Ws>` → `List`, whose element is `Any`), so a member access on an element of a
                // value-class-param member's result (`repo.findByOrg(id).firstOrNull { it.name }`) fails to
                // resolve. The metadata generic signature carries the full return (`List<Ws>`); prefer it
                // when it has type arguments, applying return nullability, else keep the `ret_class` return.
                let ret = match mf.generic_sig.as_ref() {
                    Some(g) if !g.ret.type_args().is_empty() => {
                        if mf.ret_nullable() {
                            Ty::nullable(g.ret)
                        } else {
                            g.ret
                        }
                    }
                    _ => metadata_return_info(mf.ret_class, mf.ret_nullable()).apply(physical_ret),
                };
                let mut member =
                    LibraryMember::new(mf.kotlin_name.clone(), logical, ret, desc.to_string());
                member.visibility = mf.visibility;
                member.physical_name = Some(mf.jvm_name.clone());
                member.physical_ret = physical_ret;
                member.set_ret_nullable(mf.ret_nullable());
                member.set_suspend(mf.is_suspend());
                member.call_sig = mf.member_call_sig();
                // The DECLARED return classifier, verbatim and un-substituted — recorded with no
                // value-class probing (which is unsafe on this path: it runs inside `resolve_type_name`'s
                // type build and would recurse on cyclic class graphs). A NULLABLE declared return is
                // deliberately excluded: a nullable value class really is BOXED, so it must keep the
                // ordinary boxed handling. The value-class pass decides what the classifier means.
                // `suspend` excluded for the reason given at the descriptor-loop site above: CPS
                // erases the descriptor return to `Object`, which stops witnessing the carrier.
                member.declared_ret = metadata_declared_nonnull_nonsuspend_return(mf);
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
            // A MAPPED Kotlin COLLECTION (`kotlin/collections/MutableList`, …) or `kotlin/String` takes
            // BOTH its members and its supertypes from the `.kotlin_builtins` declaration — that
            // declaration IS its Kotlin API, and the JVM class it maps to is only its physical
            // realization. Carrying the JVM class's own members or interfaces instead would put
            // `java.util.List`'s surplus method set (`remove(int)`, `stream`, `toArray`, `getFirst` —
            // none of them Kotlin members) into the Kotlin scope, directly or one rung up through
            // `java/util/List` as a supertype. That is what let `list.remove(10)` bind remove-BY-INDEX,
            // and what let `"abcdef".split("c")` bind `java.lang.String.split`, which splits on a REGEX
            // and returns an array. The builtins decode to the same erased descriptors and the same JVM
            // owner, so nothing physical changes: names stay in source terms and the Kotlin → JVM rename
            // happens at emit (`names::mapped_builtin_virtual_name`), as for every other mapped member.
            //
            // NOT the remaining mapped builtins, and not for want of anything here: kotlinc does not
            // hide every Java method on a mapped type either. `JvmBuiltInsCustomizer` re-admits an
            // explicit whitelist (`JvmBuiltInsSignatures.VISIBLE_METHOD_SIGNATURES`) over the builtins
            // scope, and krusty has no equivalent — so widening to `kotlin/CharSequence` would wrongly
            // drop `chars`/`codePoints`, to `kotlin/Enum` `name`/`ordinal`, and to `kotlin/Throwable`
            // `getStackTrace`/`initCause`/`fillInStackTrace`/… , every one of which kotlinc keeps.
            // Leaving `kotlin/CharSequence` joined is also why whatever `java.lang.CharSequence` declares
            // still reaches `String` one rung up — `charAt` on every JDK, plus `getChars` as of JDK 25,
            // which added it as a `default` method. That is the price of keeping `chars`/`codePoints`,
            // and it makes the residual leak JDK-DEPENDENT. See docs/SPEC.md.
            let rendered_internal = internal_name.render();
            let is_mapped_builtin =
                super::jvm_class_map::to_jvm_internal(&rendered_internal) != rendered_internal;
            let builtin_supertypes = self.cp.builtin_supertypes_name(internal_name);
            let mut builtin_members = if is_mapped_builtin {
                self.builtin_members_for_type_name(internal_name)
            } else {
                Vec::new()
            };
            // Members and supertypes must switch provenance together. Presence of the decoded
            // declaration, not a non-empty member vector, is the capability: an authoritative
            // declaration is allowed to state an empty member or supertype set, and falling back to
            // only half of the Java shape would recreate the very scope leak this branch prevents.
            // That same presence test is what keeps a classpath carrying a JDK but NO kotlin-stdlib
            // correct: nothing decodes there, so `kotlin/String` keeps the JVM class's supertypes
            // rather than being left with none (it would otherwise lose `CharSequence`, `Comparable`
            // and `Any`, and every subtype test against them would start failing).
            let kotlin_scope_is_authoritative = is_mapped_builtin
                && self.cp.builtin_is_interface(&rendered_internal).is_some()
                // Scope provenance belongs to the central Kotlin↔JVM mapping. Keeping the policy
                // there avoids a classpath-origin branch (and a second collection/String name list)
                // in this loader; this site only combines that semantic policy with the runtime
                // capability that the corresponding builtins declaration was actually decoded.
                && super::jvm_class_map::mapped_builtin_has_authoritative_kotlin_scope(internal_name);
            let mut supertypes = TypeNameList::new();
            if kotlin_scope_is_authoritative {
                // `java/io/Serializable` survives the replacement. It is not a Kotlin type and so is
                // absent from every `.kotlin_builtins` declaration, but kotlinc still reports a mapped
                // builtin as implementing it whenever the Java class does — `JvmBuiltInsCustomizer`
                // adds it back in `getSupertypes` (`isSerializableInJava`). Dropping it made
                // `val v: java.io.Serializable = "abc"` an error against a kotlinc that accepts it.
                // The mapped COLLECTIONS never noticed: `java/util/List` does not implement it, and a
                // concrete `java.util` class that does (`ArrayList`) is not an authoritative name.
                for s in ci.interfaces.iter_ids() {
                    if s.matches("java/io/Serializable") {
                        supertypes.push_name(s);
                    }
                }
            } else {
                for s in ci.interfaces.iter_ids() {
                    supertypes.push_name(s);
                }
                if let Some(s) = ci.super_class {
                    supertypes.push_name(s);
                }
            }
            for s in builtin_supertypes.iter_ids() {
                if !supertypes.contains_name(s) {
                    supertypes.push_name(s);
                }
            }
            // A JVM collection type (`java/util/Set`) and its JVM supertypes ARE their Kotlin mapped types
            // (`kotlin/collections/Set`) at the source level — an extension declared on
            // `kotlin/collections/Iterable` applies to a `java/util/Set` receiver (`entries.first()`). Add
            // each Kotlin equivalent as a supertype so the source-type receiver walk bridges the
            // java.util ↔ kotlin.collections platform mapping instead of dead-ending in the JDK hierarchy.
            // The read-only Kotlin face of every JVM collection interface in the hierarchy — an extension on
            // `kotlin/collections/Iterable`/`List`/… applies to a `java/util/*` receiver.
            let mut mapped: Vec<TypeName> = std::iter::once(internal_name)
                .chain(supertypes.iter_ids())
                .filter_map(super::jvm_class_map::jvm_collection_to_kotlin_type_name)
                .collect();
            // The MUTABLE face — but ONLY for a CONCRETE class (`java/util/ArrayList`, `HashMap`), never for
            // the JVM collection INTERFACES: `java/util/List` is the shared realization of BOTH the read-only
            // `kotlin/collections/List` and its mutable sibling, so tagging the interface mutable would make a
            // read-only `List` (which reaches `java/util/List` in its supertypes) spuriously satisfy a
            // `MutableCollection.plusAssign`. A concrete class is genuinely mutable, so its `MutableList`/
            // `MutableSet`/`MutableMap` face is sound (derived from the java.util interface it implements).
            if !ci.is_interface() {
                for m in std::iter::once(internal_name)
                    .chain(supertypes.iter_ids())
                    .filter_map(super::jvm_class_map::jvm_collection_to_kotlin_mutable_type_name)
                {
                    if !mapped.contains(&m) {
                        mapped.push(m);
                    }
                }
            }
            for k in mapped {
                if !supertypes.contains_name(k) {
                    supertypes.push_name(k);
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
            } else if ci.access & crate::jvm::classreader::ACC_ENUM != 0 {
                crate::libraries::TypeKind::Enum
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
                        .and_then(|m| crate::jvm::names::parse_method_descriptor(&m.descriptor))
                        .and_then(|(params, _)| params.first().copied())
                        .map(field_desc_to_ty)
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
                self.value_class_metadata_members_for_class(&ci, inline.is_some(), meta_fns);
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
            // A MAPPED Kotlin builtin (`kotlin/collections/MutableList`, `kotlin/CharSequence`, …) has
            // no `.class` of its own; the members read above came from the JVM class it maps to
            // (`java/util/List`). That class's method set is NOT its Kotlin API: `java.util.List`
            // declares `remove(int)`, `stream`, `toArray`, `getFirst`, none of which Kotlin's
            // `MutableList` has, and its `remove(int)` is reachable from Kotlin only under the renamed
            // name `removeAt`. The `.kotlin_builtins` declaration IS the Kotlin API, and it decodes to
            // the same erased descriptors and JVM owner — so for a mapped name it REPLACES the JVM
            // class's members rather than being unioned with them. The class file still supplies the
            // kind and constructors. Names stay in source terms; the Kotlin → JVM
            // rename happens at emit (`names::mapped_builtin_virtual_name`), as it does for every
            // other mapped member.
            // The members half of the same decision (see the supertype block below): for a mapped
            // collection the builtins REPLACE the JVM class's members; every other mapped builtin still
            // joins them, with anything the class file already states under a physical name dropped.
            if kotlin_scope_is_authoritative {
                members.clear();
            } else {
                builtin_members.retain(|builtin| {
                    !members.iter().any(|member| {
                        member.physical_name.is_some()
                            && member.name == builtin.name
                            && member.descriptor == builtin.descriptor
                    })
                });
            }
            // A defaulted value-class primary constructor surfaces as the `constructor-impl$default` synthetic.
            let value_ctor_has_default = ci
                .methods
                .iter()
                .any(|m| m.name == "constructor-impl$default");
            // Retain every declaration, not only readable instance fields. A private or static field
            // still hides an inherited field of the same name, and that hiding decision belongs to the
            // shared source-level hierarchy walk rather than to this classfile provider.
            let fields = ci
                .fields
                .iter()
                .map(|field| {
                    let erased_ty = field_desc_to_ty(&field.descriptor);
                    let ty = field
                        .signature
                        .as_deref()
                        .and_then(|signature| {
                            parse_field_gsig(signature, &field.descriptor, ci.signature.as_deref())
                                .map(|(ty, _)| ty)
                        })
                        .unwrap_or(erased_ty);
                    LibraryField {
                        name: field.name.clone(),
                        ty,
                        erased_ty,
                        descriptor: field.descriptor.clone(),
                        visibility: if field.access & 0x0001 != 0 {
                            Visibility::Public
                        } else if field.access & 0x0004 != 0 {
                            Visibility::Protected
                        } else {
                            Visibility::Private
                        },
                        is_static: field.access & ACC_STATIC != 0,
                    }
                })
                .collect();
            Some(LibraryType {
                is_public: ci.is_public(),
                kind,
                supertypes,
                constructors,
                fields,
                members: members
                    .into_iter()
                    .chain(value_class_metadata_members)
                    .chain(builtin_members)
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
                enum_entries_accessor,
                value_ctor_has_default,
                ctor_named_params: metadata::class_constructor_params(&ci),
                value_class_properties: self.value_class_property_members_for_class(&ci),
                retention: ci.retention.clone(),
            })
        }
    }
}

/// Parse one JVM generic-signature type off the front of `s` into a signature [`Ty`], returning
/// `(node, rest)`. A type variable becomes a [`Ty::TyParam`] (`kotlin/Any` bound).
fn parse_gsig(s: &str) -> Option<(Ty, &str)> {
    let parsed = parse_gsig_inner(s, false)?;
    Some((parsed.ty, parsed.rest))
}

struct ParsedGsig<'a> {
    ty: Ty,
    erasure: Option<String>,
    has_free: bool,
    field_inexact: bool,
    rest: &'a str,
}

fn parse_gsig_inner(s: &str, for_field: bool) -> Option<ParsedGsig<'_>> {
    let b = s.as_bytes();
    match *b.first()? {
        b'T' => {
            let end = s.find(';')?;
            (end > 1).then(|| ParsedGsig {
                ty: Ty::ty_param(&s[1..end], Ty::obj("kotlin/Any")),
                erasure: for_field.then(|| "Ljava/lang/Object;".to_string()),
                has_free: true,
                field_inexact: false,
                rest: &s[end + 1..],
            })
        }
        b'[' => {
            if s[1..].trim_start_matches('[').starts_with('V') {
                return None;
            }
            let parsed = parse_gsig_inner(&s[1..], for_field)?;
            Some(ParsedGsig {
                ty: Ty::array(parsed.ty),
                erasure: parsed.erasure.map(|erased| format!("[{erased}")),
                has_free: parsed.has_free,
                field_inexact: parsed.field_inexact,
                rest: parsed.rest,
            })
        }
        b'L' => {
            let mut rest = &s[1..];
            let end = rest.find(['<', '.', ';'])?;
            let first_component = &rest[..end];
            if !valid_gsig_class_component(first_component, true) {
                return None;
            }
            rest = &rest[end..];

            let (mut args, mut has_free, mut field_inexact, tail) =
                parse_gsig_type_args(rest, for_field)?;
            rest = tail;
            let mut binary_name = None;
            while let Some(suffix) = rest.strip_prefix('.') {
                let end = suffix.find(['<', '.', ';'])?;
                let component = &suffix[..end];
                if !valid_gsig_class_component(component, false) {
                    return None;
                }
                field_inexact |= !args.is_empty();
                let binary_name = binary_name.get_or_insert_with(|| first_component.to_string());
                binary_name.push('$');
                binary_name.push_str(component);
                rest = &suffix[end..];
                let (suffix_args, suffix_has_free, suffix_inexact, tail) =
                    parse_gsig_type_args(rest, for_field)?;
                has_free |= suffix_has_free;
                field_inexact |= suffix_inexact;
                args = suffix_args;
                rest = tail;
            }
            let after = rest.strip_prefix(';')?;
            let binary_name = binary_name.as_deref().unwrap_or(first_component);
            let internal = to_kotlin_internal(binary_name);
            let node = if let Some(arity) = jvm_function_arity(internal) {
                if arity.checked_add(1) == Some(args.len()) {
                    let ret = gsig_unbox_wrapper(args.pop()?);
                    let params = args.into_iter().map(gsig_unbox_wrapper).collect();
                    Ty::fun(params, ret)
                } else {
                    field_inexact = true;
                    Ty::obj_args(internal, &args)
                }
            } else if internal.starts_with(JVM_FUNCTION_PREFIX) {
                field_inexact = true;
                Ty::obj_args(internal, &args)
            } else if args.is_empty() {
                kotlin_name_to_ty(internal)
            } else {
                Ty::obj_args(internal, &args)
            };
            Some(ParsedGsig {
                ty: node,
                erasure: for_field.then(|| format!("L{binary_name};")),
                has_free,
                field_inexact,
                rest: after,
            })
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
            Some(ParsedGsig {
                ty: t,
                erasure: for_field.then(|| (c as char).to_string()),
                has_free: false,
                field_inexact: false,
                rest: &s[1..],
            })
        }
    }
}

fn valid_gsig_class_component(component: &str, allow_package: bool) -> bool {
    if component.is_empty() || component.contains(['[', '>', ':']) {
        return false;
    }
    if allow_package {
        !component.starts_with('/') && !component.ends_with('/') && !component.contains("//")
    } else {
        !component.contains('/')
    }
}

fn parse_gsig_type_args(s: &str, for_field: bool) -> Option<(Vec<Ty>, bool, bool, &str)> {
    let Some(mut rest) = s.strip_prefix('<') else {
        return Some((Vec::new(), false, false, s));
    };
    if rest.starts_with('>') {
        return None;
    }
    let mut args = Vec::new();
    let mut has_free = false;
    let mut field_inexact = false;
    while !rest.starts_with('>') {
        if let Some(tail) = rest.strip_prefix('*') {
            args.push(Ty::obj("kotlin/Any"));
            field_inexact = true;
            rest = tail;
            continue;
        }
        let (arg, projected) = rest
            .strip_prefix('+')
            .or_else(|| rest.strip_prefix('-'))
            .map_or((rest, false), |arg| (arg, true));
        if !matches!(arg.as_bytes().first(), Some(b'L' | b'T' | b'[')) {
            return None;
        }
        let parsed = parse_gsig_inner(arg, for_field)?;
        has_free |= parsed.has_free;
        field_inexact |= projected || parsed.field_inexact;
        args.push(parsed.ty);
        rest = parsed.rest;
    }
    Some((args, has_free, field_inexact, rest.strip_prefix('>')?))
}

const JVM_FUNCTION_PREFIX: &str = "kotlin/jvm/functions/Function";

fn jvm_function_arity(internal: &str) -> Option<usize> {
    let suffix = internal.strip_prefix(JVM_FUNCTION_PREFIX)?;
    (!suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| suffix.parse::<usize>().ok())
        .flatten()
}

fn gsig_unbox_wrapper(g: Ty) -> Ty {
    let Ty::Obj(internal, args) = g else {
        return g;
    };
    super::jvm_class_map::wrapper_to_kotlin_prim_name(internal)
        .map(super::classpath::kotlin_name_to_ty)
        .unwrap_or_else(|| {
            if args.is_empty() {
                super::classpath::kotlin_type_name_to_ty(internal)
            } else {
                g
            }
        })
}

/// Parse a leading `<Name:Bound...>` formal-type-parameter block, returning the formal names and the
/// remaining signature. No block means empty names and unchanged input.
/// The generic type-parameter NAMES of a method `Signature` (`<T:…;U:…>(…)…` → `["T", "U"]`), for
/// mapping a call's explicit type arguments onto reified type parameters at an inline splice.
pub(crate) fn signature_formals(sig: &str) -> Vec<String> {
    parse_formals(sig).0
}

fn parse_formals(s: &str) -> (Vec<String>, Vec<Vec<Ty>>, &str) {
    let Some(rest) = s.strip_prefix('<') else {
        return (Vec::new(), Vec::new(), s);
    };
    let original = s;
    let mut rest = rest;
    let mut formals = Vec::new();
    let mut formal_bounds = Vec::new();
    while !rest.starts_with('>') {
        let Some(colon) = rest.find(':') else {
            return (Vec::new(), Vec::new(), original);
        };
        if colon == 0 {
            return (Vec::new(), Vec::new(), original);
        }
        formals.push(rest[..colon].to_string());
        rest = &rest[colon + 1..];
        let mut bounds = Vec::new();
        // A missing class bound is followed by one or more interface bounds.
        if !rest.starts_with(':') {
            let Some((bound, tail)) = parse_gsig(rest) else {
                return (Vec::new(), Vec::new(), original);
            };
            bounds.push(bound);
            rest = tail;
        }
        while let Some(tail) = rest.strip_prefix(':') {
            let Some((bound, after)) = parse_gsig(tail) else {
                return (Vec::new(), Vec::new(), original);
            };
            bounds.push(bound);
            rest = after;
        }
        formal_bounds.push(bounds);
    }
    (formals, formal_bounds, &rest[1..])
}

/// Parse a JVM method generic signature `<formals>(params)ret`.
fn parse_method_gsig(sig: &str) -> Option<GenericSig> {
    let (formals, formal_bounds, s) = parse_formals(sig);
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
    // A raw signature has no source nullability annotation. When its OUTER return is one of the
    // method's own formals, specializing that formal to a source primitive must retain the declared
    // reference contract. Record that semantic behavior on the signature itself; downstream member,
    // static, and top-level resolution then share one policy without learning why the provider chose it.
    let return_policy = ret
        .ty_param_name()
        .filter(|name| formals.iter().any(|formal| formal == name))
        .map_or(GenericReturnPolicy::Exact, |_| {
            GenericReturnPolicy::FlexibleReference
        });
    // The JVM `Signature` attribute is the fallback for NON-metadata callables (Java methods): an instance
    // method has no receiver parameter and a static none either, so the receiver is not modeled here.
    Some(GenericSig {
        formals,
        formal_bounds,
        receiver: None,
        params,
        ret,
        return_policy,
    })
}

fn has_free_ty_params(ty: Ty) -> bool {
    match ty {
        Ty::TyParam(..) => true,
        Ty::Fun(sig) => {
            sig.params.iter().any(|param| has_free_ty_params(*param)) || has_free_ty_params(sig.ret)
        }
        Ty::Obj(_, args) => args.iter().any(|arg| has_free_ty_params(*arg)),
        Ty::Nullable(inner) => has_free_ty_params(*inner),
        _ => false,
    }
}

fn parse_concrete_field_gsig(signature: &str, erased_descriptor: &str) -> Option<Ty> {
    let (ty, has_free) = parse_field_gsig(signature, erased_descriptor, None)?;
    (!has_free).then_some(ty)
}

/// Decode the source type of a field while verifying that it erases to the classfile descriptor.
/// Unlike the constant-field helper, this retains type variables: the shared resolver substitutes
/// them from the applied receiver and falls back to `erased_ty` only for a raw receiver.
fn parse_field_gsig(
    signature: &str,
    erased_descriptor: &str,
    declaring_class_signature: Option<&str>,
) -> Option<(Ty, bool)> {
    if !matches!(signature.as_bytes().first(), Some(b'L' | b'T' | b'[')) {
        return None;
    }
    let parsed = parse_gsig_inner(signature, true)?;
    let erasure = field_type_parameter_erasure(signature, declaring_class_signature)
        .or(parsed.erasure.as_deref().map(str::to_string));
    if !parsed.rest.is_empty()
        || erasure.as_deref() != Some(erased_descriptor)
        || parsed.field_inexact
    {
        return None;
    }
    Some((canonicalize_jvm_collections(parsed.ty), parsed.has_free))
}

/// Compute the descriptor erasure of an outermost declaring-class type variable (or an array of it).
/// The JVM uses the variable's leftmost bound, so `T : CharSequence` erases to `CharSequence`, not
/// unconditionally to `Object`. Chained bounds are followed defensively and cycles are rejected.
fn field_type_parameter_erasure(
    signature: &str,
    declaring_class_signature: Option<&str>,
) -> Option<String> {
    let mut element = signature;
    let mut dimensions = 0;
    while let Some(rest) = element.strip_prefix('[') {
        dimensions += 1;
        element = rest;
    }
    let name = element.strip_prefix('T')?.strip_suffix(';')?;
    let (formals, bounds, _) = parse_formals(declaring_class_signature?);
    let by_name: std::collections::HashMap<&str, Ty> = formals
        .iter()
        .zip(&bounds)
        .filter_map(|(formal, bounds)| {
            bounds
                .first()
                .copied()
                .map(|bound| (formal.as_str(), bound))
        })
        .collect();
    let mut bound = *by_name.get(name)?;
    let mut seen = std::collections::HashSet::new();
    while let Ty::TyParam(next, _) = bound {
        if !seen.insert(next) {
            return None;
        }
        bound = *by_name.get(next)?;
    }
    Some(format!(
        "{}{}",
        "[".repeat(dimensions),
        type_descriptor(bound)
    ))
}

fn concrete_generic_ret(gsig: &GenericSig) -> Option<Ty> {
    match gsig.ret {
        // A FUNCTION-typed return (`getHandler(): Function2<Scope, Req, Resp>` — a fun-typed
        // property's accessor) already parsed to the shaped `Ty::Fun`; the erased descriptor
        // return is the all-`Any` `FunctionN`, so failing to recover here strips a lambda
        // assigned to the property of its parameter types and receiver. A raw-`Signature` fun
        // type spells collections/boxed primitives in Java form (`List<Integer>`) — canonicalize
        // exactly as the parameterized-`Obj` arm below does (the helper recurses into `Fun` and
        // `Nullable`, preserving the receiver/suspend shape); a metadata-decoded ret is already
        // Kotlin-spelled, so this is a no-op for it.
        ty if matches!(ty.non_null(), Ty::Fun(_)) && !has_free_ty_params(ty) => {
            Some(canonicalize_jvm_collections(ty))
        }
        Ty::Obj(_, args) if !args.is_empty() && !has_free_ty_params(gsig.ret) => {
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

/// Overlay the `@Metadata`-declared collection classifiers onto a JVM-signature-derived type, level
/// by level. The signature erases read-only vs mutable (`List`/`MutableList` both spell
/// `java/util/List`) at EVERY nesting depth; the metadata type preserves it. At each level the
/// metadata classifier replaces the signature's name ONLY when the shared builtin-erasure table says
/// it is a Kotlin collection sibling mapping to the same JVM internal — guaranteeing the same
/// collection family and arity —
/// and the walk descends into type arguments only when the two classifiers agree (sibling or
/// identical) with matching arity, so a divergent classifier (stale metadata) never forms an
/// arity-mismatched or misaligned type. The base keeps its structure, primitives, and nullability;
/// only names are taken from metadata.
fn overlay_metadata_collection_names(base: Ty, meta: Ty) -> Ty {
    // Nullability lives on the base (the resolution pipeline applies it separately); look through a
    // metadata `T?` to its classifier.
    let meta = meta.non_null();
    let (Ty::Obj(base_name, base_args), Ty::Obj(meta_name, meta_args)) = (base, meta) else {
        return base;
    };
    let sibling = super::jvm_class_map::is_kotlin_collection_type_name(meta_name)
        && super::jvm_class_map::type_names_map_to_same_jvm_internal(meta_name, base_name);
    if !sibling && base_name != meta_name {
        return base;
    }
    let name = if sibling { meta_name } else { base_name };
    if !base_args.is_empty() && base_args.len() == meta_args.len() {
        let merged: Vec<Ty> = base_args
            .iter()
            .zip(meta_args.iter())
            .map(|(&base_arg, &meta_arg)| overlay_metadata_collection_names(base_arg, meta_arg))
            .collect();
        Ty::obj_args_name(name, &merged)
    } else {
        // No arguments to align (a raw erased base) or an arity mismatch: the outer classifier is
        // still sound to take, the arguments stay the base's own.
        Ty::obj_args_name(name, base_args)
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
        Ty::Fun(sig) => {
            let params = sig
                .params
                .iter()
                .map(|param| canonicalize_jvm_collections(*param))
                .collect();
            let ret = canonicalize_jvm_collections(sig.ret);
            Ty::fun_with_shape(
                params,
                ret,
                sig.context_count,
                sig.has_receiver,
                sig.suspend,
            )
        }
        Ty::Nullable(inner) => Ty::nullable(canonicalize_jvm_collections(*inner)),
        other => other,
    }
}

/// Whether a decoded function type ends in a `Continuation` parameter — the shape a CPS-erased
/// suspend function type shares with a source-level continuation-taking one.
fn continuation_tailed_fun(ty: Ty) -> bool {
    match ty.non_null() {
        Ty::Fun(sig) => matches!(
            sig.params.last(),
            Some(Ty::Obj(cont, _)) if cont.matches("kotlin/coroutines/Continuation")
        ),
        _ => false,
    }
}

/// The LOGICAL Kotlin function type behind a CPS-erased one: the JVM `Signature` attribute spells
/// `suspend R.(P) -> T` as `FunctionN+1<R, P, Continuation<T>, Object>`, so a decoded `Fun` whose
/// LAST parameter is `Continuation<T>` IS a suspend function type — drop the continuation, take its
/// `T` as the return, and set the `suspend` flag. Anything else is returned unchanged.
fn recover_suspend_fun_shape(ty: Ty) -> Ty {
    match ty {
        Ty::Fun(sig) if !sig.suspend => {
            let Some(Ty::Obj(cont, cont_args)) = sig.params.last() else {
                return ty;
            };
            if !cont.matches("kotlin/coroutines/Continuation") {
                return ty;
            }
            let ret = cont_args
                .first()
                .copied()
                .unwrap_or_else(|| Ty::obj("kotlin/Any"));
            Ty::fun_with_shape(
                sig.params[..sig.params.len() - 1].to_vec(),
                ret,
                sig.context_count,
                sig.has_receiver,
                true,
            )
        }
        Ty::Nullable(inner) => Ty::nullable(recover_suspend_fun_shape(*inner)),
        _ => ty,
    }
}

/// Restore the RECEIVER function-type marks a JVM `Signature` attribute cannot carry: for each value
/// parameter `@Metadata` marks `@kotlin.ExtensionFunctionType`, turn the decoded `(R, …) -> T` into
/// `R.(…) -> T`. A `suspend` callable's physical signature appends a `Continuation` the source parameter
/// list does not have, so it is dropped before aligning. Applied positionally, and skipped unless the two
/// lists then align 1:1 — a mismatch means they describe different parameters.
fn mark_receiver_fun_params(gsig: &mut GenericSig, recv_fun: &[bool], suspend: bool) {
    let source_params = gsig.params.len().saturating_sub(usize::from(suspend));
    if source_params != recv_fun.len() {
        return;
    }
    for (param, &receiver_fun) in gsig.params.iter_mut().take(source_params).zip(recv_fun) {
        let Ty::Fun(sig) = *param else { continue };
        if !receiver_fun || sig.has_receiver || sig.params.is_empty() {
            continue;
        }
        *param = Ty::fun_with_shape(
            sig.params.clone(),
            sig.ret,
            sig.context_count,
            true,
            sig.suspend,
        );
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
        s if s.starts_with('[') => Ty::array(field_desc_to_ty(&s[1..])),
        s if s.starts_with('L') && s.ends_with(';') => {
            let raw_internal = &s[1..s.len() - 1];
            if raw_internal == "java/lang/Void" {
                return Ty::Unit;
            }
            let internal = to_kotlin_internal(raw_internal);
            if let Some(n) = jvm_function_arity(internal) {
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
        fields: Vec::new(),
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
        enum_entries_accessor: None,
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
    supertypes: TypeNameList,
    members: Vec<LibraryMember>,
    type_params: Vec<String>,
) -> LibraryType {
    LibraryType {
        is_public: true,
        kind: if is_interface {
            crate::libraries::TypeKind::Interface
        } else {
            crate::libraries::TypeKind::Class
        },
        supertypes,
        constructors: Vec::new(),
        fields: Vec::new(),
        members,
        companion: Vec::new(),
        companion_consts: Default::default(),
        sam_method: None,
        companion_object: None,
        value_companion_fns: Vec::new(),
        value_underlying: None,
        alias_target: None,
        type_params,
        sealed_subclasses: TypeNameList::new(),
        enum_entries: Vec::new(),
        enum_entries_accessor: None,
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
    let (formals, _, mut s) = parse_formals(sig);
    let mut supers = Vec::new();
    while !s.is_empty() {
        let (g, rest) = parse_gsig(s)?;
        supers.push(g);
        s = rest;
    }
    Some((formals, supers))
}

/// The field descriptor of the CPS `Continuation` parameter kotlinc appends to a `suspend` method.
const CONTINUATION_PARAM_DESCRIPTOR: &str = "Lkotlin/coroutines/Continuation;";

/// Read a JVM method descriptor once into the complete call-boundary layout common lowering needs.
///
/// Exactly one position may be the synthetic CPS continuation. A descriptor spelling it more than
/// once is not a shape kotlinc emits, and guessing between them would align a caller's arguments to
/// the wrong slots. Its representation facts are still valid, so retain those while reporting no
/// continuation position and let the caller stay conservative if its value count does not align.
fn method_layout(descriptor: &str) -> Option<crate::runtime::PlatformMethodLayout> {
    let (params, ret) = crate::jvm::names::parse_method_descriptor(descriptor)?;
    let mut found = params
        .iter()
        .enumerate()
        .filter(|&(_, &p)| p == CONTINUATION_PARAM_DESCRIPTOR);
    let continuation_slot = found
        .next()
        .and_then(|(index, _)| found.next().is_none().then_some(index));
    Some(crate::runtime::PlatformMethodLayout {
        // A JVM parameter is a reference exactly when its field descriptor is an object (`L…;`) or
        // an array (`[…`); everything else is a primitive carrier (`I`, `J`, `Z`, …).
        reference_slots: params
            .iter()
            .map(|p| p.starts_with('L') || p.starts_with('['))
            .collect(),
        continuation_slot,
        // Only an object return names a class. A primitive carrier (`I`, `J`, ...), array (`[...`),
        // or `V` makes no reference-class claim, which keeps a carrier-returning call from reading as
        // an already boxed value.
        return_class: ret
            .strip_prefix('L')
            .and_then(|ret| ret.strip_suffix(';'))
            .map(type_name),
    })
}

/// Parse a method descriptor `(p…)ret` into parameter `Ty`s and the return `Ty`.
/// The LOGICAL descriptor of a `suspend fun`'s physical CPS method: drop the trailing
/// `kotlin/coroutines/Continuation` parameter kotlinc appends (`(ILkotlin/coroutines/Continuation;)…`
/// → `(I)…`). The return stays erased (`Object`); the *logical* Kotlin return lives in `@Metadata`. A
/// suspend callee is resolved by this logical signature; the coroutine pass re-derives the CPS form for
/// the emitted call. A no-op if the descriptor has no trailing continuation (not a CPS method).
fn strip_continuation_param(desc: &str) -> String {
    if let Some(close) = desc.rfind(')') {
        if let Some(stripped) = desc[1..close].strip_suffix(CONTINUATION_PARAM_DESCRIPTOR) {
            return format!("({}){}", stripped, &desc[close + 1..]);
        }
    }
    desc.to_string()
}

pub(crate) fn parse_method_desc(desc: &str) -> Option<(Vec<Ty>, Ty)> {
    let (params, ret) = crate::jvm::names::parse_method_descriptor(desc)?;
    Some((
        params.into_iter().map(desc_to_ty).collect(),
        desc_to_ty(ret),
    ))
}

fn parse_method_desc_with_field_params(desc: &str) -> Option<(Vec<Ty>, Ty)> {
    let (params, ret) = crate::jvm::names::parse_method_descriptor(desc)?;
    Some((
        params.into_iter().map(field_desc_to_ty).collect(),
        desc_to_ty(ret),
    ))
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
    /// The classpath's package catalog answers directly: it already records every package a jar, class
    /// directory, or the JDK jimage declares, plus their ancestors.
    fn package_exists(&self, package: TypeName) -> bool {
        self.cp.has_package(&package.render())
    }

    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
        let Some(internal) = ty.obj_internal() else {
            return Vec::new();
        };
        let jvm_internal = crate::jvm::jvm_class_map::to_jvm_type_name(internal);
        let mut applied = self
            .cp
            .find_name(jvm_internal)
            .and_then(|class| {
                let (formals, supers) = parse_class_gsig(class.signature.as_deref()?)?;
                let bindings = formals
                    .into_iter()
                    .zip(
                        ty.type_args()
                            .iter()
                            .copied()
                            .chain(std::iter::repeat_with(|| Ty::obj("kotlin/Any"))),
                    )
                    .collect::<std::collections::HashMap<_, _>>();
                Some(
                    supers
                        .into_iter()
                        .map(|supertype| crate::symbol_resolver::ty_subst(supertype, &bindings))
                        .collect::<Vec<_>>(),
                )
            })
            .unwrap_or_else(|| {
                self.cp
                    .find_name(jvm_internal)
                    .map(|class| {
                        class
                            .interfaces
                            .iter_ids()
                            .chain(class.super_class)
                            .map(Ty::obj_name)
                            .collect()
                    })
                    .unwrap_or_default()
            });

        if let Some(shape) = self.resolve_type_name(internal) {
            for mapped in shape.supertypes.iter_ids() {
                let args = applied
                    .iter()
                    .find(|supertype| {
                        supertype.obj_internal().is_some_and(|name| {
                            crate::jvm::jvm_class_map::type_names_map_to_same_jvm_internal(
                                name, mapped,
                            )
                        })
                    })
                    .map(|supertype| supertype.type_args().to_vec())
                    .unwrap_or_default();
                let mapped = Ty::obj_args_name(mapped, &args);
                if !applied.contains(&mapped) {
                    applied.push(mapped);
                }
            }
        }
        applied
    }

    fn property_members(&self, recv: Ty, name: &str) -> PropertySet {
        // Member properties of the receiver's type + its supertypes, most-derived first (rung 0). Each
        // carries the REAL getter/setter from `@Metadata`'s `JvmPropertySignature`, so the caller emits the
        // accessor by name rather than guessing `getX`. Extension properties are surfaced by `resolve_symbols`.
        let Some(internal) = recv.kotlin_class_internal() else {
            return PropertySet::default();
        };
        let mut overloads = Vec::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(internal);
        let mut seen = std::collections::HashSet::new();
        let mut rung = 0u32;
        while let Some(cn) = queue.pop_front() {
            if !seen.insert(cn) {
                continue;
            }
            if let Some(ci) = self.cp.find_name(cn) {
                for mp in metadata::class_properties(&ci) {
                    if mp.name != name {
                        continue;
                    }
                    // Need the real accessor to emit anything; skip a property whose metadata omits it.
                    let Some(getter) = mp.getter.clone() else {
                        continue;
                    };
                    let ret_ty = mp
                        .ret_class
                        .map_or(Ty::obj("kotlin/Any"), kotlin_type_name_to_ty);
                    // `Ty::obj_name` is deliberate for a value-class-typed property (the logical class,
                    // not its collapsed underlying). It is wrong only for a Kotlin PRIMITIVE, which it
                    // keeps as the boxed class while the getter returns the unboxed form — the property's
                    // declared type and its getter's return then disagreed ("return type mismatch:
                    // expected 'Boolean', actual 'Boolean'").
                    // A FUNCTION-typed property's descriptor erases to a raw `FunctionN` (all-`Any`,
                    // no receiver mark), which gives a lambda assigned to it no expected shape — its
                    // body's bare receiver calls and parameter types were unresolved. `@Metadata`'s
                    // property type keeps the full shape (`(Scope.(Req) -> Resp)?`), decoded into
                    // `generic_sig.ret`; prefer it for exactly that case. Every other property keeps
                    // the class-name reading below (the value-class/primitive reasoning applies).
                    let meta_fun_ty = mp
                        .generic_sig
                        .as_ref()
                        .map(|gsig| gsig.ret)
                        .filter(|ret| matches!(ret.non_null(), Ty::Fun(_)));
                    let ty = if let Some(fun_ty) = meta_fun_ty {
                        fun_ty
                    } else if ret_ty.is_jvm_scalar() {
                        ret_ty
                    } else {
                        mp.ret_class.map_or(Ty::obj("kotlin/Any"), Ty::obj_name)
                    };
                    let Some((mut getter_params, getter_ret)) = parse_method_desc(&getter.desc)
                    else {
                        continue;
                    };
                    // On a `@JvmInline value class` every member is realized as a STATIC `-impl` whose
                    // FIRST parameter is the CARRIER — the receiver, not a value parameter
                    // (`kotlin/Result.isSuccess` is `isSuccess-impl(Ljava/lang/Object;)Z`). Dropping it
                    // presents the same zero-parameter accessor an ordinary class exposes; the
                    // value-class pass already routes such a member's `dispatch_receiver` through the
                    // unboxed carrier. Without this the accessor was rejected as "takes an argument"
                    // and the property read reported as an unresolved reference.
                    // NOT the value class's own sole property: that one IS the carrier (`Result.value`),
                    // reached as the underlying value itself rather than through a computed `-impl`
                    // accessor, and rerouting it breaks the box/unbox boundary.
                    let carrier_receiver = getter_params.len() == 1
                        && metadata::class_inline(&ci).is_some_and(|inline| {
                            inline.property_name.as_deref() != Some(mp.name.as_str())
                        });
                    if carrier_receiver {
                        getter_params.clear();
                    }
                    if !getter_params.is_empty() {
                        continue;
                    }
                    let getter = LibraryCallable::library(
                        cn,
                        getter.name,
                        getter_params,
                        ret_ty,
                        getter_ret,
                        getter.desc,
                    );
                    let setter = mp.setter.clone().and_then(|s| {
                        let (params, physical_ret) = parse_method_desc(&s.desc)?;
                        if params.len() != 1 || physical_ret != Ty::Unit {
                            return None;
                        }
                        // The setter's descriptor erases a fun-typed property's value parameter to
                        // the raw `FunctionN`; assignment checks the written value against
                        // `params[0]`, so a lambda written to the property would get no shape.
                        // Publish the metadata-recovered property type as the LOGICAL parameter —
                        // the descriptor keeps driving the emitted `setX` call.
                        let params = match meta_fun_ty {
                            Some(fun_ty) => vec![fun_ty],
                            None => params,
                        };
                        Some(LibraryCallable::library(
                            cn,
                            s.name,
                            params,
                            Ty::Unit,
                            physical_ret,
                            s.desc,
                        ))
                    });
                    overloads.push(PropertyInfo {
                        kind: PropKind::Member,
                        receiver: Some(Ty::obj_name(cn)),
                        formals: Vec::new(),
                        ty,
                        context_count: 0,
                        getter,
                        setter,
                        is_const: mp.is_const,
                        visibility: mp.visibility,
                        owner: cn,
                        receiver_rank: rung,
                        source_key: None,
                    });
                }
            }
            if let Some(t) = self.resolve_type_name(cn) {
                queue.extend(t.supertypes.iter_ids());
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
        let mut queue = vec![internal];
        let mut seen = std::collections::HashSet::new();
        while let Some(cn) = queue.pop() {
            if !seen.insert(cn) {
                continue;
            }
            if self.cp.builtin_member_is_property_name(cn, name) {
                return true;
            }
            queue.extend(self.cp.builtin_supertypes_name(cn).iter_ids());
        }
        false
    }

    fn inheritance_shape_name(&self, internal: TypeName) -> Option<InheritanceShape> {
        let class = self.cp.find_name(internal)?;
        let mut pending = vec![internal];
        let mut seen = std::collections::HashSet::new();
        let mut has_abstract_obligations = false;
        while let Some(current) = pending.pop() {
            if !seen.insert(current) {
                continue;
            }
            let Some(current_class) = self.cp.find_name(current) else {
                continue;
            };
            if current_class
                .methods
                .iter()
                .any(|method| !method.is_static() && method.is_abstract())
            {
                has_abstract_obligations = true;
                break;
            }
            pending.extend(current_class.interfaces.iter_ids());
            pending.extend(current_class.super_class);
        }
        let ty = self.resolve_type_name(internal)?;
        Some(InheritanceShape {
            is_interface: class.is_interface(),
            is_extensible: !class.is_final() && !class.is_interface(),
            has_no_arg_constructor: ty
                .constructors
                .iter()
                .any(|constructor| constructor.params.is_empty()),
            supports_external_subclassing: !class.is_abstract() || !has_abstract_obligations,
        })
    }

    fn resolve_type(&self, internal: &str) -> Option<LibraryType> {
        self.resolve_type_name(type_name(internal))
            .map(|rc| (*rc).clone())
    }

    fn resolve_type_name(&self, internal_name: TypeName) -> Option<std::rc::Rc<LibraryType>> {
        if let Some(hit) = self.cp.cached_library_type_name(internal_name) {
            return hit;
        }
        // A classpath `typealias` (`kotlin/collections/ArrayList` → `java/util/ArrayList`) has no class of
        // its own; resolve the underlying type and tag it with `alias_target` so name resolution records
        // the real internal.
        let built = if let Some(target) = self.cp.type_alias_target_name(internal_name) {
            self.resolve_type_name(target).map(|rc| {
                let mut t = (*rc).clone();
                t.alias_target = Some(target);
                std::rc::Rc::new(t)
            })
        } else {
            self.build_library_type(internal_name).map(std::rc::Rc::new)
        };
        self.cp
            .cache_library_type_name(internal_name, built.clone());
        built
    }

    fn classifier_visibility(&self, internal_name: TypeName) -> Option<Visibility> {
        let jvm_name = super::jvm_class_map::to_jvm_type_name(internal_name);
        let class = self.cp.find_name(jvm_name)?;
        if let Some(visibility) = class.meta.class_visibility {
            return Some(visibility);
        }
        let access = effective_class_access(&class);
        Some(if access & 0x0001 != 0 {
            Visibility::Public
        } else if access & 0x0004 != 0 {
            Visibility::Protected
        } else {
            Visibility::Private
        })
    }

    fn classifier_access(
        &self,
        internal_name: TypeName,
    ) -> Option<crate::symbol_source::ClassifierAccess> {
        use crate::symbol_source::ClassifierAccess;

        let jvm_name = super::jvm_class_map::to_jvm_type_name(internal_name);
        let class = self.cp.find_name(jvm_name)?;
        if let Some(visibility) = class.meta.class_visibility {
            return Some(visibility.into());
        }
        let access = effective_class_access(&class);
        Some(if access & 0x0001 != 0 {
            ClassifierAccess::Public
        } else if access & 0x0004 != 0 {
            ClassifierAccess::Protected
        } else if access & 0x0002 != 0 {
            ClassifierAccess::Private
        } else {
            ClassifierAccess::PackagePrivate
        })
    }

    fn classifier_accessible_from_package(
        &self,
        internal_name: TypeName,
        accessor_package: TypeName,
    ) -> bool {
        let jvm_name = super::jvm_class_map::to_jvm_type_name(internal_name);
        let Some(class) = self.cp.find_name(jvm_name) else {
            return false;
        };
        if let Some(visibility) = class.meta.class_visibility {
            return visibility == Visibility::Public;
        }
        let access = effective_class_access(&class);
        if access & 0x0001 != 0 {
            true
        } else if access & 0x0002 != 0 {
            false
        } else {
            internal_package(internal_name) == accessor_package.render()
        }
    }

    fn inherited_classifier_shape(
        &self,
        internal_name: TypeName,
        inheritor: TypeName,
    ) -> Option<std::rc::Rc<LibraryType>> {
        let jvm_name = super::jvm_class_map::to_jvm_type_name(internal_name);
        let class = self.cp.find_name(jvm_name)?;
        // Inherited lookup applies only to a genuine member classifier. Requiring a structural self
        // entry avoids treating a top-level class whose legal name contains `$` as inheritable.
        let access = class.inner_class_self()?.access;
        let accessible = if let Some(visibility) = class.meta.class_visibility {
            matches!(visibility, Visibility::Public | Visibility::Protected)
        } else {
            if access & 0x0001 != 0 || access & 0x0004 != 0 {
                true
            } else if access & 0x0002 != 0 {
                false
            } else {
                internal_package(internal_name) == internal_package(inheritor)
            }
        };
        accessible
            .then(|| self.resolve_type_name(internal_name))
            .flatten()
    }

    fn resolve_symbols(&self, fqn: &str) -> crate::libraries::ResolvedSymbols {
        (*self.resolve_symbols_name(type_name(fqn))).clone()
    }

    fn resolve_symbols_name(
        &self,
        fqn: TypeName,
    ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
        use crate::libraries::{Callables, ResolvedSymbols};
        // The spec's top-level memo: this classpath `SymbolSource` composes the namespace record for `fqn`
        // once and reuses it across the compile. A `JvmLibraries` wrapper is rebuilt per snippet, but the
        // `Classpath` (which owns the memo) is reused on the worker thread, so hot stdlib names resolve
        // without re-walking metadata/extension indexes.
        if let Some(cached) = self.cp.cached_symbols_name(fqn) {
            return cached;
        }
        // Classifier namespace: the class/interface/object (or a typealias's target) at the fqn.
        let classifier = self.resolve_type_name(fqn);
        // Callable namespace, receiver-AGNOSTIC (resolution is by fqn; the receiver binds later, in the
        // consumer). Top-level functions of the source name declared in the fqn's package, plus the
        // package's extensions (source-keyed via the tree, so a `@JvmName`-mangled extension `sum` →
        // `sumOfInt` is found under its SOURCE name; the JVM name stays on the callable for emit).
        let pkg = fqn.parent().unwrap_or_else(|| type_name(""));
        let name = fqn.segment();
        // TOP-LEVEL functions of the package (receiver-less). The receiver-less `functions(name, None)`
        // query classifies each candidate by its metadata receiver, so an EXTENSION compiled into the same
        // facade surfaces here too — but with no receiver populated (`FnKind::Extension`, `receiver: None`),
        // a malformed shape for extension selection. Take only genuine `TopLevel` here; extensions come
        // from the receiver-carrying tree loop below, so the namespace has ONE clean source per kind.
        let mut overloads: Vec<_> = self
            .top_level_overloads(&name, pkg)
            .into_iter()
            .filter(|o| o.kind == FnKind::TopLevel)
            .collect();
        // Extension PROPERTIES of the source name live in the CALLABLE namespace's property half. A name is
        // functions XOR a property, so these are surfaced separately and chosen when there are no functions.
        let mut props: Vec<PropertyInfo> = Vec::new();
        // The fqn's parent need not be a PACKAGE. `import kotlin.time.Duration.Companion.minutes` names a
        // member of an OBJECT, and Kotlin's rule is that importing one brings it into scope with that
        // object as its implicit dispatch receiver — so an object-like classifier is a legal parent of a
        // callable name, exactly like a package. Its member EXTENSIONS are surfaced here; the singleton is
        // recorded on the callable (`singleton_dispatch`) because the emit is an instance invoke on
        // `Owner.INSTANCE` / `Outer.Companion`, not a facade `invokestatic`.
        self.object_member_extensions(pkg, &name, &mut overloads, &mut props);
        // Extension discovery is @Metadata-driven (the source of truth), NOT a scan of JVM statics: the
        // package's PUBLIC facades' metadata carry each extension's SOURCE receiver, parameters, return
        // (with nullability), visibility, and generic signature. The JVM method (`@JvmName`-mangled name +
        // descriptor) is only the emit handle, rooted at the PUBLIC facade — kotlinc's `invokestatic`
        // target — so a package-private multifile PART never leaks a `false` visibility. Receiver-coupled
        // JVM specifics (element-variant `sumOfInt`, value-class mangling) are the emitter's job.
        for facade in self.cp.package_facades_name(pkg) {
            let facade_rendered = facade.render();
            let lambda_return_overload = self
                .cp
                .lambda_return_overloads(&facade_rendered)
                .contains(&name);
            for mf in self.cp.meta_functions_name(facade).iter() {
                if mf.kotlin_name != name || !mf.is_extension() {
                    continue;
                }
                let raw_receiver = mf.generic_sig.as_ref().and_then(|g| g.receiver);
                let receiver = raw_receiver
                    .map(|r| match r {
                        Ty::TyParam(..) => r,
                        _ => ty_subst(r, &std::collections::HashMap::new()),
                    })
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
                let lambda_return_mangled = lambda_return_overload
                    .then_some(mf.ret_class)
                    .flatten()
                    .map(|ret| format!("{}{}", mf.kotlin_name, ret.segment()));
                // The receiver's erased descriptor disambiguates same-named overloads on the facade
                // (`maxOrNull([I)` vs `maxOrNull([D)`); the return descriptor disambiguates same-receiver
                // overloads (`maxOrNull(Iterable)Double` vs `…Comparable`). A type-var return has no class.
                let recv_desc = type_descriptor(
                    <Self as crate::libraries::SemanticPlatform>::library_value_form(
                        self,
                        raw_receiver.unwrap_or(receiver),
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
                // a FUNCTION type (`any(Iterable, Function1)` vs `any(Iterable)`). Type variables erase
                // through their bounds. `None` (match by receiver alone) means no generic signature.
                let value_param_descs: Option<Vec<String>> = mf.generic_sig.as_ref().map(|g| {
                    g.params
                        .iter()
                        .map(|p| {
                            type_descriptor(
                                <Self as crate::libraries::SemanticPlatform>::library_value_form(
                                    self, *p,
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
                // Match the bytecode method to recover its descriptor and inline implementation details.
                let (jvm_name, descriptor, cand) = if let Some(d) = mf.jvm_desc {
                    (mf.jvm_name.clone(), d.to_string(), by_name(&mf.jvm_name))
                } else if let Some(c) = by_name(&mf.jvm_name) {
                    (c.name.clone(), c.descriptor.clone(), Some(c))
                } else if let Some(c) = lambda_return_mangled.as_ref().and_then(|n| by_name(n)) {
                    (c.name.clone(), c.descriptor.clone(), Some(c))
                } else if let Some(c) = elem_mangled.as_ref().and_then(|n| by_name(n)) {
                    (c.name.clone(), c.descriptor.clone(), Some(c))
                } else {
                    continue;
                };
                let bytecode_public = cand.as_ref().map_or(mf.is_public(), |c| c.public);
                // A `suspend fun`'s physical method appends a `Continuation` parameter and erases the
                // return to `Object`; present the LOGICAL signature (drop the continuation) so a
                // normal call resolves — the same rule the top-level and member paths apply. The
                // coroutine pass re-threads the CPS `Continuation` at the emitted call.
                let descriptor = if mf.is_suspend() {
                    strip_continuation_param(&descriptor)
                } else {
                    descriptor
                };
                let Some((params, pret)) = parse_method_desc(&descriptor) else {
                    continue;
                };
                if params.is_empty() {
                    continue;
                }
                let call_sig = mf.extension_call_sig();
                let ret_class = mf.ret_class.map(kotlin_type_name_to_ty);
                let ret = match ret_class {
                    Some(t) if mf.ret_nullable() && t.is_jvm_scalar() => Ty::nullable(t),
                    Some(t) => t,
                    None if mf.ret_nullable() && pret.is_jvm_scalar() => Ty::nullable(pret),
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
                let inline =
                    InlineKind::from_flags(mf.is_inline(), mf.is_inline() && !bytecode_public);
                let callable = LibraryCallable {
                    inline,
                    suspend: mf.is_suspend(),
                    source_receiver,
                    context_count: mf.context_count,
                    contract: mf.contract.clone(),
                    generic_sig: generic_sig.clone().map(Box::new),
                    // Carry the resolved bytecode method's generic `Signature` — a `<reified T>` extension's
                    // splice reads its formal-type-parameter NAMES from here to bind the call's explicit
                    // type arguments. Without it the reified body cannot be specialized and the call falls
                    // back to a (throwing) direct invoke of the inline-only method.
                    signature: cand.as_ref().and_then(|c| c.signature.clone()),
                    ..LibraryCallable::library(facade, jvm_name, params, ret, pret, descriptor)
                };
                overloads.push(FunctionInfo {
                    ret: ReturnInfo::new(mf.ret_nullable(), ret_class),
                    visibility: mf.visibility,
                    generic_sig,
                    context_count: mf.context_count,
                    flags: FnFlags {
                        inline,
                        suspend: mf.is_suspend(),
                        operator: mf.is_operator(),
                    },
                    call_sig,
                    ..FunctionInfo::plain(FnKind::Extension, Some(receiver), callable)
                });
            }
            // PROPERTIES declared by the facade — receiver-less TOP-LEVEL ones (`val plugin: Plugin`)
            // and EXTENSION ones (`arr.lastIndex`, `list.indices`). Both are the callable namespace's
            // property half, and both differ only by whether the accessors take a receiver parameter.
            // Metadata records the property with its REAL accessor names (`JvmPropertySignature`), so the
            // getter name is authoritative, never a `getX` guess. Facade parts are merged in the shared
            // cached decode (`meta_properties_name`), the property analogue of `meta_functions_name`.
            let mprops = self.cp.meta_properties_name(facade);
            for mp in mprops.iter() {
                if mp.name != name {
                    continue; // this property name
                }
                // The accessors' receiver parameter: present iff the property is an extension.
                let receiver_params = usize::from(mp.is_extension);
                let mp = mp.clone();
                let property_gsig = mp.generic_sig.clone();
                let Some(getter_sig) = mp.getter else {
                    continue;
                };
                let Some(facade_class) = self.cp.find_name(facade) else {
                    continue;
                };
                if !facade_class.methods.iter().any(|method| {
                    method.is_public()
                        && method.is_static()
                        && method.name == getter_sig.name
                        && method.descriptor == getter_sig.desc
                }) {
                    continue;
                }
                let Some((gparams, gret)) = parse_method_desc(&getter_sig.desc) else {
                    continue;
                };
                if gparams.len() != receiver_params {
                    continue;
                }
                let generic_receiver = property_gsig.as_ref().and_then(|gsig| gsig.receiver);
                let fallback_ret = mp.ret_class.map_or(gret, kotlin_type_name_to_ty);
                let property_ty = property_gsig.as_ref().map_or_else(
                    || {
                        if mp.ret_nullable {
                            Ty::nullable(fallback_ret)
                        } else {
                            fallback_ret
                        }
                    },
                    |gsig| gsig.ret,
                );
                let getter = LibraryCallable::library(
                    facade,
                    getter_sig.name,
                    gparams,
                    property_ty,
                    gret,
                    getter_sig.desc,
                );
                let setter = mp.setter.and_then(|setter_sig| {
                    let (sparams, sret) = parse_method_desc(&setter_sig.desc)?;
                    if sparams.len() != receiver_params + 1 || sret != Ty::Unit {
                        return None;
                    }
                    if !facade_class.methods.iter().any(|method| {
                        method.is_public()
                            && method.is_static()
                            && method.name == setter_sig.name
                            && method.descriptor == setter_sig.desc
                    }) {
                        return None;
                    }
                    Some(LibraryCallable::library(
                        facade,
                        setter_sig.name,
                        sparams,
                        Ty::Unit,
                        sret,
                        setter_sig.desc,
                    ))
                });
                props.push(PropertyInfo {
                    kind: if mp.is_extension {
                        PropKind::Extension
                    } else {
                        PropKind::TopLevel
                    },
                    receiver: mp.is_extension.then(|| {
                        generic_receiver.unwrap_or_else(|| {
                            mp.receiver_class
                                .map_or(Ty::obj("kotlin/Any"), Ty::obj_name)
                        })
                    }),
                    formals: property_gsig
                        .as_ref()
                        .map(|gsig| gsig.formals.clone())
                        .unwrap_or_default(),
                    ty: property_ty,
                    context_count: 0,
                    getter,
                    setter,
                    is_const: mp.is_const,
                    visibility: mp.visibility,
                    owner: facade,
                    receiver_rank: 0,
                    source_key: None,
                });
            }
        }
        let callables = match (overloads.is_empty(), props.is_empty()) {
            (false, false) => Callables::Both {
                functions: FunctionSet { overloads },
                properties: PropertySet { overloads: props },
            },
            (false, true) => Callables::Functions(FunctionSet { overloads }),
            (true, false) => Callables::Properties(PropertySet { overloads: props }),
            (true, true) => Callables::None,
        };
        self.cp.memoize_symbols_name(
            fqn,
            ResolvedSymbols {
                classifier,
                callables,
            },
        )
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
            queue.push_back(internal);
            let mut rung: u32 = 0;
            while let Some(cn) = queue.pop_front() {
                if !seen.insert(cn) {
                    continue;
                }
                let Some(t) = self.resolve_type_name(cn) else {
                    continue;
                };
                // Compute the provider-owned rename shapes once per hierarchy rung. A raw Java
                // method is renamed only when its physical name AND full erased descriptor match a
                // mapped interface obligation carried by this receiver's actual class hierarchy.
                // This keeps overload identity precise without a classpath-only reverse-name table.
                let function_renames = self.mapped_collection_function_renames(cn);
                for m in &t.members {
                    // A member extension has TWO receivers: the declaring class supplies the implicit
                    // dispatch receiver and the method's first JVM parameter is the extension receiver.
                    // It is therefore not an ordinary member of `receiver`, even though both shapes are
                    // encoded as instance methods in the class file. The dedicated semantic
                    // member-extension query consumes this declaration; exposing it here as well would
                    // incorrectly accept `scope.invoke("x", body)` and create two resolution paths for
                    // the same declaration.
                    if m.is_member_extension() {
                        continue;
                    }
                    // The name this member is visible under in the receiver's SOURCE scope. Normally
                    // its own; for a Java method that a renamed builtin covers, the KOTLIN name
                    // INSTEAD of the JVM one — so `ArrayList.remove(int)` answers `removeAt` and is
                    // invisible to `remove`, exactly as kotlinc's Java member scope resolves it. The
                    // emitted callable is unaffected: it reads `m.physical_name`/`m.name` below, so the
                    // call still goes to `remove(I)`. A member that already carries a `physical_name`
                    // is a synthesized alias (a value-class mangled member, or a builtin whose Kotlin
                    // name was recorded there), never a raw Java method — leave it alone.
                    let scope_name = m.physical_name.as_ref().map_or_else(
                        || {
                            function_renames
                                .iter()
                                .find(|mapping| {
                                    mapping.physical_name == m.name
                                        && method_descriptor(&mapping.params, mapping.ret)
                                            == m.descriptor
                                })
                                .map_or(m.name.as_str(), |mapping| mapping.source_name.as_str())
                        },
                        |_| m.name.as_str(),
                    );
                    if scope_name == name
                        || matches!(
                            (scope_name, name),
                            ("keySet", "keys") | ("entrySet", "entries")
                        )
                    {
                        let cn_rendered = cn.render();
                        crate::trace_compiler!(
                            "resolve",
                            "member walk {cn_rendered}.{} (rung {rung}) desc={} sig={:?}",
                            m.name,
                            m.descriptor,
                            m.signature
                        );
                        // The provider-normalized signature is the single source of truth for every
                        // carrier. For a `.class` member it also restores receiver function-type marks
                        // that the raw JVM `Signature` attribute cannot spell; for a
                        // `.kotlin_builtins` member there is no raw attribute at all, so the provider's
                        // decoded signature is what preserves and binds its type parameters. Re-parsing
                        // `m.signature` here would therefore lose facts in the former case and fail to
                        // produce any signature in the latter.
                        //
                        // A member signature can mention its OWNER's type variables in its value
                        // parameters, including under a function/SAM argument. Bind those variables
                        // from the applied dispatch type before call-site inference, exactly as return
                        // recovery below does. This is deliberately independent of whether the
                        // provider represented a receiver in `GenericSig`: metadata signatures carry
                        // one while a raw JVM `Signature` cannot, but both describe the same semantic
                        // need. The partial substitution policy leaves the member's own formals open.
                        let generic_sig = m.generic_sig.clone().map(|signature| {
                            let bindings = self.member_receiver_bindings_name(
                                receiver,
                                cn,
                                &signature.formals,
                            );
                            if bindings.is_empty() {
                                return signature;
                            }
                            GenericSig {
                                params: signature
                                    .params
                                    .iter()
                                    .map(|param| ty_subst_keep_unbound(*param, &bindings))
                                    .collect(),
                                ..signature
                            }
                        });
                        // A `suspend fun` member's physical method appends a `Continuation` parameter
                        // and erases its return to `Object`; present the LOGICAL signature (drop the
                        // continuation, recover the real return from the `Continuation<T>` type
                        // argument in the generic signature) so a normal call resolves. The coroutine
                        // pass re-derives the CPS form for the emit.
                        let suspend = m.suspend();
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
                        let meta_name = m.physical_name.as_deref().unwrap_or(&m.name);
                        let member_facts = self.cp.metadata_member_call_facts_name(
                            cn,
                            meta_name,
                            &m.descriptor,
                            &|name| self.value_underlying_name(name),
                        );
                        // A metadata FUNCTION's structured return comes from the already-aligned call
                        // facts, keeping overload selection single-sourced. A property GETTER has no
                        // metadata-function record, so only that shape consults the property-signature
                        // fallback. Do this once before the plain/suspend split so both paths consume
                        // the identical semantic return projection.
                        let metadata_ret = match member_facts.as_ref() {
                            Some(facts) => facts.declared_ret,
                            None => {
                                self.cp
                                    .metadata_property_ret_ty_name(cn, meta_name, &m.descriptor)
                            }
                        };
                        let member_ret_metadata = suspend.then(|| {
                            member_facts
                                .as_ref()
                                .map(|facts| facts.ret)
                                .unwrap_or_else(|| ReturnInfo::new(m.ret_nullable(), Some(m.ret)))
                        });
                        let suspend_ret_nullable = suspend
                            && member_ret_metadata.is_some_and(|metadata| metadata.nullable);
                        let ret = if suspend {
                            // A generic `suspend` member returns a type parameter (`byId(): T`) via
                            // `Continuation<T>`; bind `T` to the receiver's concrete argument
                            // (`Repo<Cfg>` → `T = Cfg`) so the return isn't erased to `Any`.
                            let recv_binds = generic_sig
                                .as_ref()
                                .map(|signature| {
                                    self.member_receiver_bindings_name(
                                        receiver,
                                        cn,
                                        &signature.formals,
                                    )
                                })
                                .unwrap_or_default();
                            let base = generic_sig
                                .as_ref()
                                .and_then(|g| suspend_return_from_gsig(g, &recv_binds))
                                .unwrap_or(m.ret);
                            // `suspend_return_from_gsig` canonicalized a collection return to its
                            // READ-ONLY Kotlin form (the JVM signature erases read-only vs mutable).
                            // Recover the EXACT source form (`List` vs `MutableList`, …) from the
                            // member's `@Metadata` return type — which preserves it at every nesting
                            // level — under the same-JVM-internal guard, so `.add(…)` on a declared
                            // `MutableList` (or on its `MutableSet` element) return still resolves.
                            // Ordinary and suspend members must not grow separate metadata/JVM merge
                            // policies: both arms overlay through the same guarded projection; only
                            // the way each obtains `base` differs (Continuation generic argument
                            // here, ordinary generic signature below).
                            let base = metadata_ret
                                .map_or(base, |meta| overlay_metadata_collection_names(base, meta));
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
                            let recovered = generic_sig.as_ref().and_then(|signature| {
                                concrete_generic_ret(signature).or_else(|| {
                                    let bindings = self.member_receiver_bindings_name(
                                        receiver,
                                        cn,
                                        &signature.formals,
                                    );
                                    (!bindings.is_empty())
                                        .then(|| ty_subst(signature.ret, &bindings))
                                })
                            });
                            crate::trace_compiler!(
                                "resolve",
                                "member return {}.{}: recovered={:?} erased={:?} (gsig={})",
                                receiver.name(),
                                m.name,
                                recovered,
                                m.ret,
                                generic_sig.is_some()
                            );
                            // The JVM signature erases read-only vs mutable (`List`/`MutableList`
                            // both spell `java/util/List`) at every nesting level; the member's
                            // `@Metadata` return type preserves it — for a FUNCTION and for a
                            // property GETTER (which is not a metadata function) alike. Overlay the
                            // metadata classifiers under the same-JVM-internal guard, per level —
                            // the same projection the suspend arm applies.
                            let base = recovered.unwrap_or(m.ret);
                            metadata_ret
                                .map_or(base, |meta| overlay_metadata_collection_names(base, meta))
                        };
                        let call_sig = member_facts
                            .as_ref()
                            .map(|facts| facts.call_sig.clone())
                            .unwrap_or_else(|| m.call_sig.clone());
                        // A generic-return builtin member (`Map.get(K): V?`) resolves to the erased
                        // classpath method (`java/util/Map.get` → `Object`), which carries no Kotlin
                        // nullability. Recover the source `V?` from the builtin `@Metadata`. Applied
                        // only for a PRIMITIVE return (a nullable primitive is a distinct BOXED type,
                        // so `m[k] ?: d` must null-check before unboxing); a nullable REFERENCE already
                        // null-checks regardless, and keeps its plain erased `Ty` (see below).
                        // `cn` may already be the front-end `kotlin/collections/…` name or the erased
                        // JVM form (`java/util/Map`, when the member is found on a classpath supertype);
                        // map both to the builtin whose `@Metadata` declares the nullability.
                        let builtin_cn =
                            super::jvm_class_map::jvm_to_kotlin_builtin_metadata_name(cn)
                                .unwrap_or(cn);
                        let builtin_ret_nullable = !ret.is_reference()
                            && self.cp.builtin_member_ret_nullable_name(
                                builtin_cn,
                                &m.name,
                                params.len(),
                            );
                        let callable = LibraryCallable {
                            inline: m.inline,
                            suspend,
                            signature: m.signature.clone(),
                            // Preserve the declaration-level return recovered when the class member
                            // was aligned with metadata. This overload view is the common input to
                            // instance-member selection (named calls and operator `invoke` alike);
                            // rebuilding a callable without the fact makes a later specialized
                            // `Object` return indistinguishable from a genuinely boxed generic slot.
                            declared_ret: m.declared_ret,
                            // Whether the dispatch owner is an interface is the MEMBER's fact here.
                            // For a mapped builtin resolved with no JDK on the classpath the JVM
                            // owner (`java/util/List`) has no class file, so the call site cannot
                            // recover it later — carry it on the selected callable.
                            owner_is_interface: m.is_interface(),
                            ..LibraryCallable::library(
                                m.owner.as_ref().cloned().unwrap_or(cn),
                                m.physical_name.clone().unwrap_or_else(|| m.name.clone()),
                                params,
                                ret,
                                m.physical_ret,
                                descriptor,
                            )
                        };
                        overloads.push(FunctionInfo {
                            ret: ReturnInfo::new(
                                m.ret_nullable()
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
                                operator: member_facts
                                    .as_ref()
                                    .is_some_and(|facts| facts.is_operator),
                            },
                            ..FunctionInfo::plain(FnKind::Member, Some(receiver), callable)
                        });
                    }
                }
                queue.extend(t.supertypes.iter_ids());
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

    fn is_erased_contract_callable(&self, callable: &crate::libraries::LibraryCallable) -> bool {
        // Contract erasure is a source-language decision, but the physical declaration owner is a
        // JVM-library fact. Keep that fact here: target-neutral resolve code sees only the selected
        // callable and never embeds or reports the runtime facade class name. Requiring both the
        // source name and declaring package prevents an unrelated library callable from acquiring
        // intrinsic behavior merely because one component happens to match.
        callable.name == "contract"
            && callable
                .owner
                .parent()
                .is_some_and(|package| package.matches("kotlin/contracts"))
    }

    fn supports_member_reference(&self, member: &LibraryMember) -> bool {
        let Some(owner) = member.owner else {
            return false;
        };
        let owner = super::jvm_class_map::to_jvm_type_name(owner);
        let owner_rendered = owner.render();
        let source_name = member.physical_name.as_deref().unwrap_or(&member.name);
        let physical_name =
            crate::jvm::names::mapped_builtin_virtual_name(&owner_rendered, source_name);
        self.cp.find_name(owner).is_some_and(|ci| {
            ci.methods.iter().any(|method| {
                method.is_public()
                    && !method.is_static()
                    && method.name == physical_name
                    && method.descriptor == member.descriptor
            })
        })
    }

    fn implicit_common_supertypes(&self, types: &[Ty]) -> Vec<crate::libraries::SemanticSupertype> {
        let boxed_builtin = |ty: Ty| {
            let ty = ty.non_null();
            ty.jvm_boxed_ref().is_some() || ty == Ty::String
        };
        if types.len() > 1 && types.iter().copied().all(boxed_builtin) {
            vec![
                crate::libraries::SemanticSupertype {
                    name: crate::types::type_name("kotlin/Comparable"),
                    type_parameters: 1,
                },
                crate::libraries::SemanticSupertype {
                    name: crate::types::type_name("java/io/Serializable"),
                    type_parameters: 0,
                },
            ]
        } else {
            Vec::new()
        }
    }

    fn static_field(&self, internal: &str, name: &str) -> Option<crate::libraries::StaticFieldRef> {
        let internal = crate::types::existing_type_name(internal)?;
        self.static_field_name(internal, name)
    }

    fn top_level_static_field(
        &self,
        package: TypeName,
        name: &str,
    ) -> Option<crate::libraries::StaticFieldRef> {
        // A top-level `const val` is a `public static final` field on the package FACADE that carries
        // the declaration (`kotlin.math.PI` → `kotlin/math/MathKt.PI`). Which facade that is, is a
        // platform fact, so the scan lives here rather than in the resolver.
        self.cp
            .package_facades_name(package)
            .into_iter()
            .find_map(|facade| self.static_field_name(facade, name))
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
            if let Some(f) = ci
                .fields
                .iter()
                .find(|f| f.name == name && f.access & 0x0008 != 0 && f.access & 0x0001 != 0)
            {
                let ty = f
                    .signature
                    .as_deref()
                    .and_then(|signature| parse_concrete_field_gsig(signature, &f.descriptor))
                    .unwrap_or_else(|| field_desc_to_ty(&f.descriptor));
                let constant = f.const_value.as_ref().map(|value| LibraryConst {
                    ty,
                    value: Self::library_const(value),
                });
                return Some(crate::libraries::StaticFieldRef {
                    owner: cur,
                    name: name.to_string(),
                    descriptor: Some(f.descriptor.clone()),
                    ty,
                    constant,
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
            u if u.is_unsigned() => u.scalar_value_repr(),
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

    fn canonical_source_type_name(&self, internal: TypeName) -> TypeName {
        super::jvm_class_map::jvm_collection_to_kotlin_type_name(internal).unwrap_or(internal)
    }

    fn is_default_library_owner(&self, internal: TypeName) -> bool {
        // One identity table owns every Kotlin builtin and its mapped JVM face. This capability used
        // to be inferred from three partial maps (Kotlin spelling, collection inverse, and a curated
        // interface-name subset), so adding a mapped class could change the answer depending on which
        // unrelated facet happened to list it.
        internal.starts_with("kotlin/")
            || super::jvm_class_map::type_name_to_jvm_builtin_internal(internal).is_some()
    }

    fn boxed_primitive(&self, ty: Ty) -> Option<Ty> {
        let internal = ty.non_null().obj_internal()?;
        super::jvm_class_map::wrapper_to_kotlin_prim_name(internal)
            .map(super::classpath::kotlin_name_to_ty)
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

    fn property_reference_type(&self, arity: usize, mutable: bool, args: &[Ty]) -> Option<Ty> {
        let internal = match (arity, mutable) {
            (0, false) => "kotlin/reflect/KProperty0",
            (0, true) => "kotlin/reflect/KMutableProperty0",
            (1, false) => "kotlin/reflect/KProperty1",
            (1, true) => "kotlin/reflect/KMutableProperty1",
            _ => return None,
        };
        // `KProperty0<V>` / `KProperty1<T, V>`: carrying the arguments is what lets `get()` report
        // the property's type rather than the erased `Object` upper bound. An argument list of the
        // wrong length (or one that could not be determined) leaves the reference raw, as before.
        if args.len() == arity + 1 && !args.contains(&Ty::Error) {
            return Some(Ty::obj_args(internal, args));
        }
        Some(Ty::obj(internal))
    }

    fn class_literal_type(&self) -> Option<Ty> {
        // A Kotlin class-literal expression `X::class` has type `kotlin.reflect.KClass` (emitted via
        // `Reflection.getOrCreateKotlinClass`); `X::class.java` unwraps it back to `java.lang.Class`.
        Some(Ty::obj("kotlin/reflect/KClass"))
    }

    fn intrinsic_property(&self, receiver: Ty, name: &str) -> Option<LibraryMember> {
        if name != "javaClass" || !receiver.non_null().is_reference() {
            return None;
        }
        let mut member = LibraryMember::new(
            "getClass".to_string(),
            vec![],
            Ty::obj("java/lang/Class"),
            "()Ljava/lang/Class;".to_string(),
        );
        member.owner = Some(type_name("java/lang/Object"));
        Some(member)
    }

    fn platform_default_import_packages(&self) -> &'static [&'static str] {
        PLATFORM_DEFAULT_IMPORT_PACKAGES
    }

    fn physical_property_getter_names(&self, property: &str) -> Vec<String> {
        let mut candidates: Vec<String> = self
            .physical_property_getter_name(property)
            .into_iter()
            .collect();
        // Kotlin maps `getID()` → `id` and `getURLPath()` → `urlPath` (decapitalize-smart:
        // a LEADING UPPERCASE RUN lowercases as a block). Invert that: re-uppercase the
        // property's leading lowercase run for the all-caps getter spelling.
        let run = property
            .chars()
            .take_while(|c| c.is_ascii_lowercase())
            .count();
        if run > 1 || (run == property.len() && run >= 1) || (run == 1 && property.len() == 1) {
            let smart = format!(
                "get{}{}",
                property[..run].to_ascii_uppercase(),
                &property[run..]
            );
            if !candidates.contains(&smart) {
                candidates.push(smart);
            }
        }
        candidates
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

    fn mapped_interface_members(
        &self,
        supertype: Ty,
    ) -> Vec<crate::libraries::MappedInterfaceMember> {
        let Some(internal) = supertype.obj_internal() else {
            return Vec::new();
        };
        let mut members = Vec::new();
        let jvm_internal = crate::jvm::jvm_class_map::to_jvm_type_name(internal);
        let collection_properties: &[&str] = if jvm_internal.matches("java/util/Map") {
            &["size", "values", "keys", "entries"]
        } else if jvm_internal.matches("java/util/Collection")
            || jvm_internal.matches("java/util/List")
            || jvm_internal.matches("java/util/Set")
        {
            &["size"]
        } else {
            &[]
        };
        for &property in collection_properties {
            let Some((physical, ret)) = crate::jvm::names::collection_property_stub(property)
            else {
                continue;
            };
            members.push(crate::libraries::MappedInterfaceMember {
                source_name: property.to_string(),
                physical_name: physical.to_string(),
                params: Vec::new(),
                ret,
                is_property: true,
            });
        }
        // `MutableList.removeAt(Int): E` IS `java.util.List.remove(int)` — the function half of the
        // same special-builtin renaming the properties above cover, so a class implementing
        // `MutableList` must expose its `removeAt` override under the JVM name too. Keyed on the
        // KOTLIN name, not the erased `java/util/List`: unlike `size`, this member exists only on the
        // MUTABLE side, so a read-only `List` implementation that happens to declare an unrelated
        // `removeAt` must not acquire a `remove(int)` bridge kotlinc would never emit.
        if internal.matches("kotlin/collections/MutableList") {
            members.push(crate::libraries::MappedInterfaceMember {
                source_name: "removeAt".to_string(),
                physical_name: "remove".to_string(),
                params: vec![Ty::Int],
                ret: Ty::obj("kotlin/Any"),
                is_property: false,
            });
        }
        if internal.matches("kotlin/CharSequence") || internal.matches("java/lang/CharSequence") {
            members.push(crate::libraries::MappedInterfaceMember {
                source_name: "length".to_string(),
                physical_name: "length".to_string(),
                params: Vec::new(),
                ret: Ty::Int,
                is_property: true,
            });
            members.push(crate::libraries::MappedInterfaceMember {
                source_name: "get".to_string(),
                physical_name: "charAt".to_string(),
                params: vec![Ty::Int],
                ret: Ty::Char,
                is_property: false,
            });
        }
        members
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

    fn descriptor_method_layout(
        &self,
        descriptor: &str,
    ) -> Option<crate::runtime::PlatformMethodLayout> {
        method_layout(descriptor)
    }

    fn function_reference_impl_type(&self) -> Option<Ty> {
        Some(Ty::obj("kotlin/jvm/internal/FunctionReferenceImpl"))
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
            Ty::Byte | Ty::UByte => "kotlin/jvm/internal/Ref$ByteRef",
            Ty::Short | Ty::UShort => "kotlin/jvm/internal/Ref$ShortRef",
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
            RuntimeOp::UnsignedBox | RuntimeOp::UnsignedUnbox | RuntimeOp::UnsignedEquals => {
                // Every unsigned type boxes through its OWN inline class (`kotlin/UByte`, …) over the
                // signed primitive it erases to — one row derived from the `Ty`, not a per-type table.
                if !ty.is_unsigned() {
                    return None;
                }
                let owner = &ty.kotlin_class_internal()?.render();
                let prim = crate::jvm::names::type_descriptor(ty);
                let repr = ty.scalar_value_repr()?;
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
                    // The compiled form of `override fun equals(other: Any?)` on the inline class: the
                    // receiver is the CARRIER in a primitive slot, so nothing boxes to make the call.
                    RuntimeOp::UnsignedEquals => callable(
                        owner,
                        "equals-impl",
                        // Kotlin declares `equals(other: Any?)`. The descriptor still erases the
                        // nullable reference to `Object`; retain nullability in the semantic row so
                        // target-independent lowering and JVM realization describe the same call.
                        vec![ty, Ty::nullable(Ty::obj("kotlin/Any"))],
                        Ty::Boolean,
                        Ty::Boolean,
                        format!("({prim}Ljava/lang/Object;)Z"),
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
            RuntimeOp::UnsignedToDouble if ty == Ty::UInt => callable(
                "kotlin/UnsignedKt",
                "uintToDouble",
                vec![Ty::UInt],
                Ty::Double,
                Ty::Double,
                "(I)D".to_string(),
            ),
            RuntimeOp::UnsignedToDouble if ty == Ty::ULong => callable(
                "kotlin/UnsignedKt",
                "ulongToDouble",
                vec![Ty::ULong],
                Ty::Double,
                Ty::Double,
                "(J)D".to_string(),
            ),
            RuntimeOp::UnsignedToDouble => None,
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
            RuntimeOp::ArrayHashCode => {
                // A data class CONTENT-hashes an array field via `java.util.Arrays.hashCode([X)I`
                // (kotlinc's shape), not the array's identity `Object.hashCode`. An UNSIGNED array
                // is a stdlib value class over the signed carrier — kotlinc routes its hash through
                // the class's own static `hashCode-impl(<carrier>)I` instead of `Arrays`.
                let internal = ty.non_null().obj_internal();
                if let Some(n) = internal {
                    let unsigned = if n.matches("kotlin/UIntArray") {
                        Some(("kotlin/UIntArray", "([I)I"))
                    } else if n.matches("kotlin/ULongArray") {
                        Some(("kotlin/ULongArray", "([J)I"))
                    } else if n.matches("kotlin/UByteArray") {
                        Some(("kotlin/UByteArray", "([B)I"))
                    } else if n.matches("kotlin/UShortArray") {
                        Some(("kotlin/UShortArray", "([S)I"))
                    } else {
                        None
                    };
                    if let Some((owner, desc)) = unsigned {
                        return callable(
                            owner,
                            "hashCode-impl",
                            vec![ty],
                            Ty::Int,
                            Ty::Int,
                            desc.to_string(),
                        );
                    }
                }
                let desc = match array_kotlin_fq(ty.non_null())? {
                    "kotlin/BooleanArray" => "([Z)I",
                    "kotlin/CharArray" => "([C)I",
                    "kotlin/ByteArray" => "([B)I",
                    "kotlin/ShortArray" => "([S)I",
                    "kotlin/IntArray" => "([I)I",
                    "kotlin/LongArray" => "([J)I",
                    "kotlin/FloatArray" => "([F)I",
                    "kotlin/DoubleArray" => "([D)I",
                    "kotlin/Array" => "([Ljava/lang/Object;)I",
                    _ => return None,
                };
                callable(
                    "java/util/Arrays",
                    "hashCode",
                    vec![ty],
                    Ty::Int,
                    Ty::Int,
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
            RuntimeOp::StartCoroutineReceiver => callable(
                "kotlin/coroutines/ContinuationKt",
                "startCoroutine",
                vec![
                    Ty::obj("kotlin/Function2"),
                    Ty::obj("kotlin/Any"),
                    Ty::obj("kotlin/coroutines/Continuation"),
                ],
                Ty::Unit,
                Ty::Unit,
                "(Lkotlin/jvm/functions/Function2;Ljava/lang/Object;Lkotlin/coroutines/Continuation;)V"
                    .to_string(),
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
    use super::{
        desc_to_ty, method_layout, overlay_metadata_collection_names, parse_class_gsig,
        parse_concrete_field_gsig, parse_field_gsig, parse_method_desc, parse_method_gsig,
    };
    use crate::libraries::{GenericReturnPolicy, SemanticPlatform};
    use crate::symbol_source::SymbolSource;
    use crate::types::type_name;
    use crate::types::Ty;

    /// Common lowering aligns a `suspend` `$default` call by this position, so it has to name the
    /// slot the backend fills and nothing else.
    #[test]
    fn descriptor_layout_reports_representation_and_one_continuation_from_one_parse() {
        // `withLock$default` — the continuation sits BEFORE the mask/marker tail, not at the end.
        let layout = method_layout(
            "(Lkotlinx/coroutines/sync/Mutex;Ljava/lang/Object;Lkotlin/jvm/functions/Function0;\
                 Lkotlin/coroutines/Continuation;ILjava/lang/Object;)Ljava/lang/Object;",
        )
        .expect("valid descriptor");
        assert_eq!(layout.continuation_slot, Some(3));
        assert_eq!(layout.return_class, Some(type_name("java/lang/Object")));
        assert_eq!(
            layout.reference_slots,
            vec![true, true, true, true, false, true]
        );
        // A plain suspend method's trailing continuation.
        assert_eq!(
            method_layout("(ILkotlin/coroutines/Continuation;)Ljava/lang/Object;")
                .and_then(|layout| layout.continuation_slot),
            Some(1)
        );
        assert_eq!(
            method_layout("(ILjava/lang/String;)V")
                .expect("valid descriptor")
                .continuation_slot,
            None
        );
        // Two of them: no position is derivable, so the caller must not be handed a guess.
        assert_eq!(
            method_layout("(Lkotlin/coroutines/Continuation;Lkotlin/coroutines/Continuation;)V")
                .expect("the parameter representations remain readable")
                .continuation_slot,
            None
        );
        assert!(method_layout("not a descriptor").is_none());
    }

    #[test]
    fn inherited_access_finds_self_entry_when_member_name_contains_dollar() {
        let stubs = crate::jvm::java_stub::stub_classes(
            &[(
                String::new(),
                "class Outer { public static class Inner$Part {} }".into(),
            )],
            crate::jvm::java_stub::StubMode::Strict,
            &|candidate| candidate == "java/lang/Object",
        )
        .expect("stubs");
        let class = stubs
            .iter()
            .find(|(name, _)| name == "Outer$Inner$Part")
            .and_then(|(_, bytes)| crate::jvm::classreader::parse_class(bytes).ok())
            .expect("member class");

        assert_eq!(
            class
                .inner_class_self()
                .map(|entry| entry.access & crate::jvm::classfile::ACC_PUBLIC),
            Some(crate::jvm::classfile::ACC_PUBLIC)
        );
    }

    #[test]
    fn descriptor_void_reference_normalizes_before_core() {
        assert_eq!(desc_to_ty("Ljava/lang/Void;"), Ty::Unit);
        assert_eq!(desc_to_ty("V"), Ty::Unit);
    }

    #[test]
    fn descriptor_arrays_preserve_primitive_element_width() {
        assert_eq!(desc_to_ty("B"), Ty::Int);
        assert_eq!(desc_to_ty("S"), Ty::Int);
        assert_eq!(desc_to_ty("[B"), Ty::array(Ty::Byte));
        assert_eq!(desc_to_ty("[S"), Ty::array(Ty::Short));
        assert_eq!(desc_to_ty("[I"), Ty::array(Ty::Int));
        assert_eq!(desc_to_ty("[[B"), Ty::array(Ty::array(Ty::Byte)));
        assert_eq!(desc_to_ty("[Ljava/lang/String;"), Ty::array(Ty::String));
        assert_eq!(
            desc_to_ty("[[Ljava/lang/String;"),
            Ty::array(Ty::array(Ty::String))
        );
    }

    #[test]
    fn shared_field_walk_honors_hiding_declarations() {
        let sources = [
            (
                String::new(),
                "package sample; public class Base { public int value; }".into(),
            ),
            (
                String::new(),
                "package sample; public class PrivateChild extends Base { private int value; }"
                    .into(),
            ),
            (
                String::new(),
                "package sample; public class StaticChild extends Base { public static int value; }"
                    .into(),
            ),
        ];
        let stubs = crate::jvm::java_stub::stub_classes(
            &sources,
            crate::jvm::java_stub::StubMode::Strict,
            &|candidate| candidate == "java/lang/Object",
        )
        .expect("field-hiding stubs");
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
        classpath.set_stub_overlay(stubs);
        let libraries = super::JvmLibraries::new(classpath);
        let resolver = crate::symbol_resolver::SymbolResolver::new(&libraries);

        assert_eq!(
            resolver
                .member_property_type(Ty::obj("sample/Base"), "value")
                .map(|(_, ty, _, _)| ty),
            Some(Ty::Int)
        );
        for child in ["sample/PrivateChild", "sample/StaticChild"] {
            assert!(
                resolver
                    .member_property_type(Ty::obj(child), "value")
                    .is_none(),
                "{child}.value must not expose the hidden Base.value"
            );
        }
    }

    #[test]
    fn shared_field_walk_specializes_applied_receivers_and_erases_raw_ones() {
        let sources = [
            (
                String::new(),
                "package sample; public class Holder<T extends CharSequence> { public T value; public java.util.List<T> values; }"
                    .into(),
            ),
        ];
        let stubs = crate::jvm::java_stub::stub_classes(
            &sources,
            crate::jvm::java_stub::StubMode::Strict,
            &|candidate| candidate.starts_with("java/"),
        )
        .expect("generic field stubs");
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
        classpath.set_stub_overlay(stubs);
        let libraries = super::JvmLibraries::new(classpath);
        let shape = libraries
            .resolve_type_name(type_name("sample/Holder"))
            .expect("generic holder shape");
        assert_eq!(shape.type_params, ["T"]);
        assert!(
            matches!(shape.fields.get(1), Some(field) if matches!(field.ty, Ty::Obj(_, arguments) if matches!(arguments, [Ty::TyParam("T", _)]))),
            "field signature must retain its declaration variable: {:?}",
            shape
                .fields
                .iter()
                .map(|field| field.ty)
                .collect::<Vec<_>>()
        );
        let resolver = crate::symbol_resolver::SymbolResolver::new(&libraries);

        assert_eq!(
            resolver
                .member_property_type(Ty::obj_args("sample/Holder", &[Ty::String]), "values")
                .map(|(_, ty, _, _)| ty),
            Some(Ty::obj_args("kotlin/collections/List", &[Ty::String])),
            "the field type must be substituted through the receiver hierarchy"
        );
        let raw_values = resolver
            .member_property_type(Ty::obj("sample/Holder"), "values")
            .expect("raw public field")
            .1;
        assert!(
            matches!(raw_values, Ty::Obj(_, arguments) if arguments.is_empty()),
            "an unbound field parameter must use the erased declaration type, got {raw_values:?}"
        );
    }

    #[test]
    fn source_names_normalize_raw_collections_without_collapsing_mutability() {
        let libs = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(Vec::new()),
        ));

        assert_eq!(
            libs.canonical_source_type_name(type_name("java/util/List")),
            type_name("kotlin/collections/List")
        );
        assert_eq!(
            libs.canonical_source_type_name(type_name("kotlin/collections/MutableList")),
            type_name("kotlin/collections/MutableList")
        );
    }

    #[test]
    fn metadata_collection_projection_changes_only_the_matching_outer_classifier() {
        let element = Ty::obj("fixture/Element");
        let recovered = Ty::obj_args("kotlin/collections/List", &[element]);

        assert_eq!(
            overlay_metadata_collection_names(recovered, Ty::obj("kotlin/collections/MutableList"),),
            Ty::obj_args("kotlin/collections/MutableList", &[element]),
            "metadata owns mutability while the recovered signature keeps its generic argument",
        );
        assert_eq!(
            overlay_metadata_collection_names(recovered, Ty::obj("kotlin/collections/MutableSet")),
            recovered,
            "a classifier from another erased collection family must not replace the return",
        );
        assert_eq!(
            overlay_metadata_collection_names(recovered, Ty::obj("fixture/MutableList")),
            recovered,
            "a similarly named application class must not trigger the Kotlin collection rule",
        );
    }

    #[test]
    fn metadata_collection_projection_descends_into_matching_type_arguments() {
        let inner_base = Ty::obj_args("kotlin/collections/Set", &[Ty::String]);
        let recovered = Ty::obj_args("kotlin/collections/List", &[inner_base]);
        let meta = Ty::obj_args(
            "kotlin/collections/MutableList",
            &[Ty::obj_args("kotlin/collections/MutableSet", &[Ty::String])],
        );

        assert_eq!(
            overlay_metadata_collection_names(recovered, meta),
            Ty::obj_args(
                "kotlin/collections/MutableList",
                &[Ty::obj_args("kotlin/collections/MutableSet", &[Ty::String])],
            ),
            "each nesting level recovers its declared mutability under the same guard",
        );
    }

    #[test]
    fn method_descriptor_parser_rejects_partial_or_malformed_input() {
        assert_eq!(
            parse_method_desc("(ILjava/lang/String;)[B"),
            Some((vec![Ty::Int, Ty::String], Ty::array(Ty::Byte),))
        );
        for invalid in [
            "",
            "I)V",
            "(I",
            "(I)",
            "(V)V",
            "([V)V",
            "(Q)V",
            "(Ljava/lang/String)V",
            "(Ljava/lang/String;;)V",
            "()Ljava/lang/String;I",
        ] {
            assert_eq!(
                parse_method_desc(invalid),
                None,
                "accepted malformed descriptor {invalid}"
            );
        }
    }

    #[test]
    fn method_generic_signature_retains_interformal_bounds() {
        let signature = parse_method_gsig(
            "<T:Ljava/lang/Object;R:Ljava/lang/Object;C::Ljava/util/Collection<-TR;>;>\
             ([TT;TC;Lkotlin/jvm/functions/Function1<-TT;+TR;>;)TC;",
        )
        .expect("generic signature");
        assert_eq!(signature.formals, ["T", "R", "C"]);
        assert_eq!(
            signature.formal_bounds[2],
            [Ty::obj_args(
                "java/util/Collection",
                &[Ty::ty_param("R", Ty::obj("kotlin/Any"))],
            )]
        );
    }

    #[test]
    fn raw_signature_marks_only_outer_method_formal_returns_flexible() {
        // The policy belongs to the exact declaration shape: a method-owned outer formal denotes an
        // unannotated reference result, while a nested occurrence already has a reference container and
        // an owner formal is not rebound by the method call. This prevents broad classpath provenance from
        // changing unrelated generic returns.
        let direct = parse_method_gsig("<T:Ljava/lang/Object;>(TT;)TT;").expect("direct formal");
        assert_eq!(direct.return_policy, GenericReturnPolicy::FlexibleReference);

        let nested = parse_method_gsig("<T:Ljava/lang/Object;>(TT;)Ljava/util/List<TT;>;")
            .expect("nested formal");
        assert_eq!(nested.return_policy, GenericReturnPolicy::Exact);

        let owner = parse_method_gsig("(TT;)TT;").expect("owner formal");
        assert_eq!(owner.return_policy, GenericReturnPolicy::Exact);
    }

    #[test]
    fn ordinary_generic_signatures_retain_projection_and_inner_class_parsing() {
        let method =
            parse_method_gsig("(Ljava/util/List<+Ljava/lang/String;>;)Ljava/util/List<*>;")
                .expect("method signature");
        assert_eq!(
            method.params,
            [Ty::obj_args("java/util/List", &[Ty::String])]
        );
        assert_eq!(
            method.ret,
            Ty::obj_args("java/util/List", &[Ty::obj("kotlin/Any")])
        );

        let (_, supertypes) = parse_class_gsig(
            "Ljava/lang/Object;\
             Lfixture/Outer<Ljava/lang/Integer;>.Inner<Ljava/lang/Long;>;",
        )
        .expect("class signature");
        assert_eq!(
            supertypes[1],
            Ty::obj_args("fixture/Outer$Inner", &[Ty::obj("java/lang/Long")])
        );
    }

    #[test]
    fn class_generic_signature_canonicalizes_unsigned_arguments() {
        let (_, supertypes) =
            parse_class_gsig("Ljava/lang/Object;Ljava/lang/Iterable<Lkotlin/UInt;>;")
                .expect("class signature");
        assert_eq!(
            supertypes[1],
            Ty::obj_args("java/lang/Iterable", &[Ty::UInt])
        );
    }

    #[test]
    fn function_generic_signature_canonicalizes_bottom_and_unit_returns() {
        let unit = parse_method_gsig("(Lkotlin/jvm/functions/Function0<Lkotlin/Unit;>;)V")
            .expect("Unit function signature");
        assert_eq!(unit.params, [Ty::fun(Vec::new(), Ty::Unit)]);

        let bottom = parse_method_gsig("(Lkotlin/jvm/functions/Function0<Lkotlin/Nothing;>;)V")
            .expect("Nothing function signature");
        assert_eq!(bottom.params, [Ty::fun(Vec::new(), Ty::Nothing)]);
    }

    #[test]
    fn field_generic_signature_must_be_complete_and_concrete() {
        assert_eq!(
            parse_concrete_field_gsig(
                "Ljava/util/List<Lkotlin/collections/Set<Ljava/lang/Integer;>;>;",
                "Ljava/util/List;",
            ),
            Some(Ty::obj_args(
                "kotlin/collections/List",
                &[Ty::obj_args("kotlin/collections/Set", &[Ty::Int])],
            ))
        );
        assert_eq!(
            parse_concrete_field_gsig(
                "Ljava/util/List<Ljava/lang/String;>;junk",
                "Ljava/util/List;",
            ),
            None
        );
        assert_eq!(
            parse_concrete_field_gsig("Ljava/util/List<Ljava/lang/String;", "Ljava/util/List;",),
            None
        );
    }

    #[test]
    fn field_generic_signature_rejects_wildcards_and_projections() {
        assert_eq!(
            parse_field_gsig(
                "TT;",
                "Ljava/lang/CharSequence;",
                Some("<T::Ljava/lang/CharSequence;>Ljava/lang/Object;"),
            )
            .map(|(ty, _)| ty),
            Some(Ty::ty_param("T", Ty::obj("kotlin/Any"))),
            "a field type variable must erase through its declaring-class bound"
        );
        for signature in [
            "Ljava/util/List<*>;",
            "Ljava/util/List<+Ljava/lang/String;>;",
            "Ljava/util/List<-Ljava/lang/String;>;",
            "Ljava/util/List<Ljava/util/Set<+Ljava/lang/String;>;>;",
            "Lkotlin/jvm/functions/Function1<-Ljava/lang/String;+Ljava/lang/Long;>;",
        ] {
            let descriptor = format!("L{};", &signature[1..signature.find('<').unwrap()]);
            assert_eq!(
                parse_concrete_field_gsig(signature, &descriptor),
                None,
                "accepted lossy field signature {signature}"
            );
        }
    }

    #[test]
    fn field_generic_signature_recurses_through_function_types() {
        assert_eq!(
            parse_concrete_field_gsig(
                "Lkotlin/jvm/functions/Function1<\
                 Ljava/util/List<Ljava/lang/Integer;>;\
                 Ljava/util/Set<Ljava/lang/Long;>;>;",
                "Lkotlin/jvm/functions/Function1;",
            ),
            Some(Ty::fun(
                vec![Ty::obj_args("kotlin/collections/List", &[Ty::Int])],
                Ty::obj_args("kotlin/collections/Set", &[Ty::Long]),
            ))
        );
        assert_eq!(
            parse_concrete_field_gsig(
                "Lkotlin/jvm/functions/Function1<\
                 Ljava/util/List<TT;>;\
                 Ljava/util/Set<Ljava/lang/Long;>;>;",
                "Lkotlin/jvm/functions/Function1;",
            ),
            None
        );
    }

    #[test]
    fn field_generic_signature_requires_unparameterized_inner_class_owners() {
        assert_eq!(
            parse_concrete_field_gsig(
                "Lfixture/Outer<Ljava/lang/Integer;>.\
                 Inner<Ljava/lang/Long;>.\
                 Leaf<Ljava/lang/Double;>;",
                "Lfixture/Outer$Inner$Leaf;",
            ),
            None
        );
        assert_eq!(
            parse_concrete_field_gsig(
                "Lfixture/Outer.Inner<Ljava/lang/Long;>.Leaf<Ljava/lang/Double;>;",
                "Lfixture/Outer$Inner$Leaf;",
            ),
            None
        );
        assert_eq!(
            parse_concrete_field_gsig(
                "Lfixture/Outer.Inner.Leaf<Ljava/lang/Double;>;",
                "Lfixture/Outer$Inner$Leaf;",
            ),
            Some(Ty::obj_args("fixture/Outer$Inner$Leaf", &[Ty::Double]))
        );
    }

    #[test]
    fn field_generic_signature_requires_exact_function_arity() {
        assert_eq!(
            parse_concrete_field_gsig(
                "Lkotlin/jvm/functions/Function2<\
                 Ljava/lang/Integer;\
                 Ljava/lang/Long;\
                 Ljava/lang/String;>;",
                "Lkotlin/jvm/functions/Function2;",
            ),
            Some(Ty::fun(vec![Ty::Int, Ty::Long], Ty::String))
        );

        for signature in [
            "Lkotlin/jvm/functions/Function1<Ljava/lang/String;>;",
            "Lkotlin/jvm/functions/Function1<\
             Ljava/lang/Integer;Ljava/lang/Long;Ljava/lang/String;>;",
            "Lkotlin/jvm/functions/FunctionX<Ljava/lang/String;>;",
            "Lkotlin/jvm/functions/Function1Extra<Ljava/lang/String;>;",
            "Lkotlin/jvm/functions/Function+1<Ljava/lang/String;Ljava/lang/String;>;",
            "Lkotlin/jvm/functions/Function<Ljava/lang/String;>;",
        ] {
            let descriptor = format!("L{};", &signature[1..signature.find('<').unwrap()]);
            assert_eq!(
                parse_concrete_field_gsig(signature, &descriptor),
                None,
                "accepted malformed function signature {signature}"
            );
        }
    }

    #[test]
    fn field_generic_signature_requires_reference_and_matching_erasure() {
        assert_eq!(parse_concrete_field_gsig("I", "I"), None);
        assert_eq!(parse_concrete_field_gsig("V", "V"), None);
        assert_eq!(parse_concrete_field_gsig("[V", "[V"), None);
        assert_eq!(
            parse_concrete_field_gsig("[I", "[I"),
            Some(Ty::array(Ty::Int))
        );
        assert_eq!(
            parse_concrete_field_gsig("Ljava/util/List<Ljava/lang/Integer;>;", "Ljava/util/Set;",),
            None
        );
        assert_eq!(
            parse_concrete_field_gsig("Ljava/util/List<TT;>;", "Ljava/util/List;"),
            None
        );
    }

    #[test]
    fn start_coroutine_receiver_maps_to_the_function2_overload() {
        // `(suspend R.() -> T).startCoroutine(receiver, completion)` → the stdlib's
        // three-argument `ContinuationKt.startCoroutine(Function2, Object, Continuation)`.
        use crate::runtime::{RuntimeOp, TargetRuntime};
        let libs = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(Vec::new()),
        ));
        let c = libs
            .runtime_callable(RuntimeOp::StartCoroutineReceiver, Ty::Unit)
            .expect("mapped");
        assert_eq!(c.owner.render(), "kotlin/coroutines/ContinuationKt");
        assert_eq!(c.name, "startCoroutine");
        assert_eq!(
            c.descriptor,
            "(Lkotlin/jvm/functions/Function2;Ljava/lang/Object;Lkotlin/coroutines/Continuation;)V"
        );
    }

    /// Lowering asks the provider what a call LEAVES on the stack so it never parses a JVM
    /// descriptor itself. Only an object return names a class: `kotlin/UInt.box-impl` is how an
    /// unsigned value becomes a reference, and its carrier-returning siblings (`constructor-impl`,
    /// `unbox-impl`) must answer `None` or a value still in its primitive slot would read as boxed.
    #[test]
    fn descriptor_method_layout_names_only_an_object_return_class() {
        assert_eq!(
            method_layout("(I)Lkotlin/UInt;").and_then(|layout| layout.return_class),
            Some(type_name("kotlin/UInt"))
        );
        for carrier in ["(I)I", "()I", "(Lkotlin/UInt;)V", "(I)[Ljava/lang/String;"] {
            assert_eq!(
                method_layout(carrier).and_then(|layout| layout.return_class),
                None,
                "{carrier}"
            );
        }
        // A descriptor the platform cannot read is not a claim about anything.
        assert!(method_layout("nonsense").is_none());
    }
}

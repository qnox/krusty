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
    AnnotationParameterPolicy, AnnotationPositionalPolicy, CallSig, EmptySymbolSource, FnFlags,
    FnKind, FunctionInfo, FunctionSet, GenericReturnPolicy, GenericSig, InlineBodyPlan, InlineKind,
    LibConst, LibraryCallable, LibraryConst, LibraryField, LibraryMember, LibraryType, ParamList,
    PropKind, PropertyInfo, PropertySet, ReturnInfo, SemanticPlatform, Visibility,
};
use crate::runtime::{
    CountedLoopInfo, PlatformAccessor, PlatformCtor, PlatformField, PlatformRangeCtor,
    RangeConstruction, RuntimeCtor, RuntimeOp,
};
use crate::symbol_resolver::{ty_subst, ty_subst_all, ty_subst_keep_unbound};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{type_name, Ty, TypeName, TypeNameList};

#[derive(Clone, Copy, PartialEq, Eq)]
enum FunctionClassKind {
    Function,
    KFunction,
}

#[derive(Clone, Copy)]
struct FictitiousFunctionClass {
    kind: FunctionClassKind,
}

/// Kotlin's built-in function-class provider owns these declarations; they are absent from stdlib
/// classfiles and `.kotlin_builtins`. Recognition happens once at the provider boundary. Consumers
/// receive an ordinary classifier record and never inspect this spelling.
fn fictitious_function_class(internal: TypeName) -> Option<FictitiousFunctionClass> {
    let package = internal.parent()?;
    let (kind, digits) = if package == type_name("kotlin") {
        (
            FunctionClassKind::Function,
            internal.segment_ref().strip_prefix("Function")?,
        )
    } else if package == type_name("kotlin/reflect") {
        (
            FunctionClassKind::KFunction,
            internal.segment_ref().strip_prefix("KFunction")?,
        )
    } else {
        return None;
    };
    (!digits.is_empty() && digits.bytes().all(|digit| digit.is_ascii_digit()))
        .then_some(FictitiousFunctionClass { kind })
}

fn fictitious_function_class_name(fqn: &str) -> Option<TypeName> {
    let (package, name) = fqn.rsplit_once('/')?;
    let digits = match package {
        "kotlin" => name.strip_prefix("Function")?,
        "kotlin/reflect" => name.strip_prefix("KFunction")?,
        _ => return None,
    };
    (!digits.is_empty() && digits.bytes().all(|digit| digit.is_ascii_digit())).then_some(())?;
    Some(type_name(fqn))
}

pub(crate) fn is_fictitious_kfunction(internal: TypeName) -> bool {
    fictitious_function_class(internal)
        .is_some_and(|function| function.kind == FunctionClassKind::KFunction)
}

fn effective_class_access(class: &super::classreader::ClassInfo) -> u16 {
    class
        .inner_class_self()
        .map(|entry| entry.access)
        .unwrap_or(class.access)
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
/// composed with this list in the import-level builder and in the seed filter, so neither list is duplicated.
const PLATFORM_DEFAULT_IMPORT_PACKAGES: &[&str] = &["java.lang", "kotlin.jvm"];

fn java_property_name(method: &str) -> Option<String> {
    let stem = method.strip_prefix("get")?;
    if !stem.is_ascii() {
        let mut chars = stem.chars();
        let first = chars.next()?;
        if !first.is_uppercase() {
            return None;
        }
        return Some(format!("{}{}", first.to_lowercase(), chars.as_str()));
    }
    let first = stem.as_bytes().first()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    let bytes = stem.as_bytes();
    let lower = bytes.iter().position(u8::is_ascii_lowercase);
    let prefix = match lower {
        None => bytes.len(),
        Some(0) => return None,
        Some(1) => 1,
        Some(index) => index - 1,
    };
    Some(format!(
        "{}{}",
        stem[..prefix].to_ascii_lowercase(),
        &stem[prefix..]
    ))
}

/// A platform backed by a JVM classpath (dirs + jars + the JDK jimage). The classpath is shared
/// (`Rc`) with the JVM backend/emitter so the bytecode inliner reads inline-function bodies through
/// the same lazily-populated caches — all within the `jvm` module, never through the `SymbolSource`
/// abstraction.
pub struct JvmLibraries {
    cp: std::rc::Rc<Classpath>,
    builtins_customizer: JvmBuiltInsCustomizer,
    /// Classifier currently having its exact declaration map materialized. This is construction state,
    /// not a lookup fallback: recursive metadata reads observe the same immutable raw signature.
    building_types:
        std::cell::RefCell<std::collections::HashMap<TypeName, std::sync::Arc<LibraryType>>>,
}

/// JVM-only additions to Kotlin's builtin classifier model.
///
/// These declarations are absent from the common `Array` source and are not backend guesses:
/// kotlinc's `JvmBuiltInsCustomizer` adds `Cloneable`/`Serializable` as array supertypes and publishes
/// a public, covariant `clone()` declaration on every array classifier. Keeping that transformation
/// here means every consumer sees one ordinary [`LibraryType`]; resolver and lowerer need no JVM or
/// array-specific lookup path.
#[derive(Default)]
struct JvmBuiltInsCustomizer;

impl JvmBuiltInsCustomizer {
    fn classifier_name(&self, fqn: &str) -> Option<TypeName> {
        let builtin_array = fqn
            .strip_prefix("kotlin/")
            .is_some_and(|name| name == "Array" || Ty::primitive_array_element(name).is_some());
        (fqn == "kotlin/Cloneable" || builtin_array).then(|| type_name(fqn))
    }

    fn customize(&self, internal: TypeName, base: Option<LibraryType>) -> Option<LibraryType> {
        let mut classifier = if internal.matches("kotlin/Cloneable") {
            base.unwrap_or_else(Self::cloneable_classifier)
        } else if Ty::obj_name(internal).is_array() {
            base.unwrap_or_else(LibraryType::declaration_header)
        } else {
            base?
        };

        if internal.matches("kotlin/Cloneable") {
            Self::install_cloneable_clone(&mut classifier);
        }
        if Ty::obj_name(internal).is_array() {
            Self::install_array_platform_shape(internal, &mut classifier);
        }
        Some(classifier)
    }

    /// JVM realization of an intrinsic builtin companion. The semantic companion identity comes
    /// from `.kotlin_builtins`; this mapping supplies only the platform class that stores its value.
    fn intrinsic_companion_realization(
        &self,
        owner: TypeName,
        companion: TypeName,
    ) -> Option<TypeName> {
        (owner.parent() == Some(type_name("kotlin")) && companion.nested_owner() == Some(owner))
            .then(|| {
                let segment = format!("{}CompanionObject", owner.segment_ref());
                crate::types::type_name_child(type_name("kotlin/jvm/internal"), &segment)
            })
    }

    fn cloneable_classifier() -> LibraryType {
        let mut supertypes = TypeNameList::new();
        supertypes.push("kotlin/Any");
        builtin_library_type(
            crate::libraries::TypeKind::Interface,
            crate::libraries::ClassifierAccess::Public,
            false,
            supertypes,
            Vec::new(),
            Vec::new(),
            BuiltinGenericShape {
                type_params: Vec::new(),
                type_param_variances: Vec::new(),
                supertype_templates: vec![Ty::obj("kotlin/Any")],
            },
        )
    }

    fn clone_member(ret: Ty, visibility: Visibility) -> LibraryMember {
        let physical_ret = Ty::obj("kotlin/Any");
        let mut clone = LibraryMember::new(
            "clone".to_string(),
            Vec::new(),
            ret,
            "()Ljava/lang/Object;".to_string(),
        );
        // `Array.clone()` is a source-level JVM builtin, physically realized by the protected method
        // on `Object`. The selected callable carries that complete realization into emission.
        clone.owner = Some(crate::types::wk::java_object());
        clone.physical_ret = physical_ret;
        clone.visibility = visibility;
        clone
    }

    fn install_cloneable_clone(classifier: &mut LibraryType) {
        if let Some(clone) = classifier
            .members
            .iter_mut()
            .find(|member| member.name == "clone" && member.params.is_empty())
        {
            *clone = Self::clone_member(Ty::obj("kotlin/Any"), Visibility::Protected);
        } else {
            classifier.members.push(Self::clone_member(
                Ty::obj("kotlin/Any"),
                Visibility::Protected,
            ));
        }
    }

    fn install_array_platform_shape(internal: TypeName, classifier: &mut LibraryType) {
        for supertype in ["kotlin/Cloneable", "java/io/Serializable"] {
            let name = type_name(supertype);
            if !classifier.supertypes.contains_name(name) {
                classifier.supertypes.push_name(name);
            }
            if !classifier
                .supertype_templates
                .iter()
                .any(|ty| ty.obj_internal() == Some(name))
            {
                classifier.supertype_templates.push(Ty::obj_name(name));
            }
        }

        let arguments = classifier
            .type_params
            .iter()
            .map(|formal| Ty::ty_param(formal, Ty::obj("kotlin/Any")))
            .collect::<Vec<_>>();
        let ret = Ty::obj_args_name(internal, &arguments);
        let replacement = Self::clone_member(ret, Visibility::Public);
        if let Some(clone) = classifier
            .members
            .iter_mut()
            .find(|member| member.name == "clone" && member.params.is_empty())
        {
            *clone = replacement;
        } else {
            classifier.members.push(replacement);
        }
    }
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

fn java_type_nullability(ty: Ty, nullability: Option<JavaNullability>) -> Ty {
    if !ty.is_reference() {
        return ty;
    }
    // With no type-use qualifier, Java's flexibility applies recursively: `String[]` exposes
    // `Array<String!>!`, and `List<String>` exposes `List<String!>!`. Qualifying only the outer
    // classifier incorrectly rejects `null` as an expanded Java `String...` element.
    let ty = match ty.non_null() {
        Ty::Obj(name, arguments) if !arguments.is_empty() => {
            let arguments = arguments
                .iter()
                .map(|argument| java_type_argument_nullability(*argument))
                .collect::<Vec<_>>();
            let classifier = Ty::obj_args_name(name, &arguments);
            // Java arrays are covariant. Normalize that declaration fact at the provider boundary
            // as Kotlin's `Array<(out) T!>!`; common assignability then needs no Java/classpath path.
            if classifier.is_reference_array()
                && matches!(arguments.as_slice(), [argument] if argument.projection_inner().is_none())
            {
                Ty::obj_args_name(name, &[Ty::out_projection(arguments[0])])
            } else {
                classifier
            }
        }
        _ => ty,
    };
    // Java wrapper classes are Kotlin primitive types with Java's flexible/nullability qualifier.
    // Keep the physical wrapper in the JVM descriptor; the semantic signature must be `Int!`, not
    // `java.lang.Integer!`, so core type checking needs no representation-specific compatibility rule.
    let ty = ty
        .non_null()
        .obj_internal()
        .and_then(super::jvm_class_map::wrapper_to_kotlin_prim_name)
        .map(super::classpath::kotlin_name_to_ty)
        .unwrap_or(ty);
    match nullability {
        Some(JavaNullability::NotNull) => ty.non_null(),
        Some(JavaNullability::Nullable) => Ty::nullable(ty),
        None => Ty::platform_nullable(ty),
    }
}

/// Apply Java's unqualified flexibility inside a generic argument without discarding its semantic
/// wrapper. A declaration variable stays a variable whose bound is flexible; use-site variance stays
/// a projection whose interior is flexible.
fn java_type_argument_nullability(ty: Ty) -> Ty {
    match ty {
        Ty::TyParam(name, bound) => Ty::ty_param(name, java_type_nullability(*bound, None)),
        Ty::InProjection(inner) => Ty::in_projection(java_type_argument_nullability(*inner)),
        Ty::OutProjection(inner) => Ty::out_projection(java_type_argument_nullability(*inner)),
        _ => java_type_nullability(ty, None),
    }
}

impl JvmLibraries {
    /// Turn a JVM generic-signature node into a Kotlin function type only when the referenced
    /// classifier declaration says that it is a function interface. The raw descriptor/name does
    /// not carry the callable shape; `callable_signature` is decoded from the interface's `invoke`
    /// declaration and its type-parameter metadata.
    fn semanticize_jvm_type(&self, ty: Ty) -> Ty {
        match ty {
            Ty::Obj(internal, args) => {
                let args = args
                    .iter()
                    .map(|arg| self.semanticize_jvm_type(*arg))
                    .collect::<Vec<_>>();
                let nominal = Ty::obj_args_name(internal, &args);
                let Some(class) = self.cp.find_name(internal) else {
                    return nominal;
                };
                if !class.interfaces.contains_name(type_name("kotlin/Function")) {
                    return nominal;
                }
                let Some(classifier) = self.classifier_record(internal) else {
                    return nominal;
                };
                if classifier.type_params.len() != args.len() {
                    return nominal;
                }
                let Some(signature) = classifier.callable_signature else {
                    return nominal;
                };
                let Some(arguments) = args
                    .iter()
                    .zip(classifier.type_param_variances())
                    .map(|(argument, variance)| match (argument, variance) {
                        (Ty::InProjection(inner), crate::types::TypeVariance::In)
                        | (Ty::OutProjection(inner), crate::types::TypeVariance::Out) => {
                            Some(gsig_unbox_wrapper(**inner))
                        }
                        (Ty::InProjection(_) | Ty::OutProjection(_), _) => None,
                        (argument, _) => Some(gsig_unbox_wrapper(*argument)),
                    })
                    .collect::<Option<Vec<_>>>()
                else {
                    return nominal;
                };
                let bindings = classifier
                    .type_params
                    .iter()
                    .cloned()
                    .zip(arguments)
                    .collect::<std::collections::HashMap<_, _>>();
                ty_subst_keep_unbound(signature, &bindings)
            }
            Ty::Fun(signature) => Ty::fun_with_shape(
                signature
                    .params
                    .iter()
                    .map(|param| self.semanticize_jvm_type(*param))
                    .collect(),
                self.semanticize_jvm_type(signature.ret),
                signature.context_count,
                signature.has_receiver,
                signature.suspend,
            ),
            Ty::Nullable(inner) => Ty::nullable(self.semanticize_jvm_type(*inner)),
            Ty::PlatformNullable(inner) => Ty::platform_nullable(self.semanticize_jvm_type(*inner)),
            Ty::InProjection(inner) => Ty::in_projection(self.semanticize_jvm_type(*inner)),
            Ty::OutProjection(inner) => Ty::out_projection(self.semanticize_jvm_type(*inner)),
            Ty::TyParam(name, bound) => Ty::ty_param(name, self.semanticize_jvm_type(*bound)),
            _ => ty,
        }
    }

    fn semanticize_jvm_generic_sig(&self, mut signature: GenericSig) -> GenericSig {
        for bounds in &mut signature.formal_bounds {
            for bound in bounds {
                *bound = self.semanticize_jvm_type(*bound);
            }
        }
        signature.receiver = signature
            .receiver
            .map(|receiver| self.semanticize_jvm_type(receiver));
        for parameter in &mut signature.params {
            *parameter = self.semanticize_jvm_type(*parameter);
        }
        signature.ret = self.semanticize_jvm_type(signature.ret);
        signature
    }

    fn member_scope_names(
        &self,
        internal: TypeName,
        classifier: &LibraryType,
    ) -> std::collections::HashSet<String> {
        let mut names = std::collections::HashSet::new();
        for member in &classifier.members {
            names.insert(member.name.clone());
            if let Some(physical) = &member.physical_name {
                names.insert(physical.clone());
            }
            if let Some(property) = java_property_name(&member.name) {
                names.insert(property);
            }
        }
        for rename in self.mapped_collection_function_renames(internal) {
            names.insert(rename.source_name);
        }
        if let Some(class) = self.cp.find_name(internal) {
            names.extend(
                metadata::class_properties(&class)
                    .iter()
                    .map(|property| property.name.clone()),
            );
        }
        names.extend(self.cp.builtin_member_property_names_name(internal));
        names
    }

    /// The TOP-LEVEL (receiver-less) function overloads of `name` declared in package `pkg` —
    /// `listOf`/`run`/`println`/… each with its inline/`@InlineOnly` flags and logical
    /// (continuation-stripped) suspend signature. The building block `symbols` uses so a
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
                &|name| {
                    self.classifier_record(name)
                        .and_then(|t| t.value_underlying)
                },
            );
            if meta.deprecated_hidden {
                // `@Deprecated(level = HIDDEN)`: binary-compatibility-only, kotlinc removes it
                // from the candidate set entirely.
                continue;
            }
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
            let physical_params = params.clone();
            if let Some(declared) = meta
                .declared_params
                .as_ref()
                .filter(|declared| declared.len() == params.len())
            {
                params.clone_from(declared);
            }
            let inline = meta.is_inline;
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
            let generic_sig_for_callable = meta.generic_sig.clone().or_else(|| {
                self.callable_generic_sig(
                    c.owner,
                    &c.name,
                    &c.descriptor,
                    c.signature.as_deref(),
                    false,
                )
            });
            let mut callable = LibraryCallable {
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
            callable.physical_params = physical_params;
            if !is_default && call_sig.param_defaults.iter().any(|default| *default) {
                callable.default_realization =
                    self.top_level_default_realization(&callable).map(Box::new);
            }
            callable.inline_body_plan = self.inline_body_plan(&callable).map(Box::new);
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
                visibility: meta
                    .visibility
                    .unwrap_or_else(|| Visibility::from_public(c.public)),
                overload_rank: descriptor_narrowing(&c.descriptor) as u32,
                generic_sig,
                call_sig,
                context_count,
                flags: FnFlags {
                    inline: inline_kind,
                    reified: meta.has_reified_type_params,
                    suspend,
                    operator: meta.is_operator,
                    infix: meta.is_infix,
                    is_abstract: false,
                    low_priority: meta.low_priority,
                },
                ..FunctionInfo::plain(kind, None, callable)
            });
        }
        for builtin in self.cp.builtin_package_functions(pkg, name) {
            if overloads.iter().any(|candidate| {
                candidate.generic_sig.as_ref().is_some_and(|signature| {
                    signature.receiver == builtin.generic_sig.receiver
                        && signature.params == builtin.generic_sig.params
                        && signature.ret == builtin.generic_sig.ret
                })
            }) {
                continue;
            }
            let inline = InlineKind::from_flags(builtin.is_inline, builtin.is_inline);
            let mut callable = LibraryCallable::library(
                pkg,
                name.to_string(),
                builtin.params.clone(),
                builtin.ret,
                builtin.ret,
                String::new(),
            );
            callable.inline = inline;
            callable.suspend = builtin.is_suspend;
            callable.source_receiver = builtin.generic_sig.receiver;
            callable.generic_sig = Some(Box::new(builtin.generic_sig.clone()));
            let kind = if builtin.generic_sig.receiver.is_some() {
                FnKind::Extension
            } else {
                FnKind::TopLevel
            };
            let mut function = FunctionInfo::plain(kind, builtin.generic_sig.receiver, callable);
            function.generic_sig = Some(builtin.generic_sig);
            function.call_sig = CallSig::metadata_member(
                function
                    .generic_sig
                    .as_ref()
                    .map_or(0, |signature| signature.params.len()),
                builtin.param_names,
                builtin.param_defaults,
                builtin.vararg,
            );
            function.context_count = builtin.context_count;
            function.callable.context_count = builtin.context_count;
            function.callable.compiler_intrinsic = match (
                pkg.matches("kotlin"),
                name,
                function.kind,
                function
                    .generic_sig
                    .as_ref()
                    .and_then(|signature| signature.receiver),
            ) {
                (true, "plus", FnKind::Extension, Some(receiver))
                    if receiver == Ty::nullable(Ty::String) =>
                {
                    Some(crate::libraries::CompilerIntrinsic::StringPlus)
                }
                (true, "toString", FnKind::Extension, Some(receiver))
                    if receiver == Ty::nullable(Ty::obj("kotlin/Any")) =>
                {
                    Some(crate::libraries::CompilerIntrinsic::NullableAnyToString)
                }
                _ => None,
            };
            function.visibility = builtin.visibility;
            function.flags = FnFlags {
                inline,
                reified: builtin.has_reified_type_params,
                suspend: builtin.is_suspend,
                operator: builtin.is_operator,
                infix: builtin.is_infix,
                is_abstract: false,
                low_priority: false,
            };
            overloads.push(function);
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
                let classifier = self.classifier_record(candidate)?;
                if let Some((outer, simple)) = name.rsplit_once('$') {
                    let outer = type_name(outer);
                    if let Some((holder_name, companion_type)) = self
                        .classifier_record(outer)
                        .and_then(|outer_type| outer_type.companion_object.clone())
                    {
                        // A companion is also an object, but its singleton field belongs to the
                        // enclosing class. Test that declaration relationship before the ordinary
                        // object storage shape.
                        if companion_type == candidate && holder_name == simple {
                            return Some((candidate, field(outer, &holder_name)));
                        }
                    }
                }
                if classifier.is_object() {
                    return Some((candidate, field(candidate, "INSTANCE")));
                }
                None
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

    /// Callables named `name` declared inside the `object` / `companion object` `owner`, normalized
    /// into the import scope with dispatch on that object's singleton.
    ///
    /// `import Duration.Companion.minutes` puts `minutes` in scope as an extension on `Int`, with
    /// `Duration.Companion` supplying the dispatch receiver. Everything about selection is then ordinary
    /// extension resolution — only the EMIT differs, and that difference rides on the callable as
    /// [`LibraryCallable::singleton_dispatch`]. Nothing is contributed when `owner` is not an object
    /// (the overwhelmingly common case: the parent really is a package).
    fn object_member_callables(
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
            if function.kotlin_name != name || !function.is_public() || function.deprecated_hidden()
            {
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
            if function.is_extension() && params.is_empty() {
                continue; // an extension's first parameter IS its receiver
            }
            let generic_sig = function.generic_sig.clone();
            let receiver = function.is_extension().then(|| {
                generic_sig
                    .as_ref()
                    .and_then(|gsig| gsig.receiver)
                    .or_else(|| function.receiver_class.map(kotlin_type_name_to_ty))
                    .unwrap_or(params[0])
            });
            let ret = metadata_return_info(function.ret_class, function.ret_nullable())
                .apply(physical_ret);
            let callable = LibraryCallable {
                suspend: function.is_suspend(),
                inline: function_inline,
                context_count: function.context_count(),
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
                context_count: function.context_count(),
                call_sig: function.member_call_sig(),
                flags: FnFlags {
                    inline: InlineKind::None,
                    reified: function.has_reified_type_params(),
                    suspend: function.is_suspend(),
                    operator: function.is_operator(),
                    infix: function.is_infix(),
                    is_abstract: false,
                    low_priority: function.low_priority(),
                },
                ..FunctionInfo::plain(
                    if receiver.is_some() {
                        FnKind::Extension
                    } else {
                        // In an import scope, an ordinary object member has no source-level receiver:
                        // the imported singleton supplies dispatch. It therefore participates in the
                        // same receiver-less candidate family as package functions, while the selected
                        // callable's `singleton_dispatch` preserves its instance realization.
                        FnKind::TopLevel
                    },
                    receiver,
                    callable,
                )
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
                name: property.name.clone(),
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
                context_param_names: Vec::new(),
                getter: accessor(
                    &getter_sig.name,
                    &getter_sig.desc,
                    getter_params,
                    property_ty,
                    getter_ret,
                    getter_inline,
                ),
                setter,
                setter_visibility: property.visibility,
                is_const: property.is_const,
                visibility: property.visibility,
                owner,
                receiver_rank: 0,
                source_key: None,
                source_member: None,
            });
        }
    }

    pub fn new(cp: std::rc::Rc<Classpath>) -> JvmLibraries {
        JvmLibraries {
            cp,
            builtins_customizer: JvmBuiltInsCustomizer,
            building_types: Default::default(),
        }
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

    fn constants_for_class(
        &self,
        semantic: TypeName,
        ci: &crate::jvm::classreader::ClassInfo,
    ) -> std::collections::HashMap<String, LibraryConst> {
        if let Some(owner) = semantic.nested_owner() {
            if self
                .cp
                .builtin_companion_object(owner)
                .is_some_and(|(_, companion)| companion == semantic)
            {
                return Self::const_fields(&ci.fields, |field| {
                    Some(field_desc_to_ty(&field.descriptor))
                });
            }
            if let Some(outer) = self.cp.find_name(owner) {
                let is_companion = super::metadata::class_companion_name(&outer)
                    .and_then(|name| crate::types::existing_type_name_nested_child(owner, &name))
                    == Some(semantic);
                if is_companion {
                    return self.metadata_static_companion_consts_for_class(&outer);
                }
            }
        }
        let declared: std::collections::HashMap<_, _> = super::metadata::class_properties(ci)
            .iter()
            .filter(|property| property.is_const)
            .filter_map(|property| {
                property
                    .ret_class
                    .map(|ty| (property.name.as_str(), kotlin_type_name_to_ty(ty)))
            })
            .collect();
        Self::const_fields(&ci.fields, |field| {
            declared.get(field.name.as_str()).copied()
        })
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
        q.push_back(type_name(internal));
        while let Some(cur) = q.pop_front() {
            if !seen.insert(cur) {
                continue;
            }
            let t = <Self as SymbolSource>::classifier(self, cur)?;
            if let Some(m) = t.members.iter().find(|m| m.name.starts_with(prefix)) {
                return Some(PlatformAccessor {
                    name: m.name.clone(),
                    descriptor: m.descriptor.clone(),
                });
            }
            q.extend(t.supertypes.iter_ids());
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
                    self.classifier_record(name)
                        .and_then(|t| t.value_underlying)
                })
        {
            // Metadata DESCRIBES this class's function — it is the authoritative signature and there is NO
            // fallback to the JVM `Signature`. A failure to align/decode here is a bug to fix in the reader.
            return gsig;
        }
        // No `@Metadata` FUNCTION for the name — the JVM `Signature` is the only source. Its extension
        // receiver is the leading value parameter; move it to the `receiver` ATTRIBUTE so the signature has
        // the same shape as a metadata one (consumers bind the receiver separately, not as a value param).
        let gsig = self.semanticize_jvm_generic_sig(jvm_sig.and_then(parse_method_gsig)?);
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
                Some(ci) => ci
                    .signature
                    .as_deref()
                    .and_then(parse_class_gsig)
                    .map(|(formals, _, supertypes)| (formals, supertypes))
                    .unzip(),
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
        crate::trace_compiler!(
            "resolve",
            "classpath SAM owner={internal} interface={} metadata={} fun_interface={} methods={:?}",
            ci.is_interface(),
            ci.meta.is_present(),
            ci.meta.is_fun_interface,
            ci.methods
                .iter()
                .map(|method| (&method.name, method.access))
                .collect::<Vec<_>>(),
        );
        if !ci.is_interface() {
            return None;
        }
        // Java SAM conversion is structural. Kotlin deliberately requires `fun interface`, whose
        // declaration bit is carried by metadata; an ordinary Kotlin interface with one abstract
        // method is not a SAM target.
        if ci.meta.is_present() && !ci.meta.is_fun_interface {
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
            member.generic_sig = m
                .signature
                .as_deref()
                .and_then(parse_method_gsig)
                .map(|signature| self.semanticize_jvm_generic_sig(signature));
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
            .filter(|m| m.is_public() && !m.deprecated_hidden())
            .filter_map(|m| {
                let descriptor = m.jvm_desc?;
                let (physical_params, _) = parse_method_desc(descriptor)?;
                let generic = m.generic_sig.clone();
                let params = generic.as_ref().map_or_else(
                    || physical_params.clone(),
                    |signature| signature.params.clone(),
                );
                let ret = generic
                    .as_ref()
                    .map_or_else(|| Ty::obj(&internal), |signature| signature.ret);
                let mut callable = LibraryCallable::library(
                    type_name(&companion_internal),
                    m.jvm_name.clone(),
                    physical_params,
                    ret,
                    Ty::obj("kotlin/Any"),
                    descriptor,
                );
                callable.params = params;
                callable.inline = InlineKind::MustInline;
                callable.generic_sig = generic.map(Box::new);
                Some(crate::libraries::CompanionFn {
                    class_internal: type_name(&internal),
                    companion_internal: type_name(&companion_internal),
                    companion_field: companion_field.clone(),
                    callable,
                })
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
        if let Some(function_class) = fictitious_function_class(internal_name) {
            if function_class.kind == FunctionClassKind::Function {
                let runtime = crate::types::type_name_child(
                    type_name("kotlin/jvm/functions"),
                    internal_name.segment_ref(),
                );
                return self
                    .classifier_record(runtime)
                    .map(|classifier| (*classifier).clone());
            }
            let digits = internal_name.segment_ref().strip_prefix("KFunction")?;
            let runtime_name = format!("Function{digits}");
            let runtime =
                crate::types::type_name_child(type_name("kotlin/jvm/functions"), &runtime_name);
            let runtime_shape = self.classifier_record(runtime)?;
            let type_params = runtime_shape.type_params.clone();
            let arguments = type_params
                .iter()
                .enumerate()
                .map(|(index, formal)| {
                    Ty::ty_param(
                        formal,
                        runtime_shape
                            .type_param_bounds
                            .get(index)
                            .and_then(|bounds| bounds.first())
                            .copied()
                            .unwrap_or_else(|| Ty::obj("kotlin/Any")),
                    )
                })
                .collect::<Vec<_>>();
            let mut shape =
                (*self.classifier_record(type_name(crate::types::KFUNCTION_INTERNAL))?).clone();
            let function_classifier =
                crate::types::type_name_child(type_name("kotlin"), &runtime_name);
            shape.supertypes = vec![
                type_name(crate::types::KFUNCTION_INTERNAL),
                function_classifier,
            ]
            .into();
            shape.type_parameters = crate::types::TypeParameters::invariant(
                type_params.clone(),
                vec![Vec::new(); type_params.len()],
            );
            shape.supertype_templates = vec![
                Ty::obj_args(
                    crate::types::KFUNCTION_INTERNAL,
                    arguments
                        .last()
                        .into_iter()
                        .copied()
                        .collect::<Vec<_>>()
                        .as_slice(),
                ),
                Ty::obj_args_name(function_classifier, &arguments),
            ];
            return Some(shape);
        }
        {
            let intrinsic_companion = internal_name.nested_owner().and_then(|owner| {
                self.cp
                    .builtin_companion_object(owner)
                    .filter(|(_, companion)| *companion == internal_name)
                    .and_then(|(_, companion)| {
                        self.builtins_customizer
                            .intrinsic_companion_realization(owner, companion)
                    })
            });
            let physical = intrinsic_companion.unwrap_or(internal_name);
            let ci = match self.cp.find_name(physical) {
                Some(ci) => ci,
                None => {
                    // A builtin-only classifier is valid only when there is no physical class to
                    // enrich it. Arrays have descriptor realizations rather than class files, and
                    // companions such as `Double.Companion` use a different runtime class. When a
                    // physical Kotlin class DOES exist, its class metadata owns exact JVM callable
                    // signatures; returning the builtins declaration first would discard those
                    // signatures (`IntIterator.next(): Integer`) and synthesize an invalid one from
                    // the logical return type (`next(): int`).
                    if let Some(classifier) = self.builtin_library_type(internal_name) {
                        return Some(classifier);
                    }
                    if physical == internal_name {
                        return None;
                    }
                    return mapped_builtin_signature(&internal_name.render());
                }
            };
            let internal = &internal_name.render();
            let mut constructors = Vec::new();
            let mut members = Vec::new();
            let mut companion = Vec::new();
            let mut enum_entries_accessor = None;
            let has_kotlin_metadata = ci.meta.is_present();
            let uses_java_type_semantics = !has_kotlin_metadata;
            let constructor_outer = ci.inner_class_self().and_then(|entry| {
                (entry.access & ACC_STATIC == 0)
                    .then(|| entry.outer.as_deref().map(type_name))
                    .flatten()
            });
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
            // The class's `@Metadata` CONSTRUCTOR records — the only place a constructor parameter's
            // source-level shape survives (a receiver function type erases to `FunctionN` in both the
            // descriptor and the `Signature`).
            let ctor_param_lists = metadata::class_constructor_params(&ci);
            let meta_constructors = metadata::class_constructors(&ci);
            crate::trace_compiler!(
                "resolve",
                "classifier constructor metadata owner={internal_name:?} declarations={ctor_param_lists:?}",
            );
            // Kotlin metadata owns the declaration list. JVM methods are addressed only by the exact
            // realization key carried by that declaration; methods absent from metadata are compiler
            // ABI, not source members. Java has no semantic metadata, so its classfile methods are the
            // declarations.
            let declared_methods: Vec<_> = if has_kotlin_metadata {
                meta_fns
                    .iter()
                    .filter(|declaration| !declaration.deprecated_hidden())
                    .filter_map(|declaration| {
                        let descriptor = declaration.jvm_desc?;
                        ci.methods
                            .iter()
                            .find(|method| {
                                method.name == declaration.jvm_name
                                    && method.descriptor == descriptor
                            })
                            .map(|method| (method, Some(declaration), None))
                    })
                    .chain(meta_constructors.iter().filter_map(|declaration| {
                        if declaration.deprecated_hidden {
                            return None;
                        }
                        let descriptor = declaration.jvm_desc?;
                        ci.methods
                            .iter()
                            .find(|method| {
                                method.name == "<init>" && method.descriptor == descriptor
                            })
                            .map(|method| (method, None, Some(declaration)))
                    }))
                    .chain(ci.methods.iter().filter_map(|method| {
                        let (_, ret) = parse_method_desc(&method.descriptor)?;
                        (method.is_static()
                            && owns_enum_entries_accessor
                            && method.name == "getEntries"
                            && ret.obj_internal()
                                == Some(crate::types::type_name("kotlin/enums/EnumEntries")))
                        .then_some((method, None, None))
                    }))
                    .collect()
            } else {
                ci.methods
                    .iter()
                    .map(|method| (method, None, None))
                    .collect()
            };
            for (m, declaration, constructor_declaration) in declared_methods {
                if declaration.is_none()
                    && constructor_declaration.is_none()
                    && m.is_bridge()
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
                if declaration.is_none()
                    && constructor_declaration.is_none()
                    && !m.is_public()
                    && !m.is_protected()
                {
                    continue;
                }
                let Some((mut params, mut ret)) = parse_method_desc(&m.descriptor) else {
                    continue;
                };
                let physical_params = params.clone();
                let physical_ret = ret;
                if uses_java_type_semantics {
                    for (index, parameter) in params.iter_mut().enumerate() {
                        *parameter = java_type_nullability(
                            *parameter,
                            m.parameter_nullability.get(index).copied().flatten(),
                        );
                    }
                    ret = java_type_nullability(ret, m.return_nullability);
                }
                // Kotlin metadata defines the SOURCE declarations. The class file only realizes
                // those declarations physically. An unmatched method in a Kotlin class is compiler
                // ABI (`$default`, accessors, box/unbox helpers, primitive-iterator boxing shims, …),
                // never an extra Kotlin member. Java classes have no metadata and therefore continue
                // to expose their classfile methods directly. Constructors are attributed by their
                // dedicated constructor metadata below; the enum entries accessor is retained only as
                // an explicit classifier capability and is not published as a member.
                let platform_nullable_params = uses_java_type_semantics.then(|| {
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
                if let Some(declaration) = declaration {
                    crate::trace_compiler!(
                        "member_slots",
                        "classifier metadata callable owner={internal_name:?} source={} jvm={} value_params={:?} context_params={:?}",
                        declaration.kotlin_name,
                        declaration.jvm_name,
                        declaration
                            .value_params
                            .iter()
                            .map(|parameter| parameter.name.as_str())
                            .collect::<Vec<_>>(),
                        declaration
                            .context_params
                            .iter()
                            .map(|parameter| parameter.name.as_str())
                            .collect::<Vec<_>>(),
                    );
                    let Some(signature) = declaration.generic_sig.clone() else {
                        crate::trace_compiler!(
                            "metadata_missing_type",
                            "classifier function has no semantic signature owner={internal_name:?} name={} jvm={}{}",
                            declaration.kotlin_name,
                            declaration.jvm_name,
                            m.descriptor,
                        );
                        continue;
                    };
                    member.name = declaration.kotlin_name.clone();
                    if declaration.jvm_name != declaration.kotlin_name {
                        member.physical_name = Some(declaration.jvm_name.clone());
                    }
                    let mut logical_params = signature.params.clone();
                    if declaration.is_extension() {
                        let receiver = signature
                            .receiver
                            .or_else(|| physical_params.first().copied());
                        let Some(receiver) = receiver else {
                            continue;
                        };
                        logical_params.insert(0, receiver);
                    }
                    member.params = logical_params;
                    member.ret = signature.ret;
                    member.generic_sig = Some(signature);
                    member.visibility = declaration.visibility;
                    member.set_ret_nullable(declaration.ret_nullable());
                    member.set_suspend(declaration.is_suspend());
                    member.context_count = declaration.context_count();
                    member.inline = InlineKind::from_flags(
                        declaration.is_inline(),
                        declaration.is_inline() && !m.is_public(),
                    );
                    member.reified = declaration.has_reified_type_params();
                    member.low_priority = declaration.low_priority();
                    member.contract = declaration.contract.clone();
                    member.set_is_member_extension(declaration.is_extension());
                    member.set_is_operator(declaration.is_operator());
                    member.set_is_infix(declaration.is_infix());
                    member.call_sig = declaration.member_call_sig();
                    member.declared_ret = metadata_declared_nonnull_nonsuspend_return(declaration);
                } else if let Some(declaration) = constructor_declaration {
                    if declaration.params.types.contains(&Ty::Error) {
                        crate::trace_compiler!(
                            "metadata_missing_type",
                            "constructor has no semantic signature owner={internal_name:?} descriptor={}",
                            m.descriptor,
                        );
                        continue;
                    }
                    member.params.clone_from(&declaration.params.types);
                    let physical_source_start = if let Some(outer) = constructor_outer {
                        if physical_params
                            .first()
                            .and_then(|parameter| parameter.obj_internal())
                            != Some(outer)
                        {
                            continue;
                        }
                        1
                    } else {
                        0
                    };
                    let physical_source_end = physical_source_start + member.params.len();
                    if physical_source_end > physical_params.len() {
                        continue;
                    }
                    member.physical_params =
                        physical_params[physical_source_start..physical_source_end].to_vec();
                    let class_formals = &ci.meta.class_type_parameters.type_params;
                    if !class_formals.is_empty() {
                        let class_arguments = class_formals
                            .iter()
                            .enumerate()
                            .map(|(index, formal)| {
                                let bound = ci
                                    .meta
                                    .class_type_parameters
                                    .type_param_bounds
                                    .get(index)
                                    .and_then(|bounds| bounds.first())
                                    .copied()
                                    .unwrap_or_else(|| Ty::obj("kotlin/Any"));
                                Ty::ty_param(formal, bound)
                            })
                            .collect::<Vec<_>>();
                        member.generic_sig = Some(GenericSig {
                            formals: class_formals.clone(),
                            formal_bounds: ci.meta.class_type_parameters.type_param_bounds.clone(),
                            receiver: None,
                            params: member.params.clone(),
                            ret: Ty::obj_args_name(internal_name, &class_arguments),
                            return_policy: Default::default(),
                        });
                    }
                    member.visibility = declaration.params.visibility;
                    member.call_sig = CallSig::metadata_member(
                        member.params.len(),
                        declaration.params.names.clone(),
                        declaration.params.defaults.clone(),
                        declaration.params.vararg,
                    );
                    // A constructor metadata key ending in the platform marker names an alternate
                    // realization, not a directly callable source constructor. The inner-aware pass
                    // below validates and attaches that realization after all declarations exist.
                    if physical_params.last().copied().is_some_and(|parameter| {
                        parameter.obj_internal().is_some_and(|name| {
                            name.matches("kotlin/jvm/internal/DefaultConstructorMarker")
                        })
                    }) {
                        member.descriptor.clear();
                    }
                }
                member.physical_ret = physical_ret;
                // Kotlin metadata owns declaration modality. In legacy JVM-default mode the
                // interface method is physically abstract even though the Kotlin declaration has
                // a body on `$DefaultImpls`; treating the classfile access bit as semantic made the
                // declaration disappear from `super` selection and inherited-implementation plans.
                member.set_is_abstract(
                    declaration
                        .map_or_else(|| m.is_abstract(), |declaration| declaration.is_abstract()),
                );
                member.set_is_interface(ci.is_interface());
                if m.is_static() {
                    member.realization = crate::libraries::MemberRealization::Direct {
                        pass_receiver: physical_params.len() == member.params.len() + 1,
                    };
                }
                // A concrete Kotlin interface declaration may have no body on the interface at
                // all. Normalize that ABI at the provider boundary: semantic selection still sees
                // the metadata declaration, while lowering receives the exact static holder owner,
                // name, and descriptor as its ordinary direct realization.
                if ci.is_interface()
                    && declaration.is_some_and(|declaration| !declaration.is_abstract())
                    && m.is_abstract()
                {
                    if let Some((holder, holder_method_descriptor)) =
                        interface_holder_method(&self.cp, internal_name, &m.name, &m.descriptor)
                    {
                        member.owner = Some(holder);
                        member.descriptor = holder_method_descriptor;
                        member.realization = crate::libraries::MemberRealization::Direct {
                            pass_receiver: true,
                        };
                    }
                }
                // The declared return classifier comes directly from the metadata declaration. A
                // nullable declared return stays absent because it is genuinely boxed.
                //
                // A `suspend` member is excluded: CPS makes its descriptor return `Object` whatever it
                // declares, so the descriptor no longer witnesses that the result is the erased
                // carrier — and for a PRIMITIVE-underlying value class it is not (kotlinc's
                // `make-<hash>(Continuation)Ljava/lang/Object;` hands back `M.box-impl(I)LM;`, a BOX).
                // Claiming the fact there would repr a boxed value as unboxed. Without it the member
                // falls back to the descriptor comparison, which classifies this case correctly.
                if let Some(java_nullable) = platform_nullable_params.clone() {
                    member.call_sig.platform_nullable_params = java_nullable;
                }
                member.signature = m.signature.clone();
                // Kotlin metadata is the source declaration. Its generic signature retains Kotlin
                // nullability and function-type facts that a JVM Signature attribute cannot encode.
                // Java declarations have no metadata and therefore use their JVM generic signature.
                if declaration.is_none() && constructor_declaration.is_none() {
                    member.visibility = if m.is_public() {
                        Visibility::Public
                    } else {
                        Visibility::Protected
                    };
                    member.generic_sig = m
                        .signature
                        .as_deref()
                        .and_then(parse_method_gsig)
                        .map(|signature| self.semanticize_jvm_generic_sig(signature));
                    if let Some(signature) = &mut member.generic_sig {
                        for (index, parameter) in signature.params.iter_mut().enumerate() {
                            *parameter = java_type_nullability(
                                *parameter,
                                m.parameter_nullability.get(index).copied().flatten(),
                            );
                        }
                        signature.ret = java_type_nullability(signature.ret, m.return_nullability);
                    }
                }
                let value_arity = member.params.len();
                if member.suspend() {
                    // The EMIT descriptor is the LOGICAL (continuation-stripped) form — the coroutine pass
                    // re-threads the CPS `Continuation` at the call. `physical_params` retains the classfile
                    // shape while `params` is the normalized source-semantic shape used by resolution.
                    member.descriptor = strip_continuation_param(&member.descriptor);
                }
                if is_map && member.name == "put" {
                    member.set_ret_nullable(true);
                }
                if declaration.is_none() && constructor_declaration.is_none() {
                    member.call_sig = {
                        let vararg_index = m
                            .is_vararg()
                            .then_some(value_arity)
                            .and_then(|arity| arity.checked_sub(1));
                        CallSig::metadata_member(value_arity, Vec::new(), Vec::new(), vararg_index)
                    };
                    // Java declarations cannot carry Kotlin's `operator` modifier, but Kotlin still
                    // admits the zero-argument iteration/destructuring conventions by their Java
                    // method names. Normalize that language fact at the provider boundary so common
                    // convention selection can keep requiring authoritative operator capability.
                    if uses_java_type_semantics
                        && member.params.is_empty()
                        && (matches!(member.name.as_str(), "iterator" | "hasNext" | "next")
                            || member.name.strip_prefix("component").is_some_and(|suffix| {
                                !suffix.is_empty()
                                    && suffix.bytes().all(|byte| byte.is_ascii_digit())
                            }))
                    {
                        member.set_is_operator(true);
                    }
                }
                if let Some(java_nullable) = platform_nullable_params {
                    member.call_sig.platform_nullable_params = java_nullable;
                }
                if m.name == "<init>" {
                    let signature = constructor_declaration.map(|declaration| &declaration.params);
                    if let (Some(gsig), Some(recv_fun)) = (
                        member.generic_sig.as_mut(),
                        signature
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
                } else if declaration.is_some() {
                    members.push(member);
                } else if m.is_static() {
                    companion.push(member);
                } else {
                    let source_name =
                        super::names::mapped_builtin_virtual_source_name(&ci.this_class(), &m.name);
                    if source_name != m.name {
                        let mut alias = member.clone();
                        alias.name = source_name.to_string();
                        alias.physical_name = Some(m.name.clone());
                        members.push(alias);
                    } else {
                        members.push(member);
                    }
                }
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
            let builtin_class_signature = self.cp.builtin_class_gsig_name(internal_name);
            let metadata_class_signature = ci.meta.class_visibility.map(|_| {
                (
                    ci.meta.class_type_parameters.type_params.clone(),
                    ci.meta.class_type_parameters.type_param_bounds.clone(),
                    ci.meta.class_supertypes.clone(),
                )
            });
            let semantic_class_signature = metadata_class_signature.clone().or_else(|| {
                builtin_class_signature
                    .clone()
                    .map(|(formals, supertypes)| {
                        let bounds = vec![Vec::new(); formals.len()];
                        (formals, bounds, supertypes)
                    })
            });
            let builtin_supertypes = self.cp.builtin_supertypes_name(internal_name);
            let builtin_members = if is_mapped_builtin {
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
            // A class present in `.kotlin_builtins` already has its exact Kotlin supertype list there.
            // Its JVM interfaces are realizations of that list, not additional source supertypes. In
            // particular, `IntIterator : Iterator<Int>` also implements `java.util.Iterator<Integer>`
            // in bytecode; publishing both creates two same-depth `next` declarations and lets the
            // erased Java `next(): Any!` compete with the Kotlin declaration.
            let kotlin_supertypes_are_authoritative = metadata_class_signature.is_some();
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
            } else if let Some((_, _, declared)) = &metadata_class_signature {
                for supertype in declared {
                    if let Some(name) = supertype.obj_internal() {
                        supertypes.push_name(name);
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
            if !kotlin_supertypes_are_authoritative {
                for s in builtin_supertypes.iter_ids() {
                    let duplicate_class_face = self.cp.builtin_is_interface_name(s) == Some(false)
                        && supertypes.iter_ids().any(|existing| {
                            super::jvm_class_map::type_names_map_to_same_jvm_internal(existing, s)
                        });
                    if !supertypes.contains_name(s) && !duplicate_class_face {
                        supertypes.push_name(s);
                    }
                }
            }
            // A JVM mapped builtin and its Kotlin declaration are one source-level classifier face.
            // Publish that face in the class model so core's ordinary supertype walk handles every use:
            // `StringBuilder : java.lang.CharSequence` reaches `kotlin.CharSequence`, and Java collection
            // implementations reach their Kotlin collection interfaces. The mapping table owns the
            // equivalence; extension resolution must not reconstruct a platform-name list.
            let mut mapped: Vec<TypeName> = std::iter::once(internal_name)
                .chain(supertypes.iter_ids())
                .filter_map(super::jvm_class_map::jvm_to_kotlin_builtin_metadata_name)
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
                // A mapped CLASS face is the same superclass edge under its Kotlin identity; adding
                // both (`java/lang/Object` and `kotlin/Any`) would publish two class parents. Mapped
                // INTERFACE faces stay explicit: source assignability needs the Kotlin collection /
                // CharSequence identity in addition to the physical Java interface realization.
                let duplicate_class_face = self.cp.builtin_is_interface_name(k) == Some(false)
                    && supertypes.iter_ids().any(|existing| {
                        super::jvm_class_map::type_names_map_to_same_jvm_internal(existing, k)
                    });
                if !supertypes.contains_name(k) && !duplicate_class_face {
                    supertypes.push_name(k);
                }
            }
            // A companion object compiles to a `public static final C$Name` field on `C` (default name
            // `Companion`; e.g. `Json.Default: Json$Default`). Detect it by the descriptor pattern
            // `L<this>$<fieldname>;` so a bare `C` reference can resolve to the companion instance.
            let companion_object = ci
                .fields
                .iter()
                .find_map(|f| {
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
                })
                .or_else(|| self.cp.builtin_companion_object(internal_name));
            let kind = if let Some(kind) = ci.meta.class_kind {
                kind
            } else if ci.access & 0x2000 != 0 {
                crate::libraries::TypeKind::Annotation
            } else if ci.is_interface() {
                crate::libraries::TypeKind::Interface
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
            // Metadata constructors are semantic callables even when no public `<init>` has their
            // source shape. That happens for a value class itself and for an ordinary class whose
            // parameter is a value class: JVM storage replaces/hides the primary constructor and only
            // marker overloads remain public. Publish every missing declaration here. Its empty opaque
            // emit token says only "the provider has no direct physical method for this declaration";
            // the selected application is paired with the matching physical marker below resolution.
            for declaration in meta_constructors {
                // A HIDDEN-deprecated declaration was dropped above; never resurrect it here.
                // Iterating the declarations directly (rather than the filtered param-list
                // projection) keeps this loop and the hidden filter from ever disagreeing.
                let signature = &declaration.params;
                if declaration.deprecated_hidden
                    || signature.types.len() != signature.names.len()
                    || signature.types.contains(&Ty::Error)
                    || constructors.iter().any(|constructor| {
                        constructor.params.len() == signature.types.len()
                            && constructor.call_sig.param_names == signature.names
                    })
                {
                    continue;
                }
                let mut constructor = LibraryMember::new(
                    "<init>".to_string(),
                    signature.types.clone(),
                    Ty::obj_name(internal_name),
                    String::new(),
                );
                constructor.visibility = signature.visibility;
                constructor.call_sig = CallSig::metadata_member(
                    constructor.params.len(),
                    signature.names.clone(),
                    signature.defaults.clone(),
                    signature.vararg,
                );
                constructors.insert(0, constructor);
            }
            // Metadata owns the constructor declarations. Marker/default methods are JVM realization
            // details and never enter `constructors`; couple their opaque descriptor to the matching
            // metadata declaration here, while both the source signature and classfile are available.
            if has_kotlin_metadata {
                for constructor in &mut constructors {
                    if constructor.default_realization.is_some() {
                        continue;
                    }
                    let has_defaults = constructor
                        .call_sig
                        .param_defaults
                        .iter()
                        .any(|default| *default);
                    let source_count = constructor.params.len();
                    let mask_count = source_count.div_ceil(32).max(1);
                    let outer_count = usize::from(constructor_outer.is_some());
                    let metadata = meta_constructors.iter().find(|declaration| {
                        declaration.params.names == constructor.call_sig.param_names
                            && declaration.params.types == constructor.params
                    });
                    let expected_real_params = metadata
                        .and_then(|declaration| declaration.jvm_desc)
                        .or_else(|| {
                            (!constructor.descriptor.is_empty())
                                .then_some(constructor.descriptor.as_str())
                        })
                        .and_then(parse_method_desc)
                        .and_then(|(mut params, ret)| {
                            (ret == Ty::Unit).then_some(())?;
                            if params.last().copied().is_some_and(|parameter| {
                                parameter.obj_internal().is_some_and(|name| {
                                    name.matches("kotlin/jvm/internal/DefaultConstructorMarker")
                                })
                            }) {
                                params.pop();
                            }
                            (params.len() == outer_count + source_count)
                                .then(|| params[outer_count..].to_vec())
                        });
                    let Some(expected_real_params) = expected_real_params else {
                        continue;
                    };
                    constructor.default_realization = ci.methods.iter().find_map(|method| {
                        if method.name != "<init>" {
                            return None;
                        }
                        let (params, ret) = parse_method_desc(&method.descriptor)?;
                        let marker = params.last().copied().is_some_and(|parameter| {
                            parameter.obj_internal().is_some_and(|name| {
                                name.matches("kotlin/jvm/internal/DefaultConstructorMarker")
                            })
                        });
                        let real_start = outer_count;
                        let real_end = real_start + source_count;
                        let shape_matches = if has_defaults {
                            params.len() == outer_count + source_count + mask_count + 1
                                && params[real_start..real_end] == expected_real_params
                                && params[real_end..real_end + mask_count]
                                    .iter()
                                    .all(|parameter| *parameter == Ty::Int)
                        } else {
                            params.len() == outer_count + source_count + 1
                                && params[real_start..real_end] == expected_real_params
                        };
                        (ret == Ty::Unit && marker && shape_matches).then(|| {
                            Box::new(crate::libraries::DefaultCallRealization {
                                descriptor: method.descriptor.clone(),
                                real_params: params[real_start..real_end].to_vec(),
                                mask_count: if has_defaults { mask_count } else { 0 },
                                ret: Ty::Unit,
                                suspend: false,
                            })
                        })
                    });
                }
            }
            crate::trace_compiler!(
                "resolve",
                "classifier constructors owner={internal_name:?} semantic={:?}",
                constructors
                    .iter()
                    .map(|constructor| (
                        &constructor.params,
                        constructor.visibility,
                        constructor.descriptor.as_str(),
                    ))
                    .collect::<Vec<_>>(),
            );
            // The class's own formal type parameters (`Pair` → `[A, B]`), for constructor type-argument
            // inference; empty for a non-generic type.
            let type_params = semantic_class_signature
                .as_ref()
                .map(|(formals, _, _)| formals.clone())
                .or_else(|| {
                    ci.signature
                        .as_deref()
                        .and_then(parse_class_gsig)
                        .map(|(formals, _, _)| formals)
                })
                .unwrap_or_default();
            let type_param_bounds = semantic_class_signature
                .as_ref()
                .map(|(_, bounds, _)| bounds.clone())
                .or_else(|| {
                    ci.signature
                        .as_deref()
                        .and_then(parse_class_gsig)
                        .map(|(_, bounds, _)| bounds)
                })
                .unwrap_or_else(|| vec![Vec::new(); type_params.len()]);
            let type_param_variances = if metadata_class_signature.is_some() {
                ci.meta.class_type_parameters.type_param_variances.clone()
            } else {
                self.cp
                    .builtin_class_variances_name(internal_name)
                    .unwrap_or_else(|| {
                        vec![crate::types::TypeVariance::Invariant; type_params.len()]
                    })
            };
            let declared_supertype_templates = semantic_class_signature
                .as_ref()
                .map(|(_, _, supertypes)| supertypes.clone())
                .or_else(|| {
                    ci.signature
                        .as_deref()
                        .and_then(parse_class_gsig)
                        .map(|(_, _, supertypes)| supertypes)
                })
                .unwrap_or_default();
            let callable_signature = declared_supertype_templates
                .iter()
                .copied()
                .find(|supertype| matches!(supertype, Ty::Fun(_)));
            let self_arguments = type_params
                .iter()
                .map(|formal| Ty::ty_param(formal, Ty::obj("kotlin/Any")))
                .collect::<Vec<_>>();
            let supertype_templates = supertypes
                .iter_ids()
                .map(|semantic| {
                    if let Some(template) = declared_supertype_templates.iter().find(|template| {
                        template.obj_internal().is_some_and(|declared| {
                            declared == semantic
                                || super::jvm_class_map::type_names_map_to_same_jvm_internal(
                                    declared, semantic,
                                )
                        })
                    }) {
                        return Ty::obj_args_name(semantic, template.type_args());
                    }
                    if super::jvm_class_map::type_names_map_to_same_jvm_internal(
                        internal_name,
                        semantic,
                    ) {
                        return Ty::obj_args_name(semantic, &self_arguments);
                    }
                    Ty::obj_name(semantic)
                })
                .collect();
            // An enum entry is a `static` field of the enum's OWN type (`descriptor == L<internal>;`).
            const ACC_STATIC: u16 = 0x0008;
            let enum_entry_descriptor = format!("L{internal};");
            let enum_entries: Vec<String> = ci
                .fields
                .iter()
                .filter(|f| f.access & ACC_STATIC != 0 && f.descriptor == enum_entry_descriptor)
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
                // A mapped JVM method and its Kotlin builtin entry are two descriptions of one
                // declaration, not two overloads. Prefer the builtin: it carries Kotlin-only facts such
                // as `operator`, source names, and nullability while retaining the same physical target.
                members.retain(|member| {
                    !builtin_members.iter().any(|builtin| {
                        let member_physical = member
                            .physical_name
                            .as_deref()
                            .unwrap_or(member.name.as_str());
                        let builtin_physical = builtin
                            .physical_name
                            .as_deref()
                            .unwrap_or(builtin.name.as_str());
                        member_physical == builtin_physical
                            && member.descriptor == builtin.descriptor
                    })
                });
            }
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
                                .map(|(ty, _)| self.semanticize_jvm_type(ty))
                        })
                        .unwrap_or(erased_ty);
                    let ty = if uses_java_type_semantics {
                        java_type_nullability(ty, field.nullability)
                    } else {
                        ty
                    };
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
            let inheritance = crate::libraries::ClassifierInheritance {
                is_abstract: ci.is_abstract() || ci.is_interface(),
                is_extensible: !ci.is_final() && !ci.is_interface(),
                has_no_arg_constructor: constructors.iter().any(|constructor| {
                    constructor.params.is_empty()
                        && matches!(
                            constructor.visibility,
                            crate::types::Visibility::Public | crate::types::Visibility::Protected
                        )
                }),
            };
            let members = members
                .into_iter()
                .chain(builtin_members)
                .collect::<Vec<_>>();
            let callable_signature =
                callable_signature.or_else(|| function_interface_signature(&supertypes, &members));
            let mut named_parameter_lists = metadata::class_constructor_params(&ci);
            if kind == crate::libraries::TypeKind::Annotation && !has_kotlin_metadata {
                named_parameter_lists.push(java_annotation_parameter_list(&ci)?);
            }
            Some(LibraryType {
                access: if let Some(visibility) = ci.meta.class_visibility {
                    visibility.into()
                } else {
                    let access = effective_class_access(&ci);
                    if access & 0x0001 != 0 {
                        crate::libraries::ClassifierAccess::Public
                    } else if access & 0x0004 != 0 {
                        crate::libraries::ClassifierAccess::Protected
                    } else if access & 0x0002 != 0 {
                        crate::libraries::ClassifierAccess::Private
                    } else {
                        crate::libraries::ClassifierAccess::PackagePrivate
                    }
                },
                source_file: None,
                is_nested: ci.inner_class_self().is_some(),
                outer_instance: ci.inner_class_self().and_then(|entry| {
                    (entry.access & ACC_STATIC == 0)
                        .then(|| entry.outer.as_deref().map(type_name))
                        .flatten()
                }),
                kind,
                inheritance,
                supertypes,
                supertype_templates,
                constructors,
                fields,
                declared_callables: std::collections::HashMap::new(),
                members,
                companion,
                constants: self.constants_for_class(internal_name, &ci),
                sam_method: self.sam_method_for_class(&ci.this_class()),
                callable_signature,
                companion_object,
                value_companion_fns: self.value_companion_fns_for_class(&ci, inline.is_some()),
                value_underlying,
                value_underlying_property: inline
                    .as_ref()
                    .and_then(|metadata| metadata.property_name.clone()),
                alias_target: None,
                type_parameters: crate::types::TypeParameters::new(
                    type_params,
                    type_param_bounds,
                    type_param_variances,
                ),
                sealed_subclasses: metadata::class_sealed_subclasses(&ci).into(),
                enum_entries,
                enum_entries_accessor,
                named_parameter_lists,
                // JLS default: an annotation declaration without `@Retention` has CLASS retention.
                // Normalize that provider fact here so common checking never branches on declaration
                // origin or tries to interpret an absent classfile attribute.
                retention: ci.retention.clone().or_else(|| {
                    (kind == crate::libraries::TypeKind::Annotation).then(|| "CLASS".to_string())
                }),
                // The declared `@Target`, normalized to the three sites a PROPERTY application can
                // take. A KOTLIN annotation states its own set (`kotlin.annotation.Target`, the only
                // place `PROPERTY` can be named); a JAVA `@interface` states `ElementType`s, none of
                // which is a Kotlin property — so kotlinc puts a bare Java annotation on a property's
                // FIELD, never on the property.
                annotation_targets: (kind == crate::libraries::TypeKind::Annotation)
                    .then(|| classpath_annotation_targets(&ci)),
            })
        }
    }

    fn builtin_library_type(&self, internal: TypeName) -> Option<LibraryType> {
        let (kind, access, is_nested) = self.cp.builtin_classifier_shape_name(internal)?;
        let (formals, supertype_templates) = self
            .cp
            .builtin_class_gsig_name(internal)
            .unwrap_or_default();
        let variances = self
            .cp
            .builtin_class_variances_name(internal)
            .unwrap_or_else(|| vec![crate::types::TypeVariance::Invariant; formals.len()]);
        let members = self.builtin_members_for_type_name(internal);
        crate::trace_compiler!(
            "resolve",
            "builtin classifier {} members={:?}",
            internal.render(),
            members
                .iter()
                .map(|member| member.name.as_str())
                .collect::<Vec<_>>()
        );
        crate::trace_compiler!(
            "metadata_companions",
            "builtin classifier {} resolved members={:?}",
            internal,
            members
                .iter()
                .map(|member| (member.name.as_str(), member.params.as_slice(), member.ret))
                .collect::<Vec<_>>()
        );
        let mut classifier = builtin_library_type(
            kind,
            access,
            is_nested,
            self.cp.builtin_supertypes_name(internal),
            members,
            self.cp.builtin_constructors_name(internal),
            BuiltinGenericShape {
                type_params: formals,
                type_param_variances: variances,
                supertype_templates,
            },
        );
        classifier.companion_object = self.cp.builtin_companion_object(internal);
        classifier.constants = std::collections::HashMap::new();
        Some(classifier)
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
            let node = if args.is_empty() {
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
            args.push(Ty::out_projection(Ty::nullable(Ty::obj("kotlin/Any"))));
            field_inexact = true;
            rest = tail;
            continue;
        }
        let (arg, variance) = if let Some(arg) = rest.strip_prefix('+') {
            (arg, Some(crate::types::TypeVariance::Out))
        } else if let Some(arg) = rest.strip_prefix('-') {
            (arg, Some(crate::types::TypeVariance::In))
        } else {
            (rest, None)
        };
        if !matches!(arg.as_bytes().first(), Some(b'L' | b'T' | b'[')) {
            return None;
        }
        let parsed = parse_gsig_inner(arg, for_field)?;
        has_free |= parsed.has_free;
        field_inexact |= variance.is_some() || parsed.field_inexact;
        args.push(match variance {
            Some(crate::types::TypeVariance::Out) => Ty::out_projection(parsed.ty),
            Some(crate::types::TypeVariance::In) => Ty::in_projection(parsed.ty),
            Some(crate::types::TypeVariance::Invariant) | None => parsed.ty,
        });
        rest = parsed.rest;
    }
    Some((args, has_free, field_inexact, rest.strip_prefix('>')?))
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
            bounds.push(java_type_nullability(bound, None));
            rest = tail;
        }
        while let Some(tail) = rest.strip_prefix(':') {
            let Some((bound, after)) = parse_gsig(tail) else {
                return (Vec::new(), Vec::new(), original);
            };
            bounds.push(java_type_nullability(bound, None));
            rest = after;
        }
        formal_bounds.push(bounds);
    }
    (formals, formal_bounds, &rest[1..])
}

/// Parse a JVM method generic signature `<formals>(params)ret`.
fn parse_method_gsig(sig: &str) -> Option<GenericSig> {
    let (formals, mut formal_bounds, s) = parse_formals(sig);
    let mut inline_bounds = formals
        .iter()
        .zip(&formal_bounds)
        .map(|(formal, bounds)| {
            (
                formal.clone(),
                bounds
                    .first()
                    .copied()
                    .unwrap_or_else(|| Ty::platform_nullable(Ty::obj("kotlin/Any"))),
            )
        })
        .collect::<std::collections::HashMap<_, _>>();
    for bounds in &mut formal_bounds {
        for bound in bounds {
            *bound = crate::types::ty_with_param_bounds(*bound, &inline_bounds);
        }
    }
    inline_bounds = formals
        .iter()
        .zip(&formal_bounds)
        .map(|(formal, bounds)| {
            (
                formal.clone(),
                bounds
                    .first()
                    .copied()
                    .unwrap_or_else(|| Ty::platform_nullable(Ty::obj("kotlin/Any"))),
            )
        })
        .collect();
    let inner = s.strip_prefix('(')?;
    let close = inner.find(')')?;
    let mut params_s = &inner[..close];
    let mut params = Vec::new();
    while !params_s.is_empty() {
        let (p, rest) = parse_gsig(params_s)?;
        params.push(crate::types::ty_with_param_bounds(p, &inline_bounds));
        params_s = rest;
    }
    let (ret, _) = parse_gsig(&inner[close + 1..])?;
    let ret = crate::types::ty_with_param_bounds(ret, &inline_bounds);
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
            Ty::obj(to_kotlin_internal(raw_internal))
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

/// Normalize a constructor-less annotation declaration into the common application shape while
/// JVM descriptors and `AnnotationDefault` are still provider-owned facts. Core never reparses a
/// descriptor or asks whether this classifier came from Java.
fn java_annotation_parameter_list(class: &crate::jvm::classreader::ClassInfo) -> Option<ParamList> {
    // Java exposes a class-valued annotation element as `Class<T>`, while Kotlin application syntax
    // supplies a `KClass<T>`. Keep the generic argument decoded from the method's authoritative
    // `Signature` attribute: dropping it would accept `String::class` for
    // `Class<? extends Runnable>`. The physical descriptor remains provider-owned and unchanged.
    let kotlin_class_element = |semantic: Ty| {
        let arguments = match semantic.non_null() {
            Ty::Obj(_, arguments) => arguments,
            _ => &[],
        };
        Ty::obj_args_name(crate::types::type_name("kotlin/reflect/KClass"), arguments)
    };
    let mut elements = class
        .methods
        .iter()
        .filter(|method| method.is_public() && !method.is_static() && method.name != "<init>")
        .map(|method| {
            let (parameters, ret) = crate::jvm::names::parse_method_descriptor(&method.descriptor)?;
            if !parameters.is_empty() {
                return None;
            }
            let erased = field_desc_to_ty(ret);
            let generic = method
                .signature
                .as_deref()
                .and_then(parse_method_gsig)
                .map(|signature| signature.ret);
            let ty = match erased.array_elem() {
                Some(erased_element) if erased_element.non_null() == Ty::obj("java/lang/Class") => {
                    let element = generic.and_then(Ty::array_elem).unwrap_or(erased_element);
                    Ty::array(kotlin_class_element(element))
                }
                None if erased.non_null() == Ty::obj("java/lang/Class") => {
                    kotlin_class_element(generic.unwrap_or(erased))
                }
                Some(_) | None => erased,
            };
            (ty != Ty::Error).then(|| (method.name.clone(), ty, method.has_annotation_default))
        })
        .collect::<Option<Vec<_>>>()?;
    let value_index = elements.iter().position(|(name, _, _)| name == "value");
    let value_vararg = value_index.is_some_and(|index| elements[index].1.array_elem().is_some());
    if let Some(index) = value_index {
        let value = elements.remove(index);
        elements.insert(0, value);
    }
    let positional = match (value_index, value_vararg) {
        (_, true) => AnnotationPositionalPolicy::ValueVararg,
        (Some(_), false) => AnnotationPositionalPolicy::Value,
        (None, false) => AnnotationPositionalPolicy::NamedOnly,
    };
    Some(ParamList {
        visibility: Visibility::Public,
        names: elements.iter().map(|(name, _, _)| name.clone()).collect(),
        defaults: elements.iter().map(|(_, _, default)| *default).collect(),
        types: elements.iter().map(|(_, ty, _)| *ty).collect(),
        recv_fun: vec![false; elements.len()],
        vararg: value_vararg.then_some(0),
        annotation: Some(AnnotationParameterPolicy {
            positional,
            materialize_omitted_vararg: false,
        }),
    })
}

/// Minimal classifier signature for a mapped builtin whose physical JVM class is absent. This is
/// provider construction data: core still receives an ordinary `LibraryType` record and performs the
/// same member/hierarchy selection as for every other classifier.
fn mapped_builtin_signature(internal: &str) -> Option<LibraryType> {
    // Each tuple: Kotlin member name, JVM descriptor, logical return type. The owner is left implicit
    // (the receiver's Kotlin internal, e.g. `kotlin/String`); the constant-pool boundary maps it to the
    // JVM name, exactly as for a classpath-resolved member, without exposing `java/lang/*` to core.
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
        access: crate::libraries::ClassifierAccess::Public,
        source_file: None,
        is_nested: false,
        outer_instance: None,
        kind: crate::libraries::TypeKind::Class,
        inheritance: Default::default(),
        supertypes: TypeNameList::new(),
        supertype_templates: Vec::new(),
        constructors: Vec::new(),
        fields: Vec::new(),
        declared_callables: std::collections::HashMap::new(),
        members,
        companion: Vec::new(),
        constants: Default::default(),
        sam_method: None,
        callable_signature: None,
        companion_object: None,
        value_companion_fns: Vec::new(),
        value_underlying: None,
        value_underlying_property: None,
        alias_target: None,
        type_parameters: crate::types::TypeParameters::default(),
        sealed_subclasses: TypeNameList::new(),
        enum_entries: Vec::new(),
        enum_entries_accessor: None,
        named_parameter_lists: Vec::new(),
        retention: None,
        annotation_targets: None,
    })
}

/// Property/function distinction from Kotlin's mapped built-in signature when no decoded
/// `.kotlin_builtins` fragment is present. The JVM method alone cannot express that `String.length()`
/// occupies Kotlin's property namespace. This fact is folded into `declared_callables` while the one
/// classifier record is constructed; it is not a resolution-time special case.
fn mapped_builtin_property(internal: TypeName, name: &str) -> bool {
    internal.matches("kotlin/String") && name == "length"
}

struct BuiltinGenericShape {
    type_params: Vec<String>,
    type_param_variances: Vec<crate::types::TypeVariance>,
    supertype_templates: Vec<Ty>,
}

/// The [`LibraryType`] of a classless Kotlin BUILTIN (`kotlin/Number`, `kotlin/collections/List`, …) whose
/// JVM class is absent from the classpath (a no-JDK compile) — supertypes and members from the
/// `.kotlin_builtins` data, kind from the metadata `is_interface` flag.
fn builtin_library_type(
    kind: crate::libraries::TypeKind,
    access: crate::libraries::ClassifierAccess,
    is_nested: bool,
    supertypes: TypeNameList,
    members: Vec<LibraryMember>,
    constructors: Vec<LibraryMember>,
    generic: BuiltinGenericShape,
) -> LibraryType {
    let callable_signature = generic
        .supertype_templates
        .iter()
        .copied()
        .find(|supertype| matches!(supertype, Ty::Fun(_)));
    LibraryType {
        access,
        source_file: None,
        is_nested,
        outer_instance: None,
        kind,
        inheritance: Default::default(),
        supertypes,
        supertype_templates: generic.supertype_templates,
        constructors,
        fields: Vec::new(),
        declared_callables: std::collections::HashMap::new(),
        members,
        companion: Vec::new(),
        constants: Default::default(),
        sam_method: None,
        callable_signature,
        companion_object: None,
        value_companion_fns: Vec::new(),
        value_underlying: None,
        value_underlying_property: None,
        alias_target: None,
        type_parameters: crate::types::TypeParameters::new(
            generic.type_params.clone(),
            vec![Vec::new(); generic.type_params.len()],
            generic.type_param_variances,
        ),
        sealed_subclasses: TypeNameList::new(),
        enum_entries: Vec::new(),
        enum_entries_accessor: None,
        named_parameter_lists: Vec::new(),
        retention: None,
        annotation_targets: None,
    }
}

fn function_interface_signature(
    supertypes: &TypeNameList,
    members: &[LibraryMember],
) -> Option<Ty> {
    if !supertypes.contains_name(type_name("kotlin/Function")) {
        return None;
    }
    let mut invokes = members.iter().filter(|member| member.name == "invoke");
    let invoke = invokes.next()?;
    if invokes.next().is_some() {
        return None;
    }
    let (params, ret) = invoke
        .generic_sig
        .as_ref()
        .map_or((&*invoke.params, invoke.ret), |signature| {
            (&*signature.params, signature.ret)
        });
    Some(Ty::fun(params.to_vec(), ret))
}

/// Parse a class generic signature into its formal type-parameter names and its supertypes (the
/// superclass followed by interfaces) as signature nodes, e.g. `java/util/List`'s
/// `<E:Ljava/lang/Object;>Ljava/lang/Object;Ljava/util/Collection<TE;>;` → (`[E]`, `[Object,
/// Collection<E>]`). The supertypes carry their own type arguments (in terms of this class's formals),
/// which is what lets a type argument propagate up the hierarchy (`List<Int>` → `Collection<Int>`).
fn parse_class_gsig(sig: &str) -> Option<(Vec<String>, Vec<Vec<Ty>>, Vec<Ty>)> {
    let (formals, formal_bounds, mut s) = parse_formals(sig);
    let mut supers = Vec::new();
    while !s.is_empty() {
        let (g, rest) = parse_gsig(s)?;
        supers.push(g);
        s = rest;
    }
    Some((formals, formal_bounds, supers))
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

/// Exact physical realization of a legacy concrete interface declaration, if the classpath
/// publishes one. Semantic selection stays on the metadata declaration; this only couples it to the
/// matching receiver-first static method at the provider boundary.
fn interface_holder_method(
    cp: &crate::jvm::classpath::Classpath,
    interface: TypeName,
    name: &str,
    descriptor: &str,
) -> Option<(TypeName, String)> {
    // `$DefaultImpls` also exists in compatibility mode, where the interface method itself is
    // concrete and remains the dispatch target. Only the legacy shape has an ABSTRACT interface
    // method whose implementation must be replaced by the receiver-first holder static. Keeping
    // this representation test here makes functions and property accessors share one ABI rule.
    let interface_class = cp.find_name(interface)?;
    if !interface_class.methods.iter().any(|method| {
        method.is_abstract() && method.name == name && method.descriptor == descriptor
    }) {
        return None;
    }
    let holder = crate::types::type_name_nested_child(interface, "DefaultImpls");
    let descriptor = descriptor
        .strip_prefix('(')
        .map(|tail| format!("(L{};{tail}", interface.render()))?;
    cp.find_name(holder)
        .is_some_and(|class| {
            class.methods.iter().any(|method| {
                method.is_static() && method.name == name && method.descriptor == descriptor
            })
        })
        .then_some((holder, descriptor))
}

fn inline_body_descriptor(callable: &LibraryCallable) -> Option<String> {
    if !callable.suspend {
        return Some(callable.descriptor.clone());
    }
    let close = callable.descriptor.rfind(')')?;
    Some(format!(
        "({}{}){}",
        &callable.descriptor[1..close],
        CONTINUATION_PARAM_DESCRIPTOR,
        &callable.descriptor[close + 1..]
    ))
}

fn callable_parameter_slots(parameters: &[Ty]) -> Vec<u16> {
    let mut next = 0u16;
    parameters
        .iter()
        .map(|parameter| {
            let slot = next;
            next += u16::from(matches!(*parameter, Ty::Long | Ty::Double)) + 1;
            slot
        })
        .collect()
}

fn inline_plan_member(target: (&str, &str, &str, bool), suspend: bool) -> Option<LibraryMember> {
    let (owner, name, descriptor, interface) = target;
    let logical_descriptor = if suspend {
        strip_continuation_param(descriptor)
    } else {
        descriptor.to_string()
    };
    let (params, ret) = parse_method_desc(&logical_descriptor)?;
    let mut member = LibraryMember::new(name.to_string(), params, ret, logical_descriptor);
    member.owner = Some(type_name(owner));
    member.physical_ret = parse_method_desc(descriptor)?.1;
    member.set_is_interface(interface);
    member.set_suspend(suspend);
    Some(member)
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

impl JvmLibraries {
    /// Federate the classpath package catalog with the core declaration source. `symbols()` already
    /// combines those sources; package-prefix resolution must expose the same namespace or an explicit
    /// import can be rejected before its core symbol is queried.
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        self.cp.has_package(parent, name) || EmptySymbolSource.package_exists(parent, name)
    }

    fn declared_callables_for(&self, recv: Ty, name: &str) -> crate::libraries::Callables {
        let functions = self.member_functions(recv, name);
        // Exact declarations on this classifier. The resolver owns the one inheritance walk.
        let Some(internal) = recv.kotlin_class_internal() else {
            return crate::libraries::Callables::from_parts(functions, PropertySet::default());
        };
        let mut overloads = Vec::new();
        let cn = internal;
        if let Some(ci) = self.cp.find_name(cn) {
            for mp in metadata::class_properties(&ci) {
                if mp.name != name {
                    continue;
                }
                let property_signature = mp.generic_sig.as_ref();
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
                let ty = if ret_ty.is_jvm_scalar() {
                    ret_ty
                } else {
                    mp.ret_class.map_or(Ty::obj("kotlin/Any"), Ty::obj_name)
                };
                let Some((mut getter_params, getter_ret)) = parse_method_desc(&getter.desc) else {
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
                let mut getter = LibraryCallable::library(
                    cn,
                    getter.name,
                    getter_params,
                    ret_ty,
                    getter_ret,
                    getter.desc,
                );
                getter.owner_is_interface = ci.is_interface();
                getter.is_abstract = mp.is_abstract;
                if let Some((holder, descriptor)) =
                    interface_holder_method(&self.cp, cn, &getter.name, &getter.descriptor)
                {
                    getter.owner = holder;
                    getter.descriptor = descriptor;
                    getter.member_realization = crate::libraries::MemberRealization::Direct {
                        pass_receiver: true,
                    };
                }
                let getter_signature = self
                    .member_functions(recv, &getter.name)
                    .overloads
                    .into_iter()
                    .find(|function| function.callable.params.is_empty())
                    .and_then(|function| function.generic_sig);
                // The property metadata is the source declaration. Its signature retains nested
                // arguments and Kotlin identities (`List<Int>`), while the accessor descriptor is
                // only its erased physical realization (`java/util/List`). Do not rediscover the
                // property type through a getter overload query.
                let ty = property_signature
                    .map(|signature| signature.ret)
                    .or_else(|| getter_signature.as_ref().map(|signature| signature.ret))
                    .unwrap_or(ty);
                getter.ret = ty;
                let setter = mp.setter.clone().and_then(|s| {
                    let (physical_params, physical_ret) = parse_method_desc(&s.desc)?;
                    if physical_params.len() != 1 || physical_ret != Ty::Unit {
                        return None;
                    }
                    let mut setter = LibraryCallable::library(
                        cn,
                        s.name,
                        physical_params,
                        Ty::Unit,
                        physical_ret,
                        s.desc,
                    );
                    setter.params = vec![ty];
                    setter.owner_is_interface = ci.is_interface();
                    setter.is_abstract = mp.is_abstract;
                    if let Some((holder, descriptor)) =
                        interface_holder_method(&self.cp, cn, &setter.name, &setter.descriptor)
                    {
                        setter.owner = holder;
                        setter.descriptor = descriptor;
                        setter.member_realization = crate::libraries::MemberRealization::Direct {
                            pass_receiver: true,
                        };
                    }
                    Some(setter)
                });
                overloads.push(PropertyInfo {
                    name: name.to_string(),
                    kind: PropKind::Member,
                    receiver: Some(Ty::obj_name(cn)),
                    formals: property_signature
                        .map(|signature| signature.formals.clone())
                        .or_else(|| {
                            getter_signature
                                .as_ref()
                                .map(|signature| signature.formals.clone())
                        })
                        .unwrap_or_default(),
                    ty,
                    context_count: 0,
                    context_param_names: Vec::new(),
                    getter,
                    setter,
                    setter_visibility: mp.visibility,
                    is_const: mp.is_const,
                    visibility: mp.visibility,
                    owner: cn,
                    receiver_rank: 0,
                    source_key: None,
                    source_member: None,
                });
            }
        }
        if overloads.is_empty()
            && (self.cp.builtin_member_is_property_name(cn, name)
                || mapped_builtin_property(cn, name))
        {
            if let Some(function) = functions
                .overloads
                .iter()
                .find(|function| function.callable.params.is_empty())
            {
                let mut getter = function.callable.clone();
                let ty = function
                    .generic_sig
                    .as_ref()
                    .map(|signature| signature.ret)
                    .unwrap_or_else(|| function.ret.apply(getter.ret));
                getter.ret = ty;
                overloads.push(PropertyInfo {
                    name: name.to_string(),
                    kind: PropKind::Member,
                    receiver: Some(recv),
                    formals: Vec::new(),
                    ty,
                    context_count: 0,
                    context_param_names: Vec::new(),
                    getter,
                    setter: None,
                    setter_visibility: function.visibility,
                    is_const: false,
                    visibility: function.visibility,
                    owner: function.callable.owner,
                    receiver_rank: 0,
                    source_key: None,
                    source_member: None,
                });
            }
        }
        const ACC_STATIC: u16 = 0x0008;
        const ACC_PUBLIC: u16 = 0x0001;
        let declares_instance_field = self.cp.find_name(cn).is_some_and(|class| {
            class.fields.iter().any(|field| {
                field.name == name
                    && field.access & ACC_STATIC == 0
                    && field.access & ACC_PUBLIC != 0
            })
        });
        if overloads.is_empty() && !declares_instance_field {
            for getter_name in self.physical_property_getter_names(name) {
                for function in self.member_functions(recv, &getter_name).overloads {
                    if !function.callable.params.is_empty()
                        || function.callable.ret == Ty::Unit
                        || function.ret.apply(function.callable.ret) == Ty::Unit
                    {
                        continue;
                    }
                    let visibility = function.visibility;
                    let owner = function.callable.owner;
                    let mut getter = function.callable;
                    let ty = function
                        .generic_sig
                        .as_ref()
                        .map(|signature| signature.ret)
                        .unwrap_or_else(|| function.ret.apply(getter.ret));
                    getter.ret = ty;
                    let setter_name = crate::names::property_setter_name(name);
                    let setter = self
                        .member_functions(recv, &setter_name)
                        .overloads
                        .into_iter()
                        .find(|setter| {
                            setter.callable.params.len() == 1
                                && setter.callable.ret == Ty::Unit
                                // Java synthetic-property pairing is declaration-shape matching,
                                // not call overload selection. Publish only the setter whose single
                                // value parameter is the getter's exact semantic storage type; core
                                // assignability belongs above this provider boundary.
                                && self.library_value_form(ty)
                                    == self.library_value_form(setter.callable.params[0])
                        });
                    let setter_visibility = setter
                        .as_ref()
                        .map_or(visibility, |setter| setter.visibility);
                    let setter = setter.map(|setter| setter.callable);
                    overloads.push(PropertyInfo {
                        name: name.to_string(),
                        kind: PropKind::Member,
                        receiver: Some(recv),
                        formals: Vec::new(),
                        ty,
                        context_count: 0,
                        context_param_names: Vec::new(),
                        getter,
                        setter,
                        setter_visibility,
                        is_const: false,
                        visibility,
                        owner,
                        receiver_rank: 0,
                        source_key: None,
                        source_member: None,
                    });
                }
            }
        }
        crate::libraries::Callables::from_parts(functions, PropertySet { overloads })
    }

    /// Build the classifier half of the provider's unified symbol record.
    fn classifier_record(&self, internal_name: TypeName) -> Option<std::sync::Arc<LibraryType>> {
        if let Some(building) = self.building_types.borrow().get(&internal_name) {
            return Some(building.clone());
        }
        if let Some(hit) = self.cp.cached_library_type_name(internal_name) {
            return hit;
        }
        // Nested builtin classifiers are stored source-facing in `.kotlin_builtins` (`Outer.Inner`),
        // while the qualifier walk's structural identity uses `$`. Canonicalize from the decoded
        // classifier table itself; no rendered-name rewrite or synthetic classifier is involved.
        if let Some(canonical) = self
            .cp
            .builtin_classifier_name(internal_name)
            .filter(|canonical| *canonical != internal_name)
        {
            let built = self.classifier_record(canonical).map(|shape| {
                let mut shape = (*shape).clone();
                shape.alias_target = Some(shape.alias_target.unwrap_or(canonical));
                std::sync::Arc::new(shape)
            });
            self.cp
                .cache_library_type_name(internal_name, built.clone());
            return built;
        }
        // A classpath `typealias` (`kotlin/collections/ArrayList` → `java/util/ArrayList`) has no class of
        // its own; resolve the underlying type and tag it with `alias_target` so name resolution records
        // the real internal.
        let built = if let Some(target) = self.cp.type_alias_target_name(internal_name) {
            self.classifier_record(target).map(|rc| {
                let mut t = (*rc).clone();
                t.alias_target = Some(target);
                std::sync::Arc::new(t)
            })
        } else {
            self.builtins_customizer
                .customize(internal_name, self.build_library_type(internal_name))
                .map(|mut classifier| {
                    let raw = std::sync::Arc::new(classifier.clone());
                    self.building_types.borrow_mut().insert(internal_name, raw);
                    let receiver = Ty::obj_name(internal_name);
                    for name in self.member_scope_names(internal_name, &classifier) {
                        let declarations = self.declared_callables_for(receiver, &name);
                        crate::trace_compiler!(
                            "member_slots",
                            "classifier callable record owner={internal_name:?} name={name} members={:?}",
                            declarations
                                .functions()
                                .iter()
                                .map(|function| (
                                    function.callable.name.as_str(),
                                    function.call_sig.param_names.as_slice(),
                                ))
                                .collect::<Vec<_>>(),
                        );
                        if !matches!(declarations, crate::libraries::Callables::None) {
                            classifier.declared_callables.insert(name, declarations);
                        }
                    }
                    self.building_types.borrow_mut().remove(&internal_name);
                    crate::libraries::add_core_builtin_declarations(&mut classifier, internal_name);
                    std::sync::Arc::new(classifier)
                })
        };
        self.cp
            .cache_library_type_name(internal_name, built.clone());
        built
    }

    fn symbols(
        &self,
        namespace: SymbolNamespace,
        name: &str,
    ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
        use crate::libraries::{Callables, ResolvedSymbols};
        if let Some(cached) = self.cp.cached_symbols(namespace, name) {
            return cached;
        }
        let namespace_name = namespace.name();
        let namespace_text = namespace_name.render();
        let fqn = if namespace_text.is_empty() {
            name.to_string()
        } else {
            format!("{namespace_text}/{name}")
        };
        let classifier_fqn = match namespace {
            SymbolNamespace::Package(_) => fqn.clone(),
            SymbolNamespace::Classifier(_) => format!("{namespace_text}${name}"),
        };
        // Classifier namespace: the class/interface/object (or a typealias's target) at the fqn.
        let alias_target = self.cp.type_alias_target_text(&classifier_fqn);
        let classifier_name = match namespace {
            SymbolNamespace::Package(package) => {
                crate::types::existing_type_name_child(package, name)
            }
            SymbolNamespace::Classifier(owner) => {
                crate::types::existing_type_name_nested_child(owner, name)
            }
        }
        .or_else(|| self.builtins_customizer.classifier_name(&classifier_fqn))
        .or_else(|| fictitious_function_class_name(&classifier_fqn))
        .or_else(|| self.cp.builtin_classifier_name_text(&classifier_fqn))
        .or_else(|| {
            // A raw namespace probe must not intern arbitrary property/function names. Promote the
            // spelling only after the classpath proves that an exact classifier exists.
            (self.cp.class_exists(&classifier_fqn) || alias_target.is_some())
                .then(|| type_name(&classifier_fqn))
        });
        let classifier = classifier_name.and_then(|internal| self.classifier_record(internal));
        let classifier_name = classifier.as_ref().map(|classifier| {
            classifier
                .alias_target
                .unwrap_or_else(|| classifier_name.expect("classifier identity"))
        });
        // Callable namespace, receiver-AGNOSTIC (resolution is by fqn; the receiver binds later, in the
        // consumer). Top-level functions of the source name declared in the fqn's package, plus the
        // package's extensions (source-keyed via the tree, so a `@JvmName`-mangled extension `sum` →
        // `sumOfInt` is found under its SOURCE name; the JVM name stays on the callable for emit).
        let package = match namespace {
            SymbolNamespace::Package(package) => Some(package),
            SymbolNamespace::Classifier(_) => None,
        };
        // Class-file extensions are returned here without a receiver and are read once from the
        // receiver-carrying declaration tree below. A package builtin with no physical JVM method has
        // no class-file entry; its metadata receiver is complete, so retain that semantic declaration.
        let mut overloads: Vec<_> = package
            .map(|package| {
                self.top_level_overloads(name, package)
                    .into_iter()
                    .filter(|o| {
                        o.kind == FnKind::TopLevel
                            || (o.kind == FnKind::Extension && o.receiver.is_some())
                    })
                    .collect()
            })
            .unwrap_or_default();
        // The parent of an explicit callable import may be a classifier rather than a package
        // (`import java.util.Arrays.asList`, `import C.factory`). Classifier metadata already exposes
        // the complete `Type.name(...)` family. Normalize that family into receiver-less import
        // candidates here, at the provider boundary, so qualified and imported spellings share the
        // ordinary argument mapper, generic binder, overload selector, and selected-call handoff.
        if let SymbolNamespace::Classifier(owner) = namespace {
            if let Some(classifier) = self.classifier_record(owner) {
                overloads.extend(
                    classifier
                        .classifier_callables(owner)
                        .into_iter()
                        .filter(|member| member.name == name && member.visibility.is_public())
                        .map(|member| {
                            FunctionInfo::classifier_member(FnKind::TopLevel, owner, member)
                        }),
                );
            }
        }
        // Extension PROPERTIES of the source name live in the CALLABLE namespace's property half. A name is
        // functions XOR a property, so these are surfaced separately and chosen when there are no functions.
        let mut props: Vec<PropertyInfo> = Vec::new();
        // The fqn's parent need not be a PACKAGE. `import kotlin.time.Duration.Companion.minutes` names a
        // member of an OBJECT, and Kotlin's rule is that importing one brings it into scope with that
        // object as its implicit dispatch receiver — so an object-like classifier is a legal parent of a
        // callable name, exactly like a package. Its member EXTENSIONS are surfaced here; the singleton is
        // recorded on the callable (`singleton_dispatch`) because the emit is an instance invoke on
        // `Owner.INSTANCE` / `Outer.Companion`, not a facade `invokestatic`.
        if let SymbolNamespace::Classifier(owner) = namespace {
            self.object_member_callables(owner, name, &mut overloads, &mut props);
        }
        // Extension discovery is @Metadata-driven (the source of truth), NOT a scan of JVM statics: the
        // package's PUBLIC facades' metadata carry each extension's SOURCE receiver, parameters, return
        // (with nullability), visibility, and generic signature. The JVM method (`@JvmName`-mangled name +
        // descriptor) is only the emit handle, rooted at the PUBLIC facade — kotlinc's `invokestatic`
        // target — so a package-private multifile PART never leaks a `false` visibility. Receiver-coupled
        // JVM specifics (element-variant `sumOfInt`, value-class mangling) are the emitter's job.
        for facade in package
            .into_iter()
            .flat_map(|package| self.cp.package_facades_name(package))
        {
            let facade_rendered = facade.render();
            let lambda_return_overload = self
                .cp
                .lambda_return_overloads(&facade_rendered)
                .contains(name);
            for mf in self.cp.meta_functions_name(facade).iter() {
                if mf.kotlin_name != name || !mf.is_extension() || mf.deprecated_hidden() {
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
                crate::trace_compiler!(
                    "resolve",
                    "extension emit handle {} metadata_params={:?} expected_descs={value_param_descs:?} selected={jvm_name}{descriptor}",
                    mf.kotlin_name,
                    mf.generic_sig.as_ref().map(|signature| &signature.params),
                );
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
                let Some((physical_params, pret)) = parse_method_desc(&descriptor) else {
                    continue;
                };
                if physical_params.is_empty() {
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
                let params = generic_sig
                    .as_ref()
                    .map(|signature| {
                        signature
                            .receiver
                            .into_iter()
                            .chain(signature.params.iter().copied())
                            .collect::<Vec<_>>()
                    })
                    .filter(|params| params.len() == physical_params.len())
                    .unwrap_or_else(|| physical_params.clone());
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
                let mut callable = LibraryCallable {
                    inline,
                    suspend: mf.is_suspend(),
                    source_receiver,
                    context_count: mf.context_count(),
                    contract: mf.contract.clone(),
                    generic_sig: generic_sig.clone().map(Box::new),
                    // Carry the resolved bytecode method's generic `Signature` — a `<reified T>` extension's
                    // splice reads its formal-type-parameter NAMES from here to bind the call's explicit
                    // type arguments. Without it the reified body cannot be specialized and the call falls
                    // back to a (throwing) direct invoke of the inline-only method.
                    signature: cand.as_ref().and_then(|c| c.signature.clone()),
                    ..LibraryCallable::library(facade, jvm_name, params, ret, pret, descriptor)
                };
                callable.physical_params = physical_params;
                if call_sig.param_defaults.iter().any(|default| *default) {
                    callable.default_realization =
                        self.top_level_default_realization(&callable).map(Box::new);
                }
                callable.inline_body_plan = self.inline_body_plan(&callable).map(Box::new);
                overloads.push(FunctionInfo {
                    ret: ReturnInfo::new(mf.ret_nullable(), ret_class),
                    visibility: mf.visibility,
                    generic_sig,
                    context_count: mf.context_count(),
                    flags: FnFlags {
                        inline,
                        reified: mf.has_reified_type_params(),
                        suspend: mf.is_suspend(),
                        operator: mf.is_operator(),
                        infix: mf.is_infix(),
                        is_abstract: false,
                        low_priority: mf.low_priority(),
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
            let matching_properties = mprops
                .iter()
                .filter(|property| property.name == name)
                .count();
            if matching_properties != 0 {
                crate::trace_compiler!(
                    "metadata_properties",
                    "property symbols fqn={fqn} facade={} decoded={} matching={matching_properties}",
                    facade.render(),
                    mprops.iter().count(),
                );
            }
            for mp in mprops.iter() {
                if mp.name != name {
                    continue; // this property name
                }
                // The accessors' receiver parameter: present iff the property is an extension.
                let receiver_params = usize::from(mp.is_extension);
                let mp = mp.clone();
                let property_gsig = mp.generic_sig.clone();
                let Some(getter_sig) = mp.getter else {
                    crate::trace_compiler!(
                        "metadata_properties",
                        "property {fqn} rejected: metadata has no getter realization"
                    );
                    continue;
                };
                let Some(getter_method) =
                    self.cp
                        .facade_static(&facade_rendered, &getter_sig.name, &getter_sig.desc)
                else {
                    crate::trace_compiler!(
                        "metadata_properties",
                        "property {fqn} rejected: getter {}{} is absent from facade {}",
                        getter_sig.name,
                        getter_sig.desc,
                        facade.render()
                    );
                    continue;
                };
                if !getter_method.public {
                    continue;
                }
                let Some((gparams, gret)) = parse_method_desc(&getter_sig.desc) else {
                    crate::trace_compiler!(
                        "metadata_properties",
                        "property {fqn} rejected: malformed getter descriptor {}",
                        getter_sig.desc
                    );
                    continue;
                };
                if gparams.len() != receiver_params {
                    crate::trace_compiler!(
                        "metadata_properties",
                        "property {fqn} rejected: getter parameter count {} != metadata receiver count {receiver_params}",
                        gparams.len()
                    );
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
                    getter_method.owner,
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
                    let setter_method = self.cp.facade_static(
                        &facade_rendered,
                        &setter_sig.name,
                        &setter_sig.desc,
                    )?;
                    if !setter_method.public {
                        return None;
                    }
                    Some(LibraryCallable::library(
                        setter_method.owner,
                        setter_sig.name,
                        sparams,
                        Ty::Unit,
                        sret,
                        setter_sig.desc,
                    ))
                });
                props.push(PropertyInfo {
                    name: name.to_string(),
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
                    context_param_names: Vec::new(),
                    getter,
                    setter,
                    setter_visibility: mp.visibility,
                    is_const: mp.is_const,
                    visibility: mp.visibility,
                    owner: facade,
                    receiver_rank: 0,
                    source_key: None,
                    source_member: None,
                });
            }
        }
        if matches!(namespace, SymbolNamespace::Package(package)
            if package.matches("kotlin/coroutines/intrinsics"))
            && name == "COROUTINE_SUSPENDED"
        {
            for property in &mut props {
                if property.kind == PropKind::TopLevel {
                    property.getter.compiler_intrinsic =
                        Some(crate::libraries::CompilerIntrinsic::CoroutineSuspended);
                }
            }
        }
        if matches!(namespace, SymbolNamespace::Package(package) if package.matches("kotlin"))
            && name == "code"
        {
            for property in &mut props {
                if property.kind == PropKind::Extension && property.receiver == Some(Ty::Char) {
                    property.getter.compiler_intrinsic =
                        Some(crate::libraries::CompilerIntrinsic::CharCode);
                }
            }
        }
        let compiler_intrinsic = match namespace {
            SymbolNamespace::Package(package) if package.matches("kotlin/coroutines") => match name
            {
                "suspendCoroutine" => Some(crate::libraries::CompilerIntrinsic::SuspendCoroutine),
                "startCoroutine" => Some(crate::libraries::CompilerIntrinsic::StartCoroutine),
                _ => None,
            },
            SymbolNamespace::Package(package)
                if package.matches("kotlin/coroutines/intrinsics") =>
            {
                match name {
                    "suspendCoroutineUninterceptedOrReturn" => Some(
                        crate::libraries::CompilerIntrinsic::SuspendCoroutineUninterceptedOrReturn,
                    ),
                    _ => None,
                }
            }
            SymbolNamespace::Package(package) if package.matches("kotlin/io") => match name {
                "print" => Some(crate::libraries::CompilerIntrinsic::Print),
                "println" => Some(crate::libraries::CompilerIntrinsic::Println),
                _ => None,
            },
            SymbolNamespace::Package(package) if package.matches("kotlin") => match name {
                "assert" => Some(crate::libraries::CompilerIntrinsic::Assert),
                "enumValues" => Some(crate::libraries::CompilerIntrinsic::EnumValues),
                "enumValueOf" => Some(crate::libraries::CompilerIntrinsic::EnumValueOf),
                _ => None,
            },
            SymbolNamespace::Package(package) if package.matches("kotlin/test") => match name {
                "assertFailsWith" => Some(crate::libraries::CompilerIntrinsic::AssertFailsWith),
                _ => None,
            },
            SymbolNamespace::Package(package)
                if package.matches("kotlin/collections") || package.matches("kotlin/text") =>
            {
                match name {
                    "forEach" => Some(crate::libraries::CompilerIntrinsic::ForEach),
                    "forEachIndexed" => Some(crate::libraries::CompilerIntrinsic::ForEachIndexed),
                    "map" if package.matches("kotlin/collections") => {
                        Some(crate::libraries::CompilerIntrinsic::Map)
                    }
                    "flatMap" if package.matches("kotlin/collections") => {
                        Some(crate::libraries::CompilerIntrinsic::FlatMap)
                    }
                    "isEmpty" if package.matches("kotlin/collections") => {
                        Some(crate::libraries::CompilerIntrinsic::IsEmpty)
                    }
                    "isNotEmpty" if package.matches("kotlin/collections") => {
                        Some(crate::libraries::CompilerIntrinsic::IsNotEmpty)
                    }
                    "count" if package.matches("kotlin/collections") => {
                        Some(crate::libraries::CompilerIntrinsic::Count)
                    }
                    "trimIndent" if package.matches("kotlin/text") => {
                        Some(crate::libraries::CompilerIntrinsic::TrimIndent)
                    }
                    "trimMargin" if package.matches("kotlin/text") => {
                        Some(crate::libraries::CompilerIntrinsic::TrimMargin)
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(intrinsic) = compiler_intrinsic {
            let declaration_package = match namespace {
                SymbolNamespace::Package(package) => Some(package),
                SymbolNamespace::Classifier(_) => None,
            };
            for overload in &mut overloads {
                let selected_declaration_kind = match intrinsic {
                    crate::libraries::CompilerIntrinsic::Print
                    | crate::libraries::CompilerIntrinsic::Println
                    | crate::libraries::CompilerIntrinsic::Assert
                    | crate::libraries::CompilerIntrinsic::AssertFailsWith
                    | crate::libraries::CompilerIntrinsic::CoroutineSuspended
                    | crate::libraries::CompilerIntrinsic::SuspendCoroutine
                    | crate::libraries::CompilerIntrinsic::SuspendCoroutineUninterceptedOrReturn
                    | crate::libraries::CompilerIntrinsic::EnumValues
                    | crate::libraries::CompilerIntrinsic::EnumValueOf => FnKind::TopLevel,
                    crate::libraries::CompilerIntrinsic::ArraySize
                    | crate::libraries::CompilerIntrinsic::CharCode
                    | crate::libraries::CompilerIntrinsic::StringLength => continue,
                    crate::libraries::CompilerIntrinsic::ForEach
                    | crate::libraries::CompilerIntrinsic::ForEachIndexed
                    | crate::libraries::CompilerIntrinsic::StartCoroutine
                    | crate::libraries::CompilerIntrinsic::Map
                    | crate::libraries::CompilerIntrinsic::FlatMap
                    | crate::libraries::CompilerIntrinsic::IsEmpty
                    | crate::libraries::CompilerIntrinsic::IsNotEmpty
                    | crate::libraries::CompilerIntrinsic::Count
                    | crate::libraries::CompilerIntrinsic::TrimIndent
                    | crate::libraries::CompilerIntrinsic::TrimMargin
                    | crate::libraries::CompilerIntrinsic::StringPlus
                    | crate::libraries::CompilerIntrinsic::NullableAnyToString => FnKind::Extension,
                };
                if overload.kind == selected_declaration_kind {
                    overload.callable.compiler_intrinsic = Some(intrinsic);
                    if matches!(
                        intrinsic,
                        crate::libraries::CompilerIntrinsic::ForEach
                            | crate::libraries::CompilerIntrinsic::ForEachIndexed
                            | crate::libraries::CompilerIntrinsic::Map
                            | crate::libraries::CompilerIntrinsic::FlatMap
                    ) {
                        overload.iterator_protocol_scope =
                            declaration_package.into_iter().collect();
                    }
                }
            }
        }
        let platform_callables = match (overloads.is_empty(), props.is_empty()) {
            (false, false) => Callables::Both {
                functions: FunctionSet { overloads },
                properties: PropertySet { overloads: props },
            },
            (false, true) => Callables::Functions(FunctionSet { overloads }),
            (true, false) => Callables::Properties(PropertySet { overloads: props }),
            (true, true) => Callables::None,
        };
        // Language builtin classifiers exist independently of whether the target classpath contains
        // stdlib. Federate that classifier source with the platform record here; platform metadata wins
        // when present, while callables remain exclusively metadata/platform declarations.
        let core = EmptySymbolSource.symbols(namespace, name);
        let (classifier_name, classifier) = if classifier.is_some() {
            (classifier_name, classifier)
        } else {
            (core.classifier_name, core.classifier.clone())
        };
        let callables = if matches!(platform_callables, Callables::None) {
            core.callables.clone()
        } else {
            platform_callables
        };
        self.cp.memoize_symbols(
            namespace,
            name,
            ResolvedSymbols {
                classifier_name,
                classifier,
                callables,
            },
        )
    }
}

impl SymbolSource for JvmLibraries {
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        JvmLibraries::package_exists(self, parent, name)
    }

    fn symbols(
        &self,
        namespace: SymbolNamespace,
        name: &str,
    ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
        JvmLibraries::symbols(self, namespace, name)
    }
}

impl JvmLibraries {
    fn inline_body_plan(&self, callable: &LibraryCallable) -> Option<InlineBodyPlan> {
        if !callable.inline.can_inline() {
            return None;
        }
        let body_descriptor = inline_body_descriptor(callable)?;
        // Every candidate overload the provider builds computes a plan, so the decode below —
        // body read, disassembly, invoke-site analysis — is memoized per declaration. The key must
        // carry EVERY input the decode reads: besides the bytecode locator, the callable's
        // physical slot layout and `$default` bridge — the same JVM method surfaces through
        // several provider channels (plain, suspend facade, extension) whose plans differ in
        // exactly those parameter indexes.
        let parameter_slots = callable_parameter_slots(&callable.physical_params);
        let default_descriptor = callable
            .default_realization
            .as_deref()
            .map(|realization| realization.descriptor.as_str());
        if let Some(plan) = self.cp.cached_inline_plan(
            callable.owner,
            &callable.name,
            &body_descriptor,
            &parameter_slots,
            default_descriptor,
        ) {
            return plan.map(|boxed| *boxed);
        }
        let mut body_unavailable = false;
        let plan = self.inline_body_plan_uncached(
            callable,
            &body_descriptor,
            &parameter_slots,
            &mut body_unavailable,
        );
        // "No plan" is only a memoizable FACT when it was decoded from bytes actually read. A
        // failed body read (archive open/read error under load, a jar changing mid-run) must stay
        // transient — publishing it into the per-entry global map would suppress the plan for
        // every later compile sharing the jar (the body cache guards the same hazard one level
        // down: "only a SUCCESSFUL read may populate the process-global cache").
        if !body_unavailable {
            self.cp.memoize_inline_plan(
                callable.owner,
                &callable.name,
                &body_descriptor,
                &parameter_slots,
                default_descriptor,
                plan.clone().map(Box::new),
            );
        }
        plan
    }

    /// Decode one callable's inline-body plan from bytecode. `body_unavailable` is set (and `None`
    /// returned) when a body READ failed — the caller must not memoize that answer; a `None` with
    /// the flag clear is a decoded "no expandable shape", which is a stable fact of the bytes.
    fn inline_body_plan_uncached(
        &self,
        callable: &LibraryCallable,
        body_descriptor: &str,
        parameter_slots: &[u16],
        body_unavailable: &mut bool,
    ) -> Option<InlineBodyPlan> {
        let owner = callable.owner.render();
        let inline_name = format!("{}$$forInline", callable.name);
        let Some(body) = self
            .cp
            .method_code(&owner, &inline_name, body_descriptor)
            .or_else(|| self.cp.method_code(&owner, &callable.name, body_descriptor))
        else {
            *body_unavailable = true;
            return None;
        };
        let instructions = crate::jvm::inline::disassemble(&body.code)?;
        let parameter_at = |slot: u16| {
            parameter_slots
                .iter()
                .position(|candidate| *candidate == slot)
        };
        let invoke_sites =
            crate::jvm::inline::function_invoke_sites(&instructions, &body.source_cp);
        let [invoke] = invoke_sites.as_slice() else {
            return None;
        };
        let invoke_loads = instructions[..*invoke]
            .iter()
            .rev()
            .map_while(crate::jvm::inline::loaded_local)
            .collect::<Vec<_>>();
        // JVM invocation operands are loaded receiver-first. Walking backward therefore sees
        // arguments first and the function object last.
        let (&lambda_slot, invoke_argument_slots) = invoke_loads.split_last()?;
        let lambda_parameter = parameter_at(lambda_slot)?;

        let calls = instructions
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| {
                let target = crate::jvm::inline::invoked_method(instruction, &body.source_cp)?;
                (!target.0.starts_with("kotlin/jvm/internal/")
                    && !target.0.starts_with("kotlin/jvm/functions/"))
                .then_some((index, target))
            })
            .collect::<Vec<_>>();
        let enter = calls.iter().find(|(index, target)| {
            *index < *invoke && target.2.contains("Lkotlin/coroutines/Continuation;")
        });
        if enter.is_none() {
            if !calls.is_empty() {
                return None;
            }
            let argument_parameters = invoke_argument_slots
                .iter()
                .rev()
                .map(|slot| parameter_at(*slot))
                .collect::<Option<Vec<_>>>()?;
            let return_parameter = instructions
                .iter()
                .rev()
                .nth(1)
                .and_then(crate::jvm::inline::loaded_local)
                .and_then(parameter_at);
            return Some(InlineBodyPlan::InvokeLambda {
                lambda_parameter,
                argument_parameters,
                return_parameter,
            });
        }
        let enter = enter?;
        let cleanup_calls = calls
            .iter()
            .filter(|(index, target)| *index > *invoke && *target != enter.1)
            .collect::<Vec<_>>();
        let [cleanup, repeated] = cleanup_calls.as_slice() else {
            return None;
        };
        if cleanup.1 != repeated.1 {
            return None;
        }
        let receiver_slot = instructions[..enter.0]
            .iter()
            .rev()
            .filter_map(crate::jvm::inline::loaded_local)
            .nth(2)?;
        if parameter_at(receiver_slot)? != 0 {
            return None;
        }
        let state_slot = instructions[..enter.0]
            .iter()
            .rev()
            .filter_map(crate::jvm::inline::loaded_local)
            .nth(1)?;
        let state_parameter = parameter_at(state_slot)?;
        match self.inline_default_is_null(callable, parameter_slots, state_parameter) {
            None => {
                *body_unavailable = true;
                return None;
            }
            Some(false) => return None,
            Some(true) => {}
        }
        let enter = inline_plan_member(enter.1, true)?;
        let cleanup = inline_plan_member(cleanup.1, false)?;
        Some(InlineBodyPlan::SuspendBeforeLambdaFinally {
            lambda_parameter,
            state_parameter,
            state_default: crate::libraries::DefaultValue::Null,
            enter: Box::new(enter),
            cleanup: Box::new(cleanup),
        })
    }

    /// Whether the `$default` bridge stores `null` into `parameter`'s slot. `None` means the bridge
    /// body could not be READ (a transient failure the caller must not memoize); `Some(false)`
    /// covers every decoded negative, including "the callable has no `$default` bridge at all"
    /// (stable — the bridge descriptor is part of the plan cache key).
    fn inline_default_is_null(
        &self,
        callable: &LibraryCallable,
        parameter_slots: &[u16],
        parameter: usize,
    ) -> Option<bool> {
        let Some(realization) = callable.default_realization.as_deref() else {
            return Some(false);
        };
        let owner = callable.owner.render();
        let bridge_name = format!("{}$default", callable.name);
        // A failed bridge-body READ is the transient case the caller must not memoize.
        let body = self
            .cp
            .method_code(&owner, &bridge_name, &realization.descriptor)?;
        let Some(instructions) = crate::jvm::inline::disassemble(&body.code) else {
            return Some(false);
        };
        let Some(slot) = parameter_slots.get(parameter).copied() else {
            return Some(false);
        };
        Some(instructions.windows(2).any(|window| {
            matches!(window[0], crate::jvm::inline::Insn::Plain { op: 0x01, .. })
                && crate::jvm::inline::stored_local(&window[1]) == Some(slot)
        }))
    }

    fn top_level_default_realization(
        &self,
        callable: &LibraryCallable,
    ) -> Option<crate::libraries::DefaultCallRealization> {
        let bridge_name = format!("{}$default", callable.name);
        let (base_params, _) = parse_method_desc_with_field_params(&callable.descriptor)?;
        let is_continuation = |ty: Ty| {
            ty.obj_internal()
                .is_some_and(|name| name.matches("kotlin/coroutines/Continuation"))
        };
        let mut owner = Some(callable.owner);
        let mut seen = std::collections::HashSet::new();
        while let Some(current) = owner {
            if !seen.insert(current) {
                break;
            }
            let class = self.cp.find_name(current)?;
            crate::trace_compiler!(
                "default_semantics",
                "default realization owner={} scan={} bridge={} base={base_params:?} matching={:?}",
                callable.owner,
                current,
                bridge_name,
                class
                    .methods
                    .iter()
                    .filter(|method| method.name == bridge_name)
                    .map(|method| method.descriptor.as_str())
                    .collect::<Vec<_>>(),
            );
            if let Some(realization) = class.methods.iter().find_map(|method| {
                if !method.is_static() || method.name != bridge_name {
                    return None;
                }
                let (params, ret) = parse_method_desc_with_field_params(&method.descriptor)?;
                if !params.starts_with(&base_params) {
                    return None;
                }
                let mut suffix = &params[base_params.len()..];
                if callable.suspend {
                    let (continuation, rest) = suffix.split_first()?;
                    if !is_continuation(*continuation) {
                        return None;
                    }
                    suffix = rest;
                }
                let mask_count = base_params.len().div_ceil(32).max(1);
                if suffix.len() != mask_count + 1
                    || !suffix[..suffix.len() - 1]
                        .iter()
                        .all(|parameter| *parameter == Ty::Int)
                    || !suffix.last().copied().is_some_and(Ty::is_reference)
                {
                    return None;
                }
                Some(crate::libraries::DefaultCallRealization {
                    descriptor: method.descriptor.clone(),
                    real_params: callable.physical_params.clone(),
                    mask_count,
                    ret,
                    suspend: callable.suspend,
                })
            }) {
                return Some(realization);
            }
            owner = class.super_class;
        }
        None
    }

    fn member_default_realization(
        &self,
        owner: TypeName,
        member: &LibraryMember,
    ) -> Option<crate::libraries::DefaultCallRealization> {
        let class = self.cp.find_name(owner)?;
        let physical_name = member.physical_name.as_deref().unwrap_or(&member.name);
        let bridge_name = format!("{physical_name}$default");
        let (base_params, _) = parse_method_desc_with_field_params(&member.descriptor)?;
        let is_continuation = |ty: Ty| {
            ty.obj_internal()
                .is_some_and(|name| name.matches("kotlin/coroutines/Continuation"))
        };
        let suspend = member.suspend();
        let real_params = member.params.clone();
        class.methods.iter().find_map(|method| {
            if !method.is_static() || method.name != bridge_name {
                return None;
            }
            let (params, ret) = parse_method_desc_with_field_params(&method.descriptor)?;
            // A dispatched member's base descriptor excludes its receiver while its static
            // `$default` bridge prepends one. A provider-normalized direct realization (legacy
            // interface holder, value-class implementation method) already carries that receiver
            // in the base descriptor. Compare the exact physical prefix supplied by the selected
            // declaration instead of assuming one origin-specific layout.
            let prefix = match member.realization {
                crate::libraries::MemberRealization::Direct {
                    pass_receiver: true,
                } => params.as_slice(),
                _ => params.get(1..).unwrap_or_default(),
            };
            if !prefix.starts_with(&base_params) {
                return None;
            }
            let mut suffix = &prefix[base_params.len()..];
            if suspend {
                let (continuation, rest) = suffix.split_first()?;
                if !is_continuation(*continuation) {
                    return None;
                }
                suffix = rest;
            }
            let mask_count = member.params.len().div_ceil(32).max(1);
            if suffix.len() != mask_count + 1
                || !suffix[..suffix.len() - 1]
                    .iter()
                    .all(|parameter| *parameter == Ty::Int)
                || !suffix.last().copied().is_some_and(Ty::is_reference)
            {
                return None;
            }
            Some(crate::libraries::DefaultCallRealization {
                descriptor: method.descriptor.clone(),
                real_params: real_params.clone(),
                mask_count,
                ret,
                suspend,
            })
        })
    }

    fn member_functions(&self, receiver: Ty, name: &str) -> FunctionSet {
        // Exact declarations on this classifier. The resolver assigns inheritance distance.
        let mut overloads = Vec::new();
        if let Some(cn) = receiver.kotlin_class_internal() {
            if let Some(t) = self.classifier_record(cn) {
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
                            "member declaration {cn_rendered}.{} desc={} sig={:?}",
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
                        // and erases its return to `Object`; the provider-normalized member already
                        // exposes the logical parameter list. The coroutine pass re-derives the CPS
                        // form for emission.
                        let suspend = m.suspend();
                        let params = m.params.clone();
                        let descriptor = if suspend {
                            strip_continuation_param(&m.descriptor)
                        } else {
                            m.descriptor.clone()
                        };
                        let meta_name = m.physical_name.as_deref().unwrap_or(&m.name);
                        let metadata_ret = m.declared_ret.or_else(|| {
                            self.cp
                                .metadata_property_ret_ty_name(cn, meta_name, &m.descriptor)
                        });
                        let suspend_ret_nullable = suspend && m.ret_nullable();
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
                        let call_sig = m.call_sig.clone();
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
                        let physical_owner = m.owner.as_ref().copied().unwrap_or(cn);
                        let callable = LibraryCallable {
                            inline: m.inline,
                            suspend,
                            context_count: m.context_count,
                            member_realization: m.realization,
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
                            default_realization: self
                                .member_default_realization(physical_owner, m)
                                .map(Box::new),
                            ..LibraryCallable::library(
                                physical_owner,
                                m.physical_name.clone().unwrap_or_else(|| m.name.clone()),
                                params,
                                ret,
                                m.physical_ret,
                                descriptor,
                            )
                        };
                        let mut callable = callable;
                        callable.inline_body_plan = self.inline_body_plan(&callable).map(Box::new);
                        let inline_body_plan = callable.inline_body_plan.clone();
                        overloads.push(FunctionInfo {
                            ret: ReturnInfo::new(
                                m.ret_nullable()
                                    || builtin_ret_nullable
                                    || (suspend_ret_nullable && !ret.is_reference()),
                                None,
                            ),
                            visibility: m.visibility,
                            receiver_rank: 0,
                            overload_rank: descriptor_narrowing(&m.descriptor) as u32,
                            generic_sig,
                            call_sig,
                            context_count: m.context_count,
                            flags: FnFlags {
                                inline: m.inline,
                                reified: m.reified,
                                suspend,
                                operator: m.is_operator(),
                                infix: m.is_infix(),
                                is_abstract: m.is_abstract(),
                                low_priority: m.low_priority,
                            },
                            ..FunctionInfo::plain(FnKind::Member, Some(receiver), callable)
                        });
                        if let Some(function) = overloads.last_mut() {
                            // Keep the selected member record and callable handle structurally
                            // identical; either lowering seam may consume the declaration.
                            function.callable.inline_body_plan = inline_body_plan;
                        }
                    }
                }
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
                    .map(|ty| self.semanticize_jvm_type(ty))
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
            _ => self
                .classifier_record(ty.obj_internal()?)
                .and_then(|t| t.value_underlying),
        }
    }

    fn library_value_form(&self, ty: Ty) -> Ty {
        // A reference type erases to its JVM internal name (a Kotlin collection → its single
        // `java/util/*` interface) with type arguments dropped — exactly what a descriptor-read
        // constructor/method parameter carries. Arrays recurse into their element (`Array<Set<String>>`
        // → `[Ljava/util/Set;` on the descriptor side), so a nested collection element normalizes too.
        // Other kinds (primitives, `String`, function types) already compare exactly across the sides.
        match ty {
            // Kotlin scalar classifiers are semantic class identities in core. At this JVM boundary their
            // non-null value form is the scalar carrier (`kotlin/Int` -> `I`, `UInt` -> `I`), never the
            // wrapper class used by a nullable/generic position.
            _ if ty.scalar_value_repr().is_some() => ty.scalar_value_repr().unwrap(),
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

    fn type_alias_expansion(&self, internal: TypeName) -> Option<crate::libraries::AliasExpansion> {
        self.cp.type_alias_expansion(internal).map(
            |(target, formals, expansion, expansion_spelling)| crate::libraries::AliasExpansion {
                identity: internal,
                target: self.canonical_source_type_name(target),
                formals,
                expansion_spelling,
                // Metadata may name a mapped JVM collection as the expanded classifier. Normalize
                // the complete template at the provider boundary so core resolution only sees
                // source identities, including inside projections, function types, and nullability.
                expansion: canonicalize_jvm_collections(expansion),
            },
        )
    }

    fn canonical_source_type_name(&self, internal: TypeName) -> TypeName {
        // The frontend always reasons in the canonical Kotlin declaration space. A classpath lookup
        // may find the JVM realization (`java/lang/String`, `java/util/List`), but its source identity
        // is the one `.kotlin_builtins` record that declares the Kotlin API. Keeping the physical name
        // here loses members such as inherited `CharSequence.length` and creates a second class model.
        super::jvm_class_map::jvm_to_kotlin_builtin_metadata_name(internal).unwrap_or(internal)
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

    fn property_reference_type(&self, arity: usize, mutable: bool, args: &[Ty]) -> Option<Ty> {
        let internal = match (arity, mutable) {
            (0, false) => "kotlin/reflect/KProperty0",
            (0, true) => "kotlin/reflect/KMutableProperty0",
            (1, false) => "kotlin/reflect/KProperty1",
            (1, true) => "kotlin/reflect/KMutableProperty1",
            _ => return None,
        };
        // `KProperty0<V>` / `KProperty1<T, V>` has no useful raw semantic form: raw `get()` exposes
        // the declaration's unbound `V` to the checker. Require the complete source signature.
        if args.len() != arity + 1 || args.contains(&Ty::Error) {
            return None;
        }
        Some(Ty::obj_args(internal, args))
    }

    fn function_reference_type(&self, function: Ty) -> Option<Ty> {
        let Ty::Fun(signature) = function else {
            return None;
        };
        if signature.suspend || signature.has_receiver || signature.context_count != 0 {
            return None;
        }
        let mut arguments = signature.params.to_vec();
        arguments.push(signature.ret);
        let classifier = crate::types::type_name_child(
            type_name("kotlin/reflect"),
            &format!("KFunction{}", signature.params.len()),
        );
        Some(Ty::obj_args_name(classifier, &arguments))
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
        // `property_getter_name` deliberately preserves an `isX` spelling. Keep that candidate:
        // Java's `boolean isX()` is the getter for Kotlin's synthetic `isX` property, not evidence
        // that no getter exists.
        let mut candidates = vec![property_getter_name(property)];
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
        Some(type_descriptor(crate::jvm::ir_emit::ir_ty_to_jvm(&ty)))
    }

    fn ir_type_descriptor(&self, ty: Ty) -> Option<String> {
        Some(type_descriptor(crate::jvm::ir_emit::ir_ty_to_jvm(&ty)))
    }

    fn ir_value_type(&self, ty: Ty) -> Ty {
        crate::jvm::ir_emit::ir_ty_to_jvm(&ty)
    }

    fn method_descriptor(&self, params: &[Ty], ret: Ty) -> Option<String> {
        let params = crate::jvm::ir_emit::jvm_tys(params);
        let ret = crate::jvm::ir_emit::ir_ty_to_jvm(&ret);
        Some(method_descriptor(&params, ret))
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
        let physical = super::names::classfile_internal_name(internal);
        Some(PlatformField {
            owner: physical.clone(),
            name: "INSTANCE".to_string(),
            descriptor: format!("L{physical};"),
        })
    }

    fn companion_instance_field(
        &self,
        class_internal: &str,
        companion_internal: &str,
        field_name: &str,
    ) -> Option<PlatformField> {
        let physical_companion = super::names::classfile_internal_name(companion_internal);
        if super::jvm_class_map::intrinsic_companion_to_jvm(companion_internal).is_some() {
            return Some(PlatformField {
                owner: physical_companion.clone(),
                name: "INSTANCE".to_string(),
                descriptor: format!("L{physical_companion};"),
            });
        }
        Some(PlatformField {
            owner: super::names::classfile_internal_name(class_internal),
            name: field_name.to_string(),
            descriptor: format!("L{physical_companion};"),
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
            RuntimeOp::FloatingIsNaN | RuntimeOp::FloatingIsInfinite => {
                let (owner, primitive) = match ty {
                    Ty::Float => ("java/lang/Float", "F"),
                    Ty::Double => ("java/lang/Double", "D"),
                    _ => return None,
                };
                let name = match op {
                    RuntimeOp::FloatingIsNaN => "isNaN",
                    RuntimeOp::FloatingIsInfinite => "isInfinite",
                    _ => unreachable!(),
                };
                callable(
                    owner,
                    name,
                    vec![ty],
                    Ty::Boolean,
                    Ty::Boolean,
                    format!("({primitive})Z"),
                )
            }
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
}

/// Where an application of this classpath annotation, written with no use-site prefix, may land.
///
/// A KOTLIN annotation carries `@kotlin.annotation.Target(allowedTargets = …)` — the only form that
/// can name `PROPERTY` — and declaring none means "applicable everywhere". A JAVA `@interface`
/// carries `@java.lang.annotation.Target(ElementType…)`, and Java has no property: kotlinc places a
/// bare Java annotation written on a Kotlin property on its parameter or backing field instead.
fn classpath_annotation_targets(
    ci: &crate::jvm::classreader::ClassInfo,
) -> crate::types::AnnotationTargets {
    if !ci.kotlin_targets.is_empty() {
        return crate::types::AnnotationTargets {
            value_parameter: ci.kotlin_targets.iter().any(|t| t == "VALUE_PARAMETER"),
            property: ci.kotlin_targets.iter().any(|t| t == "PROPERTY"),
            field: ci.kotlin_targets.iter().any(|t| t == "FIELD"),
        };
    }
    // A Kotlin annotation is identified by its own `@Metadata`; without a declared `@Target` it is
    // applicable everywhere.
    if ci.meta.class_kind.is_some() {
        return crate::types::AnnotationTargets::DEFAULT;
    }
    // Java. An `@interface` with no `@Target` is applicable in every declaration context.
    let java_target = |name: &str| {
        ci.java_targets.is_empty() || ci.java_targets.iter().any(|entry| entry == name)
    };
    crate::types::AnnotationTargets {
        value_parameter: java_target("PARAMETER"),
        property: false,
        field: java_target("FIELD"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        desc_to_ty, fictitious_function_class, java_type_nullability, method_layout,
        overlay_metadata_collection_names, parse_class_gsig, parse_concrete_field_gsig,
        parse_field_gsig, parse_formals, parse_method_desc, parse_method_gsig,
    };
    use crate::libraries::{GenericReturnPolicy, SemanticPlatform};
    use crate::symbol_source::SymbolNamespace;
    use crate::types::type_name;
    use crate::types::Ty;

    #[test]
    fn function_class_provider_recognizes_only_the_builtin_classifier_namespace() {
        assert!(fictitious_function_class(type_name("kotlin/reflect/KFunction23")).is_some());
        assert!(fictitious_function_class(type_name("kotlin/reflect/KFunction")).is_none());
        assert!(fictitious_function_class(type_name("other/KFunction1")).is_none());
        assert!(fictitious_function_class(type_name("kotlin/reflect/KFunctionX")).is_none());
    }

    #[test]
    fn runtime_function_classifier_takes_callable_shape_from_invoke_metadata() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let classifier = libraries
            .classifier_record(type_name("kotlin/Function1"))
            .expect("semantic Function1 classifier");
        let Ty::Fun(signature) = classifier.callable_signature.expect("invoke signature") else {
            panic!("Function1 callable signature is not a function")
        };
        assert_eq!(signature.params.len(), 1);
    }

    #[test]
    fn mapped_number_members_publish_only_their_kotlin_names() {
        let (Some(stdlib), Some(jdk)) = (
            crate::toolchain::stdlib_jar(),
            crate::toolchain::jdk_modules(),
        ) else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib, jdk]),
        ));
        let classifier = libraries
            .classifier_record(type_name("java/lang/Number"))
            .expect("Number classifier");
        assert!(classifier
            .members
            .iter()
            .any(|member| member.name == "toInt"
                && member.physical_name.as_deref() == Some("intValue")));
        assert!(
            classifier
                .members
                .iter()
                .all(|member| member.name != "intValue"),
            "provider boundary must not leak the physical Java spelling"
        );
    }

    #[test]
    fn mapped_mutable_list_publishes_its_applied_read_only_supertype() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let actual = Ty::obj_args("kotlin/collections/MutableList", &[Ty::String]);
        let supertypes = crate::symbol_resolver::direct_supertypes(&libraries, actual);
        assert!(
            supertypes.contains(&Ty::obj_args("kotlin/collections/List", &[Ty::String])),
            "MutableList<String> direct supertypes: {supertypes:?}"
        );
        assert!(crate::symbol_resolver::resolution_subtype(
            &libraries,
            actual,
            Ty::obj_args("kotlin/collections/List", &[Ty::String]),
        ));
    }

    #[test]
    fn concrete_java_collection_keeps_its_kotlin_interface_faces() {
        let (Some(stdlib), Some(jdk)) = (
            crate::toolchain::stdlib_jar(),
            crate::toolchain::jdk_modules(),
        ) else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib, jdk]),
        ));
        let classifier = libraries
            .classifier_record(type_name("java/util/ArrayList"))
            .expect("ArrayList classifier");
        assert!(
            classifier
                .supertypes
                .contains("kotlin/collections/MutableList"),
            "ArrayList supertypes: {:?}",
            classifier.supertypes
        );
    }

    #[test]
    fn java_generic_static_sam_retains_its_declared_nullable_bound() {
        let Some(jdk) = crate::toolchain::jdk_modules() else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![jdk]),
        ));
        let classifier = libraries
            .classifier_record(type_name("java/lang/ThreadLocal"))
            .expect("ThreadLocal classifier");
        let callable = classifier
            .companion
            .iter()
            .find(|member| member.name == "withInitial")
            .expect("ThreadLocal.withInitial");
        let parameter = callable
            .generic_sig
            .as_ref()
            .and_then(|signature| signature.params.first())
            .copied()
            .expect("generic Supplier parameter");
        let sam = crate::symbol_resolver::semantic_sam_signature(&libraries, parameter)
            .expect("Supplier SAM declaration");
        assert!(
            crate::symbol_resolver::sam_return_matches(&libraries, &libraries, sam.ret, Ty::Null,),
            "withInitial parameter={parameter:?}, SAM return={:?}",
            sam.ret,
        );

        let specialized_parameter = Ty::platform_nullable(Ty::obj_args(
            "java/util/function/Supplier",
            &[Ty::out_projection(Ty::nullable(Ty::String))],
        ));
        let supplier = libraries
            .classifier_record(type_name("java/util/function/Supplier"))
            .expect("Supplier classifier");
        assert!(
            supplier
                .type_param_bounds()
                .first()
                .is_some_and(|bounds| bounds.iter().all(|bound| bound.upper_bound_admits_null())),
            "Java Supplier bounds must retain platform nullability: {:?}",
            supplier.type_param_bounds()
        );
        let specialized =
            crate::symbol_resolver::semantic_sam_signature(&libraries, specialized_parameter)
                .expect("specialized Supplier SAM declaration");
        assert_eq!(
            specialized.ret,
            Ty::nullable(Ty::String),
            "specialized Supplier return must retain the applied nullable type argument"
        );
    }

    #[test]
    fn mutable_property_classifier_preserves_applied_set_parameter_types() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let classifier = libraries
            .classifier_record(type_name("kotlin/reflect/KMutableProperty1"))
            .expect("KMutableProperty1 classifier");
        assert_eq!(classifier.type_params, ["T", "V"]);
        assert!(
            classifier
                .type_param_bounds
                .iter()
                .all(|bounds| bounds.iter().all(|bound| bound.is_nullable())),
            "reflection property parameters must admit nullable applications: {:?}",
            classifier.type_param_bounds
        );
        let declared_set = classifier
            .declared_callables
            .get("set")
            .and_then(|callables| {
                callables
                    .functions()
                    .iter()
                    .find(|function| function.semantic_params().len() == 2)
            })
            .expect("declared set(T, V)");
        assert!(
            matches!(declared_set.semantic_params()[1], Ty::TyParam("V", bound) if bound.is_nullable()),
            "declared set value parameter: {:?}",
            declared_set.semantic_params()[1]
        );
        let callables = crate::symbol_resolver::SymbolResolver::new(&libraries).receiver_callables(
            Ty::obj_args(
                "kotlin/reflect/KMutableProperty1",
                &[Ty::String, Ty::nullable(Ty::Int)],
            ),
            "set",
        );
        let set = callables
            .functions()
            .iter()
            .find(|function| function.semantic_params().len() == 2)
            .expect("set(T, V)");
        assert_eq!(set.semantic_params(), [Ty::String, Ty::nullable(Ty::Int)]);

        assert_eq!(
            crate::symbol_resolver::classifier_callable_signature(
                &libraries,
                Ty::obj_args("kotlin/reflect/KProperty1", &[Ty::String, Ty::Int]),
            ),
            Some(Ty::fun(vec![Ty::String], Ty::Int)),
        );
    }

    #[test]
    fn metadata_inline_bodies_decode_parameter_roles_without_source_dispatch() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let symbols = libraries.symbols(SymbolNamespace::Package(type_name("kotlin")), "let");
        let decoded = symbols
            .callables
            .functions()
            .iter()
            .map(|function| {
                (
                    function.callable.owner,
                    function.callable.descriptor.as_str(),
                    function.callable.inline,
                    function.callable.inline_body_plan.as_deref(),
                )
            })
            .collect::<Vec<_>>();
        assert!(
            symbols.callables.functions().iter().any(|function| {
                matches!(
                    function.callable.inline_body_plan.as_deref(),
                    Some(crate::libraries::InlineBodyPlan::InvokeLambda {
                        lambda_parameter: 1,
                        argument_parameters,
                        ..
                    }) if argument_parameters.as_slice() == [0]
                )
            }),
            "decoded declarations: {decoded:?}"
        );
    }

    #[test]
    fn suspend_finally_inline_body_decodes_exact_member_handles() {
        let (Some(stdlib), Some(coroutines)) = (
            crate::toolchain::stdlib_jar(),
            crate::toolchain::coroutines_jar(),
        ) else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib, coroutines]),
        ));
        let symbols = libraries.symbols(
            SymbolNamespace::Package(type_name("kotlinx/coroutines/sync")),
            "withLock",
        );
        assert!(symbols.callables.functions().iter().any(|function| {
            matches!(
                function.callable.inline_body_plan.as_deref(),
                Some(crate::libraries::InlineBodyPlan::SuspendBeforeLambdaFinally {
                    lambda_parameter: 2,
                    state_parameter: 1,
                    enter,
                    cleanup,
                    ..
                }) if enter.suspend() && !cleanup.suspend()
            )
        }));
    }

    #[test]
    fn builtin_int_companion_is_one_structural_classifier_with_declared_constants() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let companion = type_name("kotlin/Int$Companion");
        let classifier = libraries
            .classifier_record(companion)
            .expect("Int.Companion classifier from builtins metadata");
        assert!(
            classifier.constants.contains_key("MAX_VALUE"),
            "Int.Companion fields={:?}, constants={:?}",
            classifier
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            classifier.constants.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn stdlib_type_of_is_an_ordinary_metadata_callable() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let symbols = libraries.symbols(
            SymbolNamespace::Package(type_name("kotlin/reflect")),
            "typeOf",
        );
        let functions = match &symbols.callables {
            crate::libraries::Callables::Functions(functions)
            | crate::libraries::Callables::Both { functions, .. } => functions,
            _ => panic!("typeOf callable missing"),
        };
        let type_of = functions
            .overloads
            .iter()
            .find(|function| function.callable.name == "typeOf")
            .expect("typeOf overload");
        assert!(type_of.callable.params.is_empty());
        assert_eq!(type_of.callable.ret, Ty::obj("kotlin/reflect/KType"));
        assert!(type_of.flags.inline.can_inline());
        assert_eq!(
            type_of
                .generic_sig
                .as_ref()
                .map(|signature| signature.formals.as_slice()),
            Some(["T".to_string()].as_slice())
        );
        let scope = [type_name("kotlin/reflect")];
        let selected = crate::symbol_resolver::SymbolResolver::new_scoped(&libraries, &scope)
            .resolve_symbol(
                crate::symbol_resolver::SymRecv::TopLevel,
                "typeOf",
                &[],
                &[Ty::String],
            )
            .and_then(crate::symbol_resolver::Symbol::top_level_call)
            .expect("typeOf<String>() must select from its imported package");
        assert_eq!(selected.ret, Ty::obj("kotlin/reflect/KType"));
    }

    #[test]
    fn stdlib_unsigned_range_operators_are_ordinary_metadata_extensions() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let symbols =
            libraries.symbols(SymbolNamespace::Package(type_name("kotlin/ranges")), "step");
        let functions = match &symbols.callables {
            crate::libraries::Callables::Functions(functions)
            | crate::libraries::Callables::Both { functions, .. } => functions,
            _ => panic!("kotlin.ranges.step callable missing"),
        };
        assert!(
            functions.overloads.iter().any(|function| {
                function.kind == crate::libraries::FnKind::Extension
                    && function.semantic_receiver().is_some_and(|receiver| {
                        receiver == Ty::obj("kotlin/ranges/UIntProgression")
                    })
                    && function.semantic_params() == [Ty::Int]
            }),
            "decoded step overloads: {:?}",
            functions
                .overloads
                .iter()
                .map(|function| (
                    function.callable.name.as_str(),
                    function.semantic_receiver(),
                    function.semantic_params()
                ))
                .collect::<Vec<_>>()
        );
        let scope = [type_name("kotlin/ranges")];
        let selected = crate::symbol_resolver::SymbolResolver::new_scoped(&libraries, &scope)
            .resolve_symbol(
                crate::symbol_resolver::SymRecv::Value(Ty::obj("kotlin/ranges/UIntRange")),
                "step",
                &[Ty::Int],
                &[],
            )
            .and_then(crate::symbol_resolver::Symbol::extension_call)
            .expect("UIntRange.step(Int) must select from parsed stdlib metadata");
        assert_eq!(selected.ret, Ty::obj("kotlin/ranges/UIntProgression"));

        let ubyte_symbols =
            libraries.symbols(SymbolNamespace::Package(type_name("kotlin")), "UByte");
        let ubyte = ubyte_symbols.classifier.as_ref().expect("UByte classifier");
        let ubyte_range_members = ubyte
            .members
            .iter()
            .filter(|member| member.name == "rangeTo")
            .map(|member| {
                (
                    member.params.clone(),
                    member.ret,
                    member.physical_name.clone(),
                    member.descriptor.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert!(
            ubyte_range_members.iter().any(|(params, ret, _, _)| {
                params == &[Ty::obj("kotlin/UByte")] && *ret == Ty::obj("kotlin/ranges/UIntRange")
            }),
            "UByte.rangeTo declarations: {ubyte_range_members:?}"
        );

        let ubyte_range = crate::symbol_resolver::SymbolResolver::new_scoped(&libraries, &scope)
            .resolve_symbol(
                crate::symbol_resolver::SymRecv::Value(Ty::obj("kotlin/UByte")),
                "rangeTo",
                &[Ty::obj("kotlin/UByte")],
                &[],
            )
            .and_then(crate::symbol_resolver::Symbol::call)
            .expect("UByte.rangeTo(UByte) must select from parsed stdlib metadata");
        assert_eq!(ubyte_range.ret, Ty::obj("kotlin/ranges/UIntRange"));

        let ushort_range = crate::symbol_resolver::SymbolResolver::new_scoped(&libraries, &scope)
            .resolve_symbol(
                crate::symbol_resolver::SymRecv::Value(Ty::obj("kotlin/UShort")),
                "rangeTo",
                &[Ty::obj("kotlin/UShort")],
                &[],
            )
            .and_then(crate::symbol_resolver::Symbol::call)
            .expect("UShort.rangeTo(UShort) must select from parsed stdlib metadata");
        assert_eq!(ushort_range.ret, Ty::obj("kotlin/ranges/UIntRange"));
    }

    #[test]
    fn stdlib_array_classifier_contains_its_iterator_declaration() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let symbols = libraries.symbols(SymbolNamespace::Package(type_name("kotlin")), "Array");
        let classifier = symbols.classifier.as_ref().expect("Array classifier");
        let iterator = classifier
            .declared_callables
            .get("iterator")
            .and_then(|callables| match callables {
                crate::libraries::Callables::Functions(functions)
                | crate::libraries::Callables::Both { functions, .. } => {
                    functions.overloads.first()
                }
                crate::libraries::Callables::None | crate::libraries::Callables::Properties(_) => {
                    None
                }
            })
            .expect("Array's builtins record must publish iterator");
        assert_eq!(iterator.kind, crate::libraries::FnKind::Member);
        assert!(iterator.callable.params.is_empty());
        assert!(iterator.callable.owner_matches("kotlin/Array"));
        assert!(
            iterator
                .callable
                .ret
                .obj_internal()
                .is_some_and(|name| name.matches("kotlin/collections/Iterator")),
            "decoded Array.iterator return: {:?}",
            iterator.callable.ret
        );
        assert!(
            classifier.constructors.iter().any(|constructor| {
                constructor.params.len() == 2
                    && constructor.params[0] == Ty::Int
                    && constructor.params[1].fun_arity() == Some(1)
            }),
            "Array's builtins record must publish its (size, init) constructor: {:?}",
            classifier.constructors
        );

        assert!(classifier.supertypes.contains("kotlin/Cloneable"));
        assert!(classifier.supertypes.contains("java/io/Serializable"));
        let applied = Ty::obj_args("kotlin/Array", &[Ty::String]);
        let clone = crate::symbol_resolver::declared_member_callables(&libraries, applied, "clone");
        let clone = match clone {
            crate::libraries::Callables::Functions(functions)
            | crate::libraries::Callables::Both { functions, .. } => functions
                .overloads
                .into_iter()
                .next()
                .expect("Array clone overload"),
            crate::libraries::Callables::None | crate::libraries::Callables::Properties(_) => {
                panic!("Array clone function")
            }
        };
        assert_eq!(clone.callable.ret, applied);
        assert_eq!(clone.callable.physical_ret, Ty::obj("kotlin/Any"));
        assert!(clone.callable.owner_matches("java/lang/Object"));
        assert_eq!(clone.visibility, crate::types::Visibility::Public);
    }

    #[test]
    fn stdlib_boolean_classifier_exposes_comparable_member() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let symbols = libraries.symbols(SymbolNamespace::Package(type_name("kotlin")), "Boolean");
        let classifier = symbols.classifier.as_ref().expect("Boolean classifier");
        assert!(
            classifier.supertypes.contains("kotlin/Comparable"),
            "Boolean supertypes: {:?}",
            classifier.supertypes
        );
        let selected = crate::symbol_resolver::SymbolResolver::new(&libraries)
            .resolve_symbol(
                crate::symbol_resolver::SymRecv::Value(Ty::Boolean),
                "compareTo",
                &[Ty::Boolean],
                &[],
            )
            .and_then(crate::symbol_resolver::Symbol::call)
            .expect("Boolean.compareTo(Boolean)");
        assert_eq!(selected.ret, Ty::Int);
    }

    #[test]
    fn jvm_customizer_synthesizes_cloneable_classifier() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let symbols = libraries.symbols(SymbolNamespace::Package(type_name("kotlin")), "Cloneable");
        let classifier = symbols.classifier.as_ref().expect("Cloneable classifier");
        assert!(classifier.is_interface());
        assert!(classifier.supertypes.contains("kotlin/Any"));
        let clone = classifier
            .members
            .iter()
            .find(|member| member.name == "clone" && member.params.is_empty())
            .expect("protected Cloneable.clone");
        assert_eq!(clone.visibility, crate::types::Visibility::Protected);
        assert_eq!(clone.ret, Ty::obj("kotlin/Any"));
        assert_eq!(clone.physical_ret, Ty::obj("kotlin/Any"));
        assert_eq!(clone.descriptor, "()Ljava/lang/Object;");
        assert_eq!(clone.owner, Some(crate::types::wk::java_object()));
    }

    #[test]
    fn stdlib_primitive_iterator_has_semantic_primitive_supertype_argument() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let metadata = libraries
            .cp
            .find("kotlin/collections/IntIterator")
            .expect("IntIterator classfile");
        assert!(
            metadata.meta.class_supertypes.iter().any(|supertype| {
                supertype.obj_internal().is_some_and(|name| {
                    name.matches("kotlin/collections/Iterator")
                        && supertype.type_args() == [Ty::Int]
                })
            }),
            "@Metadata must decode IntIterator : Iterator<Int>, got {:?}",
            metadata.meta.class_supertypes
        );
        let symbols = libraries.symbols(
            SymbolNamespace::Package(type_name("kotlin/collections")),
            "IntIterator",
        );
        let classifier = symbols.classifier.as_ref().expect("IntIterator classifier");
        assert!(
            classifier.supertype_templates.iter().any(|supertype| {
                supertype.obj_internal().is_some_and(|name| {
                    name.matches("kotlin/collections/Iterator")
                        && supertype.type_args() == [Ty::Int]
                })
            }),
            "IntIterator supertypes must contain Iterator<Int>, got {:?}",
            classifier.supertype_templates
        );
        assert!(
            classifier
                .supertype_templates
                .iter()
                .all(|supertype| !supertype
                    .obj_internal()
                    .is_some_and(|name| name.matches("java/util/Iterator"))),
            "the JVM realization must not enter the Kotlin supertype graph: {:?}",
            classifier.supertype_templates
        );
    }

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
    fn descriptor_void_and_java_void_are_distinct() {
        assert_eq!(desc_to_ty("Ljava/lang/Void;"), Ty::obj("java/lang/Void"));
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
    fn classpath_classifier_record_returns_only_the_exact_owner() {
        let sources = [
            (
                String::new(),
                "package sample; public class Base { public int greet() { return 1; } }".into(),
            ),
            (
                String::new(),
                "package sample; public class Child extends Base {}".into(),
            ),
        ];
        let stubs = crate::jvm::java_stub::stub_classes(
            &sources,
            crate::jvm::java_stub::StubMode::Strict,
            &|candidate| candidate == "java/lang/Object",
        )
        .expect("member stubs");
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
        classpath.set_stub_overlay(stubs);
        let libraries = super::JvmLibraries::new(classpath);

        assert!(crate::symbol_resolver::declared_member_callables(
            &libraries,
            Ty::obj("sample/Child"),
            "greet",
        )
        .into_parts()
        .0
        .overloads
        .is_empty());
        let declared = crate::symbol_resolver::declared_member_callables(
            &libraries,
            Ty::obj("sample/Base"),
            "greet",
        )
        .into_parts()
        .0;
        assert_eq!(declared.overloads.len(), 1);
        assert_eq!(declared.overloads[0].receiver_rank, 0);
        assert!(declared.overloads[0].callable.owner.matches("sample/Base"));
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
                .select_member_property(Ty::obj("sample/Base"), "value")
                .map(|property| property.ty),
            Some(Ty::Int)
        );
        for child in ["sample/PrivateChild", "sample/StaticChild"] {
            assert!(
                resolver
                    .select_member_property(Ty::obj(child), "value")
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
            &|candidate| {
                matches!(
                    candidate,
                    "java/lang/Object" | "java/lang/CharSequence" | "java/util/List"
                )
            },
        )
        .expect("generic field stubs");
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
        classpath.set_stub_overlay(stubs);
        let libraries = super::JvmLibraries::new(classpath);
        let shape = libraries
            .classifier_record(type_name("sample/Holder"))
            .expect("generic holder shape");
        assert_eq!(shape.type_params, ["T"]);
        assert!(
            matches!(shape.fields.get(1), Some(field) if matches!(field.ty.non_null(), Ty::Obj(_, arguments) if matches!(arguments, [Ty::TyParam("T", _)]))),
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
                .select_member_property(Ty::obj_args("sample/Holder", &[Ty::String]), "values")
                .map(|property| property.ty),
            Some(Ty::platform_nullable(Ty::obj_args(
                "kotlin/collections/List",
                &[Ty::String]
            ))),
            "the field type must be substituted through the receiver hierarchy"
        );
        let raw_values = resolver
            .select_member_property(Ty::obj("sample/Holder"), "values")
            .expect("raw public field")
            .ty;
        assert!(
            matches!(raw_values.non_null(), Ty::Obj(_, arguments) if arguments.is_empty()),
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
        assert_eq!(
            libs.canonical_source_type_name(type_name("java/lang/String")),
            type_name("kotlin/String")
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
            [Ty::platform_nullable(Ty::obj_args(
                "kotlin/collections/Collection",
                &[Ty::in_projection(Ty::ty_param(
                    "R",
                    Ty::platform_nullable(Ty::obj("kotlin/Any")),
                ))],
            ))]
        );
    }

    #[test]
    fn java_flexibility_recurses_through_formal_bounds_and_projections() {
        let (_, bounds, _) =
            parse_formals("<T:Ljava/util/List<Ljava/lang/String;>;>Ljava/lang/Object;");
        assert_eq!(
            bounds,
            [vec![Ty::platform_nullable(Ty::obj_args(
                "kotlin/collections/List",
                &[Ty::platform_nullable(Ty::String)],
            ))]],
        );

        assert_eq!(
            java_type_nullability(
                Ty::obj_args("java/util/List", &[Ty::in_projection(Ty::String)]),
                None,
            ),
            Ty::platform_nullable(Ty::obj_args(
                "java/util/List",
                &[Ty::in_projection(Ty::platform_nullable(Ty::String))],
            )),
        );

        let base = Ty::obj("fixtures/Base");
        assert_eq!(
            java_type_nullability(Ty::array(base), None),
            Ty::platform_nullable(Ty::obj_args(
                "kotlin/Array",
                &[Ty::out_projection(Ty::platform_nullable(base))],
            )),
            "the Java provider must publish reference-array covariance in the normalized type",
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
            [Ty::obj_args(
                "kotlin/collections/List",
                &[Ty::out_projection(Ty::String)]
            )]
        );
        assert_eq!(
            method.ret,
            Ty::obj_args(
                "kotlin/collections/List",
                &[Ty::out_projection(Ty::nullable(Ty::obj("kotlin/Any")))]
            )
        );

        let (_, _, supertypes) = parse_class_gsig(
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
        let (_, _, supertypes) =
            parse_class_gsig("Ljava/lang/Object;Ljava/lang/Iterable<Lkotlin/UInt;>;")
                .expect("class signature");
        assert_eq!(
            supertypes[1],
            Ty::obj_args("kotlin/collections/Iterable", &[Ty::UInt])
        );
    }

    #[test]
    fn class_generic_signature_retains_formal_bounds() {
        let (formals, bounds, _) =
            parse_class_gsig("<T:Ljava/lang/CharSequence;>Ljava/lang/Object;")
                .expect("class signature");
        assert_eq!(formals, ["T"]);
        assert_eq!(
            bounds,
            [vec![Ty::platform_nullable(Ty::obj("kotlin/CharSequence"))]]
        );
    }

    #[test]
    fn function_generic_signature_canonicalizes_bottom_and_unit_returns() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let unit = libraries.semanticize_jvm_generic_sig(
            parse_method_gsig("(Lkotlin/jvm/functions/Function0<Lkotlin/Unit;>;)V")
                .expect("Unit function signature"),
        );
        assert_eq!(unit.params, [Ty::fun(Vec::new(), Ty::Unit)]);

        let bottom = libraries.semanticize_jvm_generic_sig(
            parse_method_gsig("(Lkotlin/jvm/functions/Function0<Lkotlin/Nothing;>;)V")
                .expect("Nothing function signature"),
        );
        assert_eq!(bottom.params, [Ty::fun(Vec::new(), Ty::Nothing)]);
    }

    #[test]
    fn function_generic_signature_consumes_function_interface_variance() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let signature = libraries.semanticize_jvm_generic_sig(
            parse_method_gsig(
                "<T:Ljava/lang/Object;>(Lkotlin/jvm/functions/Function1<\
                 -Ljava/lang/String;+TT;>;)TT;",
            )
            .expect("function-typed generic signature"),
        );
        assert_eq!(
            signature.params,
            [Ty::fun(
                vec![Ty::String],
                Ty::ty_param("T", Ty::platform_nullable(Ty::obj("kotlin/Any"))),
            )]
        );
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
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let raw = parse_concrete_field_gsig(
            "Lkotlin/jvm/functions/Function1<\
             Ljava/util/List<Ljava/lang/Integer;>;\
             Ljava/util/Set<Ljava/lang/Long;>;>;",
            "Lkotlin/jvm/functions/Function1;",
        )
        .expect("concrete generic field signature");
        assert_eq!(
            libraries.semanticize_jvm_type(raw),
            Ty::fun(
                vec![Ty::obj_args("kotlin/collections/List", &[Ty::Int])],
                Ty::obj_args("kotlin/collections/Set", &[Ty::Long]),
            )
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
    fn field_function_shape_comes_from_the_classifier_declaration() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let valid = parse_concrete_field_gsig(
            "Lkotlin/jvm/functions/Function2<\
                 Ljava/lang/Integer;\
                 Ljava/lang/Long;\
                 Ljava/lang/String;>;",
            "Lkotlin/jvm/functions/Function2;",
        )
        .expect("valid generic field signature");
        assert_eq!(
            libraries.semanticize_jvm_type(valid),
            Ty::fun(vec![Ty::Int, Ty::Long], Ty::String)
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
            let raw = parse_concrete_field_gsig(signature, &descriptor)
                .expect("well-formed nominal generic signature");
            assert!(
                !matches!(libraries.semanticize_jvm_type(raw), Ty::Fun(_)),
                "invented a function shape for {signature}"
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

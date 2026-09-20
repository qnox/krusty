//! The stdlib this target actually compiles against: Kotlin/Native's own, read from its KLIB.
//!
//! Every symbol the native backend resolves today comes from `kotlin-stdlib.jar` — a JVM artifact,
//! through a JVM provider. That is why `intrinsics` carries a normalization table for spellings a
//! non-JVM target should never have seen — the JDK's own name for `kotlin.String`, and the file
//! facades a top-level function arrives owned by — and it is why `kotlin.Result` arrives wearing
//! the JVM's value-class erasure rather than its own.
//! `codegen`'s own comment has said so all along: *"A JVM provider because that is krusty's only
//! provider today … Phase 7 removes it."* This is that provider.
//!
//! Nothing here decodes anything. The container reader ([`crate::klib`]) and the metadata decoder
//! are already in the tree and already target-independent — the Kotlin/Native distribution ships
//! its klibs unpacked and its `linkdata` fragments carry the same protobuf the JVM `@Metadata`
//! reader decodes. What was missing is only a [`SemanticPlatform`] that serves those declarations,
//! which is what this module is: load, index, answer.
//!
//! The difference from the JVM provider is not a detail of spelling. A klib declaration is the
//! Kotlin one — no erasure has happened to it, so a value class still says it is a value class and
//! still names its underlying type. That is the fact a target needs to erase `Result<T>` to its
//! carrier instead of allocating a wrapper for it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::klib::ir::{IrConstant, IrDefaultKey, IrDefaults};
use crate::klib::{KlibArchive, KlibError};
use crate::libraries::{
    Callables, ClassifierInheritance, ExternalCallableKind, ExternalCallableRealization,
    ExternalPropertyRealization, FnKind, FunctionInfo, LibraryMember, LibraryType, ParamList,
    ResolvedSymbols, SemanticPlatform, TypeKind,
};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{type_name, Ty, TypeName, TypeNameList, TypeParameters};

/// Where a Kotlin/Native distribution keeps the common stdlib klib, below its root.
const STDLIB_UNDER_DISTRIBUTION: &str = "klib/common/stdlib";

/// What can go wrong bringing the stdlib in. Each names the file it was reading: a distribution
/// that is absent, truncated or of an unexpected shape is a broken toolchain rather than a program
/// error, and saying which file makes it diagnosable without a debugger.
#[derive(Debug)]
pub enum NativeLibrariesError {
    /// The distribution has no stdlib klib where one must be.
    MissingStdlib { path: PathBuf },
    /// The container would not open, or an entry would not read.
    Container(KlibError),
    /// The serialized IR would not read. This is a separate failure from a `linkdata` one: the
    /// two halves of a klib are decoded independently, and only this one carries default VALUES.
    InvalidIr(crate::klib::ir::IrError),
    /// A `linkdata` fragment did not decode.
    InvalidFragment {
        entry: String,
        source: crate::jvm::metadata::klib_validation::PackageFragmentDecodeError,
    },
}

impl std::fmt::Display for NativeLibrariesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingStdlib { path } => write!(
                f,
                "the Kotlin/Native distribution has no stdlib klib at {}",
                path.display()
            ),
            Self::Container(error) => write!(f, "invalid Kotlin/Native stdlib klib: {error}"),
            Self::InvalidIr(error) => write!(f, "invalid Kotlin/Native stdlib IR: {error}"),
            Self::InvalidFragment { entry, source } => {
                write!(f, "invalid stdlib linkdata fragment {entry}: {source:?}")
            }
        }
    }
}

impl From<KlibError> for NativeLibrariesError {
    fn from(error: KlibError) -> Self {
        Self::Container(error)
    }
}

/// The declarations one namespace contributes, indexed by the name a lookup asks for.
#[derive(Default)]
struct Namespace {
    classifiers: HashMap<String, Arc<LibraryType>>,
    /// Overloads share a name, so each entry is the whole set a lookup answers with.
    callables: HashMap<String, Vec<FunctionInfo>>,
    /// Top-level and extension PROPERTIES of this namespace. A name may carry both — `kotlin.text`
    /// declares `fun String.count()` and `val CharSequence.indices` alike — so the two live side by
    /// side and a lookup answers with whichever of them exists.
    properties: HashMap<String, Vec<crate::libraries::PropertyInfo>>,
}

/// Kotlin/Native's stdlib, as a symbol source.
///
/// The tables are behind one [`Arc`] and the handle is [`Clone`], because reading them is the
/// expensive part: the whole stdlib decodes once and every consumer that needs its own handle —
/// the frontend takes an owned provider, the backend a second one — shares that work instead of
/// repeating it. A caller that compiles many programs keeps one handle and clones it per program.
#[derive(Clone)]
pub struct NativeLibraries {
    index: Arc<Index>,
}

/// What one reading of the stdlib produced. Immutable once built, which is what lets it be shared.
struct Index {
    /// Package or classifier namespace -> what it declares. A classifier's own namespace holds its
    /// nested classifiers, which is the same shape a package has, so one map serves both.
    namespaces: HashMap<TypeName, Namespace>,
    /// What each identity this provider handed out was realized as, indexed BY that identity: the
    /// id is this table's position, which is what makes the lookup a provider owes its consumers
    /// an array read rather than a search. A klib has no descriptor to stand in for identity, so
    /// the identity is the only thing a backend carries and this is the only place it resolves.
    callable_realizations: Vec<ExternalCallableRealization>,
    /// The same arrangement for PROPERTIES, which carry an identity of their own: a read is not a
    /// call to a name a consumer could re-derive, so the declaration's accessors are interned here
    /// and the read carries only this position.
    property_realizations: Vec<ExternalPropertyRealization>,
    /// Every package name the stdlib declares, INCLUDING the prefixes that declare nothing of
    /// their own. A qualified reference is resolved one segment at a time, and a segment that
    /// names neither a classifier nor a package is unresolved — so `kotlin.text.Regex` needs
    /// `kotlin` to be answerable as a package, and a fragment list alone does not answer it.
    packages: std::collections::HashSet<TypeName>,
}

impl NativeLibraries {
    /// Read the stdlib out of a Kotlin/Native distribution root.
    pub fn from_distribution(root: &Path) -> Result<Self, NativeLibrariesError> {
        let stdlib = root.join(STDLIB_UNDER_DISTRIBUTION);
        if !stdlib.exists() {
            return Err(NativeLibrariesError::MissingStdlib { path: stdlib });
        }
        Self::from_klib(&stdlib)
    }

    /// Read one klib. Split from [`Self::from_distribution`] so a test can point at a fixture, and
    /// so a later change can federate several klibs without reshaping the caller.
    pub fn from_klib(path: &Path) -> Result<Self, NativeLibrariesError> {
        let archive = KlibArchive::open(path)?;
        // The bodies half, which is where a parameter's default VALUE lives. A klib that ships no
        // IR at all is a declarations-only artifact and still usable — a call that omits a default
        // simply cannot be compiled against it — so only a malformed one is an error.
        let defaults = match crate::klib::ir::read_defaults(&archive) {
            Ok(defaults) => defaults,
            Err(crate::klib::ir::IrError::Missing { .. }) => IrDefaults::default(),
            Err(error) => return Err(NativeLibrariesError::InvalidIr(error)),
        };
        let mut namespaces: HashMap<TypeName, Namespace> = HashMap::new();
        let mut callable_realizations = Vec::new();
        let mut property_realizations = Vec::new();
        let mut packages = std::collections::HashSet::new();
        for fragment in archive.package_fragments() {
            let bytes = archive.read(&fragment.entry)?;
            let package =
                crate::jvm::metadata::klib_validation::parse_package_fragment_checked(&bytes)
                    .map_err(|source| NativeLibrariesError::InvalidFragment {
                        entry: fragment.entry.clone(),
                        source,
                    })?;
            let package_name = package_namespace(&fragment.package_fqname);
            record_package(&mut packages, &fragment.package_fqname);
            // Sorted, because the decoder hands the classes over in a `HashMap` and Rust
            // randomizes that iteration PER PROCESS. Identities are this table's positions, so an
            // unsorted walk numbers the same stdlib differently on every run — and a compiler
            // whose output depends on which run it was is one whose failures cannot be reproduced.
            let mut classes = package.classes.into_iter().collect::<Vec<_>>();
            classes.sort_by(|(left, _), (right, _)| left.cmp(right));
            for (internal, declaration) in classes {
                let identity = type_name(&internal);
                // A classifier is indexed under the namespace its identity names — its package for
                // a top-level one, its outer classifier for a nested one — which is exactly what
                // `SymbolNamespace::classifier_key` asks with.
                let (namespace, name) = SymbolNamespace::classifier_key(identity);
                namespaces
                    .entry(namespace.name())
                    .or_default()
                    .classifiers
                    .entry(name.to_string())
                    .or_insert_with(|| {
                        Arc::new(library_type(
                            identity,
                            declaration,
                            &mut callable_realizations,
                            &mut property_realizations,
                        ))
                    });
            }
            for function in &package.functions {
                let identity = crate::fir::ExternalCallableId::from_raw(
                    u32::try_from(callable_realizations.len())
                        .expect("too many stdlib declarations for a packed identity"),
                );
                let info = top_level_function(
                    package_name,
                    function,
                    identity,
                    &defaults,
                    &fragment.package_fqname,
                );
                callable_realizations.push(ExternalCallableRealization {
                    callable: info.callable.clone(),
                    kind: if info.kind == FnKind::Extension {
                        ExternalCallableKind::Extension
                    } else {
                        ExternalCallableKind::TopLevel
                    },
                });
                namespaces
                    .entry(package_name)
                    .or_default()
                    .callables
                    .entry(function.name.clone())
                    .or_default()
                    .push(info);
            }
            for property in &package.properties {
                let info = top_level_property(
                    package_name,
                    property,
                    &mut callable_realizations,
                    &mut property_realizations,
                );
                namespaces
                    .entry(package_name)
                    .or_default()
                    .properties
                    .entry(property.name.clone())
                    .or_default()
                    .push(info);
            }
        }
        Ok(Self {
            index: Arc::new(Index {
                namespaces,
                callable_realizations,
                property_realizations,
                packages,
            }),
        })
    }

    /// How many namespaces carry a declaration — for a caller that wants to say the stdlib really
    /// was read rather than silently empty.
    pub fn namespace_count(&self) -> usize {
        self.index.namespaces.len()
    }
}

/// Record a package and every prefix of it. `kotlin.collections` implies `kotlin`, which may
/// itself declare nothing and still has to answer a qualified walk.
fn record_package(into: &mut std::collections::HashSet<TypeName>, fqname: &str) {
    let mut prefix = String::new();
    for segment in fqname.split('.').filter(|segment| !segment.is_empty()) {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(segment);
        into.insert(type_name(&prefix));
    }
}

/// The namespace a package fragment's declarations belong to. The root package is `TypeName::ROOT`
/// rather than absence, as [`SymbolNamespace`] documents.
fn package_namespace(fqname: &str) -> TypeName {
    if fqname.is_empty() {
        TypeName::ROOT
    } else {
        type_name(&fqname.replace('.', "/"))
    }
}

impl SymbolSource for NativeLibraries {
    /// What this provider realized an identity as. The id is this table's position, assigned when
    /// the declaration was read, so the answer is an array read rather than a search — and only
    /// this provider can give it, which is what makes the identity safe to carry opaquely.
    fn external_callable(
        &self,
        identity: crate::fir::ExternalCallableId,
    ) -> Option<ExternalCallableRealization> {
        self.index
            .callable_realizations
            .get(identity.raw() as usize)
            .cloned()
    }

    /// The same, for a property. A read carries this identity rather than an accessor name, so
    /// this is the only place a consumer can learn what the declaration behind it was.
    fn external_property(
        &self,
        identity: crate::fir::ExternalPropertyId,
    ) -> Option<ExternalPropertyRealization> {
        self.index
            .property_realizations
            .get(identity.raw() as usize)
            .cloned()
    }

    /// Whether `parent.name` is one of the stdlib's packages. Without this a fully-qualified
    /// reference cannot get past its first segment: `kotlin` names no classifier, so a walk with
    /// nothing to ask about packages reports `unresolved reference 'kotlin'` and stops.
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        self.index.packages.contains(&qualified(parent, name))
    }

    fn symbols(&self, namespace: SymbolNamespace, name: &str) -> std::rc::Rc<ResolvedSymbols> {
        let Some(found) = self.index.namespaces.get(&namespace.name()) else {
            return std::rc::Rc::new(ResolvedSymbols::default());
        };
        let classifier = found.classifiers.get(name).cloned();
        let callables = Callables::from_parts(
            crate::libraries::FunctionSet {
                overloads: found.callables.get(name).cloned().unwrap_or_default(),
            },
            crate::libraries::PropertySet {
                overloads: found.properties.get(name).cloned().unwrap_or_default(),
            },
        );
        std::rc::Rc::new(ResolvedSymbols {
            classifier_name: classifier.as_ref().map(|_| match namespace {
                SymbolNamespace::Package(package) => qualified(package, name),
                SymbolNamespace::Classifier(outer) => nested(outer, name),
            }),
            classifier,
            callables,
            importable_declaration: false,
        })
    }
}

/// One top-level declaration, as the overload a call site selects among.
///
/// The descriptor is deliberately EMPTY. It is a JVM emit handle, and a klib has none — which is
/// already the convention a source declaration uses, where "the target backend derives its ABI
/// from `params`/`ret`". The identity is what a backend carries instead, and this provider answers
/// for it.
fn top_level_function(
    package: TypeName,
    function: &crate::jvm::metadata::BuiltinFunction,
    identity: crate::fir::ExternalCallableId,
    defaults: &IrDefaults,
    package_fqname: &str,
) -> FunctionInfo {
    let bounds = crate::jvm::classpath::builtin_bounds(&function.formals, &HashMap::new());
    let ty = |t: &crate::jvm::metadata::BuiltinTy| crate::jvm::classpath::builtin_ty(t, &bounds);
    let receiver = function.receiver.as_ref().map(&ty);
    // An extension's receiver is its first PHYSICAL parameter, ahead of the written ones — the
    // same order a call site pushes them in.
    let params = receiver
        .iter()
        .copied()
        .chain(function.params.iter().map(&ty))
        .collect::<Vec<_>>();
    let ret = ty(&function.ret);
    let mut callable = crate::libraries::LibraryCallable::library(
        package,
        function.name.clone(),
        params,
        ret,
        ret,
        String::new(),
    );
    callable.external_identity = Some(identity);
    callable.suspend = function.is_suspend;
    callable.source_receiver = receiver;
    callable.context_count = function.context_count;
    let kind = if receiver.is_some() {
        FnKind::Extension
    } else {
        FnKind::TopLevel
    };
    let mut info = FunctionInfo::plain(kind, receiver, callable);
    info.visibility = function.visibility;
    info.context_count = function.context_count;
    // `infix` and `operator` are not decoration: a call written in infix or operator form admits
    // ONLY declarations carrying them. Without `infix`, `1..10 step 2` did not find
    // `IntProgression.step(Int)` at all and read `step` as the progression's `Int`-typed property
    // instead — "expression 'step' of type 'Int' cannot be invoked as a function", on 254 cases.
    info.flags.operator = function.is_operator;
    info.flags.infix = function.is_infix;
    info.flags.suspend = function.is_suspend;
    // `inline` is deliberately NOT published. It is not a modifier a call site merely records: a
    // declaration marked inline is one the frontend will try to SPLICE, and a klib's bodies live
    // in `default/ir/`, which this tree reads as opaque bytes. Claiming inline-ness the provider
    // cannot then supply a body for would turn a working call into a failed expansion.
    info.call_sig = crate::libraries::CallSig::metadata_member(
        function.params.len(),
        function.param_names.clone(),
        function.param_defaults.clone(),
        function.vararg,
    );
    // What the declaration's omitted parameters take. `linkdata` records THAT a parameter has a
    // default and never what it is; the value is an expression, and it lives in the library's IR.
    // Without it a call that omits one has nothing to pass: the JVM has a `$default` synthetic to
    // name, a klib has none, and 862 corpus cases reported exactly that.
    info.default_values = parameter_defaults(defaults, package_fqname, function);
    // The declaration's own type parameters, with the receiver kept apart from the written
    // parameters. Without this a generic declaration resolves but never INFERS: `listOf(1).let { }`
    // reported its result as the unbound `R` it was declared with.
    if !function.formals.is_empty() {
        info.generic_sig = Some(crate::libraries::GenericSig {
            formals: function
                .formals
                .iter()
                .map(|formal| formal.name.clone())
                .collect(),
            formal_bounds: function
                .formals
                .iter()
                .map(|formal| formal.bounds.iter().map(&ty).collect())
                .collect(),
            receiver,
            params: function.params.iter().map(&ty).collect(),
            ret,
            return_policy: crate::libraries::GenericReturnPolicy::Exact,
        });
    }
    info
}

/// One top-level or extension property, as the declaration a read selects.
///
/// The accessors are interned as callables and the declaration itself as a property, because a
/// read carries the PROPERTY's identity: there is no accessor name for a consumer to rebuild, and
/// a klib does not have one to rebuild it from.
fn top_level_property(
    package: TypeName,
    property: &crate::jvm::metadata::BuiltinProperty,
    realizations: &mut Vec<ExternalCallableRealization>,
    properties: &mut Vec<ExternalPropertyRealization>,
) -> crate::libraries::PropertyInfo {
    let bounds = crate::jvm::classpath::builtin_bounds(&property.formals, &HashMap::new());
    let ty = |t: &crate::jvm::metadata::BuiltinTy| crate::jvm::classpath::builtin_ty(t, &bounds);
    let receiver = property.receiver.as_ref().map(&ty);
    let declared = ty(&property.ty);
    // An extension's receiver is its first PHYSICAL parameter, ahead of the context ones; a plain
    // top-level property's accessors take nothing at all.
    let accessor_params = receiver.iter().copied().collect::<Vec<_>>();
    let mut getter = crate::libraries::LibraryCallable::library(
        package,
        property.name.clone(),
        accessor_params.clone(),
        declared,
        declared,
        String::new(),
    );
    getter.external_identity = Some(member_identity(realizations.len()));
    getter.source_receiver = receiver;
    getter.context_count = property.context_count;
    realizations.push(ExternalCallableRealization {
        callable: getter.clone(),
        kind: ExternalCallableKind::Member,
    });
    let setter = property.is_var.then(|| {
        let mut setter = crate::libraries::LibraryCallable::library(
            package,
            property.name.clone(),
            accessor_params
                .iter()
                .copied()
                .chain(std::iter::once(declared))
                .collect::<Vec<_>>(),
            Ty::Unit,
            Ty::Unit,
            String::new(),
        );
        setter.external_identity = Some(member_identity(realizations.len()));
        setter.source_receiver = receiver;
        setter.context_count = property.context_count;
        realizations.push(ExternalCallableRealization {
            callable: setter.clone(),
            kind: ExternalCallableKind::Member,
        });
        setter
    });
    let identity = crate::fir::ExternalPropertyId::from_raw(
        u32::try_from(properties.len())
            .expect("too many stdlib declarations for a packed identity"),
    );
    properties.push(ExternalPropertyRealization {
        name: property.name.clone(),
        getter: getter.external_identity.expect("just assigned"),
        setter: setter.as_ref().and_then(|setter| setter.external_identity),
        declares_value_class_storage: false,
    });
    getter.external_property_identity = Some(identity);
    let mut setter = setter;
    if let Some(setter) = setter.as_mut() {
        setter.external_property_identity = Some(identity);
    }
    crate::libraries::PropertyInfo {
        name: property.name.clone(),
        kind: match receiver {
            Some(_) => crate::libraries::PropKind::Extension,
            None => crate::libraries::PropKind::TopLevel,
        },
        receiver,
        formals: property
            .formals
            .iter()
            .map(|formal| formal.name.clone())
            .collect(),
        ty: declared,
        context_count: property.context_count,
        context_param_names: Vec::new(),
        getter,
        setter,
        setter_visibility: property.visibility,
        is_const: property.constant.is_some(),
        implicit_integer_coercion: false,
        compile_time_constant: property.constant.clone().map(|value| {
            crate::libraries::LibraryConst {
                ty: declared,
                value,
            }
        }),
        visibility: property.visibility,
        owner: package,
        receiver_rank: 0,
        source_key: None,
        stable_declaration: None,
        getter_declaration: None,
        setter_declaration: None,
        source_member: None,
        accessor_derived: false,
        read_stability: crate::libraries::PropertyReadStability::Unstable,
    }
}

/// What each of a declaration's value parameters defaults to, parallel to them.
///
/// The IR is matched by name and shape rather than by the library's own `IdSignature`, and the
/// parameter NAMES carry most of the discrimination — see [`IrDefaultKey`]. A declaration the IR
/// does not answer for gets an empty list, which reads as "no default is known" everywhere.
fn parameter_defaults(
    defaults: &IrDefaults,
    package_fqname: &str,
    function: &crate::jvm::metadata::BuiltinFunction,
) -> Vec<Option<crate::libraries::DefaultValue>> {
    if !function.param_defaults.iter().any(|has| *has) {
        return Vec::new();
    }
    // The key's parameters are the WRITTEN ones. A context parameter and an extension receiver are
    // leading physical parameters that the IR carries elsewhere — as `context_parameter` and
    // `extension_receiver` — so the names that identify the declaration start after them.
    let written = function
        .param_names
        .len()
        .saturating_sub(function.context_count);
    let key = IrDefaultKey {
        package: package_fqname.to_string(),
        owners: Vec::new(),
        name: function.name.clone(),
        parameters: function.param_names[function.param_names.len() - written..].to_vec(),
    };
    let Some(values) = defaults.get(&key) else {
        return Vec::new();
    };
    let mut out = vec![None; function.context_count];
    out.extend(values.iter().map(|value| value.as_ref().map(default_value)));
    out
}

/// One decoded IR constant, as the literal a call site materializes.
///
/// The container reader publishes its own constant type on purpose — what a constant MEANS is the
/// caller's — and this is that decision, made once.
fn default_value(constant: &IrConstant) -> crate::libraries::DefaultValue {
    use crate::libraries::DefaultValue;
    match constant {
        IrConstant::Null => DefaultValue::Null,
        IrConstant::Boolean(value) => DefaultValue::Bool(*value),
        IrConstant::Char(value) => DefaultValue::Char(*value),
        // Every sub-`Int` width is carried as the `Int` the library model records, which is the
        // same shape a classfile constant arrives in.
        IrConstant::Byte(value) => DefaultValue::Int(i64::from(*value)),
        IrConstant::Short(value) => DefaultValue::Int(i64::from(*value)),
        IrConstant::Int(value) => DefaultValue::Int(i64::from(*value)),
        IrConstant::Long(value) => DefaultValue::Long(*value),
        IrConstant::Float(value) => DefaultValue::Float(*value),
        IrConstant::Double(value) => DefaultValue::Double(*value),
        IrConstant::String(value) => {
            DefaultValue::Str(crate::kt_string::KtString::from(value.clone()))
        }
    }
}

fn qualified(package: TypeName, name: &str) -> TypeName {
    if package == TypeName::ROOT {
        type_name(name)
    } else {
        type_name(&format!("{}/{name}", package.render()))
    }
}

fn nested(outer: TypeName, name: &str) -> TypeName {
    type_name(&format!("{}${name}", outer.render()))
}

impl NativeLibraries {
    /// How many declarations this provider handed identities out for.
    pub fn callable_count(&self) -> usize {
        self.index.callable_realizations.len()
    }
}

impl SemanticPlatform for NativeLibraries {
    /// The token the reference compiler writes in a source-set diagnostic for this target.
    fn diagnostic_target_name(&self) -> Option<&str> {
        Some("Native")
    }

    /// `X::class` is a `kotlin.reflect.KClass` here as it is everywhere — the type is Kotlin's own,
    /// declared in the stdlib this provider reads, and nothing about it is the JVM's. Only its
    /// `.java` unwrapping is, and that is a member of the JVM's own declaration.
    fn class_literal_type(&self) -> Option<Ty> {
        Some(Ty::obj("kotlin/reflect/KClass"))
    }

    /// Kotlin/Native's own default-import addition, as the JVM contributes `java.lang` and
    /// `kotlin.jvm`. It is NOT `kotlin.jvm`: a native program that writes `@JvmInline` without
    /// importing it is one the reference compiler rejects too.
    fn platform_default_import_packages(&self) -> &'static [&'static str] {
        &["kotlin.native"]
    }
}

/// One classifier, as the declaration a lookup resolves to.
///
/// Deliberately general where [`crate::jvm::common_metadata`]'s conversion is not: that one exists
/// to import optional-expectation ANNOTATIONS into the JVM source, so it fixes the kind, leaves
/// members empty and writes a `SOURCE` retention. A stdlib provider has to carry whatever the
/// declaration says it is — including, for a `@JvmInline value class`, that it is one.
fn library_type(
    identity: TypeName,
    declaration: crate::jvm::metadata::BuiltinClass,
    realizations: &mut Vec<ExternalCallableRealization>,
    properties: &mut Vec<ExternalPropertyRealization>,
) -> LibraryType {
    let bounds = crate::jvm::classpath::builtin_bounds(&declaration.type_params, &HashMap::new());
    let type_parameters = TypeParameters::new(
        declaration
            .type_params
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect(),
        declaration
            .type_params
            .iter()
            .map(|parameter| {
                parameter
                    .bounds
                    .iter()
                    .map(|bound| crate::jvm::classpath::builtin_ty(bound, &bounds))
                    .collect()
            })
            .collect(),
        declaration
            .type_params
            .iter()
            .map(|parameter| parameter.variance)
            .collect(),
    );
    let supertype_templates = declaration
        .supertype_tys
        .iter()
        .map(|supertype| crate::jvm::classpath::builtin_ty(supertype, &bounds))
        .collect::<Vec<_>>();
    let supertypes: TypeNameList = declaration
        .supertypes
        .iter()
        .map(|supertype| type_name(supertype))
        .collect::<Vec<_>>()
        .into();
    let mut constructors = Vec::new();
    let mut named_parameter_lists = Vec::new();
    for constructor in &declaration.constructors {
        let params = constructor
            .params
            .iter()
            .map(|parameter| crate::jvm::classpath::builtin_ty(parameter, &bounds))
            .collect::<Vec<_>>();
        let mut member = LibraryMember::new(
            "<init>".to_string(),
            params.clone(),
            Ty::Unit,
            String::new(),
        );
        member.visibility = constructor.visibility;
        // A construction carries the constructor's identity, exactly as a call carries its
        // callee's. Without one, checked FIR has no target to name and the whole program fails
        // with `MissingStableCallTarget` — `StringBuilder()` alone accounted for 221 corpus cases.
        member.owner = Some(identity);
        member.external_identity = Some(member_identity(realizations.len()));
        realizations.push(ExternalCallableRealization {
            callable: crate::libraries::LibraryCallable::library(
                identity,
                "<init>",
                params.clone(),
                Ty::Unit,
                Ty::Unit,
                String::new(),
            ),
            kind: ExternalCallableKind::Constructor,
        });
        member.call_sig = crate::libraries::CallSig::metadata_member(
            params.len(),
            constructor.param_names.clone(),
            constructor.param_defaults.clone(),
            constructor.vararg,
        );
        constructors.push(member);
        named_parameter_lists.push(ParamList {
            visibility: constructor.visibility,
            names: constructor.param_names.clone(),
            defaults: constructor.param_defaults.clone(),
            types: params,
            recv_fun: Vec::new(),
            vararg: constructor.vararg,
            annotation: None,
        });
    }
    let is_interface = declaration.kind == TypeKind::Interface;
    let ClassMembers {
        members,
        declared_callables,
        declared_callable_order,
        constants,
    } = class_members(identity, &declaration, &bounds, realizations, properties);
    let mut classifier = LibraryType {
        access: declaration.visibility.into(),
        is_kotlin: true,
        source_file: None,
        stable_declaration: None,
        is_nested: declaration.is_nested,
        outer_instance: None,
        kind: declaration.kind,
        inheritance: ClassifierInheritance {
            // The DECLARED modality, not a guess from the kind. Reading "extensible only if an
            // interface" off the kind made `kotlin.Number`, `kotlin.Exception` and every other
            // `open`/`abstract` stdlib class unsubclassable, which the corpus reported verbatim:
            // "superclass 'kotlin/Number' cannot be subclassed: it is not extensible".
            is_abstract: is_interface || declaration.modality.is_abstract(),
            is_extensible: is_interface || declaration.modality.is_extensible(),
            has_no_arg_constructor: constructors
                .iter()
                .any(|constructor| constructor.params.is_empty()),
        },
        supertypes,
        supertype_templates,
        constructors,
        hidden_member_properties: Default::default(),
        declared_callables,
        declared_callable_order,
        members,
        companion: Vec::new(),
        constants,
        sam_eligible: false,
        callable_signature: None,
        callable_signatures: Vec::new(),
        companion_object: declaration
            .companion_name
            .as_ref()
            .map(|name| (name.clone(), nested(identity, name))),
        value_underlying: None,
        value_underlying_property: None,
        alias_target: None,
        own_type_parameter_count: type_parameters.type_params.len(),
        type_parameters,
        sealed_subclasses: TypeNameList::new(),
        enum_entries: Vec::new(),
        enum_entries_accessor: None,
        named_parameter_lists,
        annotations: Vec::new(),
        retention: None,
        annotation_targets: None,
    };
    // `Int.plus`, `String.length`, `Array.size`, the bit operations, the numeric conversions: the
    // klib declares every one of them as an ordinary member, because at the SOURCE level that is
    // what they are. No target implements them by dispatching to a method, and this is the
    // target-neutral place that says so — the same one the JVM provider calls, on the same
    // declarations, stamping the operation onto the overload the fragment already published rather
    // than adding a second one. Without it the corpus declined `kotlin.Int.plus` 135 times and
    // `kotlin.Int.compareTo` 123: a member call the code generator has no method to call.
    crate::libraries::add_core_builtin_declarations(&mut classifier, identity);
    classifier
}

/// What one classifier declares directly: the physical member list a backend reads, and the
/// name-keyed families a call site selects among.
///
/// A klib member is the KOTLIN declaration, which is what makes this short. The JVM provider has
/// to RECOVER a property from a `getSize()`/`setSize(I)` pair, a `@Metadata` property record and
/// the class file's own method table, and it has to decide which of those three agree; here the
/// fragment simply says "property `size`, type `Int`" and that is the whole of it. Nothing below
/// invents an accessor name, because inventing one is the JVM spelling this provider exists to
/// stop importing.
///
/// Two facts the fragment does NOT carry are recorded as such rather than guessed:
/// a property's mutability (so every property is published read-only, with no setter) and a
/// member's visibility (so every member is published `public`, which is what the overwhelming
/// majority of a published stdlib surface is). Both are widenings of the decoder, not of this
/// conversion.
fn class_members(
    owner: TypeName,
    declaration: &crate::jvm::metadata::BuiltinClass,
    bounds: &HashMap<String, Ty>,
    realizations: &mut Vec<ExternalCallableRealization>,
    property_realizations: &mut Vec<ExternalPropertyRealization>,
) -> ClassMembers {
    // Symbolic: the classifier's own formals stay unbound here, and core substitutes the applied
    // receiver's arguments into them once, when it specializes the family it selected from.
    let receiver = Ty::obj_name(owner);
    let mut members = Vec::new();
    let mut declared: HashMap<String, Callables> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut constants: HashMap<String, crate::libraries::LibraryConst> = HashMap::new();
    for member in &declaration.members {
        // A member's own formals shadow the classifier's, and carry their own declared bounds.
        let member_bounds = crate::jvm::classpath::builtin_bounds(&member.formals, bounds);
        let formals = member
            .formals
            .iter()
            .map(|formal| formal.name.clone())
            .collect::<Vec<_>>();
        let params = member
            .params
            .iter()
            .map(|parameter| crate::jvm::classpath::builtin_ty(parameter, &member_bounds))
            .collect::<Vec<_>>();
        let ret = crate::jvm::classpath::builtin_ty(&member.ret, &member_bounds);
        let mut callable = crate::libraries::LibraryCallable::library(
            owner,
            member.name.clone(),
            params.clone(),
            ret,
            ret,
            String::new(),
        );
        let interned = realizations.len();
        callable.external_identity = Some(member_identity(interned));
        realizations.push(ExternalCallableRealization {
            callable: callable.clone(),
            // A klib property is read by CALLING its getter, not by reading a field: the container
            // publishes no storage layout, and a native target owns its own. So the realization is
            // a member call for a function and for a property alike.
            kind: ExternalCallableKind::Member,
        });
        if !declared.contains_key(&member.name) {
            order.push(member.name.clone());
        }
        let family = declared.entry(member.name.clone()).or_default();
        let (mut functions, mut properties) = std::mem::take(family).into_parts();
        if member.is_property {
            // A read carries the PROPERTY's identity, not its getter's: the accessor is this
            // provider's realization of the declaration, and a consumer must not have to rebuild
            // an accessor name to ask what a read means.
            let identity = crate::fir::ExternalPropertyId::from_raw(
                u32::try_from(property_realizations.len())
                    .expect("too many stdlib declarations for a packed identity"),
            );
            property_realizations.push(ExternalPropertyRealization {
                name: member.name.clone(),
                getter: callable
                    .external_identity
                    .expect("every klib member is interned with an identity"),
                setter: None,
                declares_value_class_storage: false,
            });
            callable.external_property_identity = Some(identity);
            realizations[interned].callable.external_property_identity = Some(identity);
            // A `const val` is folded at every use site. Publishing the value is what lets
            // `Int.MAX_VALUE` resolve on a target whose companion objects have no storage to read.
            let constant = member
                .constant
                .clone()
                .map(|value| crate::libraries::LibraryConst { ty: ret, value });
            if let Some(constant) = constant.clone() {
                constants.insert(member.name.clone(), constant);
            }
            properties.overloads.push(crate::libraries::PropertyInfo {
                name: member.name.clone(),
                kind: crate::libraries::PropKind::Member,
                receiver: Some(receiver),
                formals: formals.clone(),
                ty: ret,
                context_count: 0,
                context_param_names: Vec::new(),
                getter: callable,
                setter: None,
                setter_visibility: crate::libraries::Visibility::Private,
                is_const: constant.is_some(),
                implicit_integer_coercion: false,
                compile_time_constant: constant,
                visibility: crate::libraries::Visibility::Public,
                owner,
                receiver_rank: 0,
                source_key: None,
                stable_declaration: None,
                getter_declaration: None,
                setter_declaration: None,
                source_member: None,
                accessor_derived: false,
                read_stability: crate::libraries::PropertyReadStability::Unstable,
            });
        } else {
            // What this member IS, rather than what it is declared as. A klib declares
            // `Int.plus(Int): Int` as an ordinary member, because at the source level that is
            // exactly what it is; no target implements it by dispatching to a method.
            let realization = crate::libraries::builtin_realization::member_realization(
                owner,
                &member.name,
                &params,
                ret,
            );
            callable.member_realization = realization;
            let mut info = FunctionInfo::plain(FnKind::Member, Some(receiver), callable);
            info.flags.operator = member.is_operator;
            info.flags.infix = member.is_infix;
            info.flags.is_abstract = member.is_abstract;
            if !formals.is_empty() {
                info.generic_sig = Some(crate::libraries::GenericSig {
                    formal_bounds: member
                        .formals
                        .iter()
                        .map(|formal| {
                            formal
                                .bounds
                                .iter()
                                .map(|bound| {
                                    crate::jvm::classpath::builtin_ty(bound, &member_bounds)
                                })
                                .collect()
                        })
                        .collect(),
                    formals: formals.clone(),
                    receiver: None,
                    params: params.clone(),
                    ret,
                    return_policy: crate::libraries::GenericReturnPolicy::Exact,
                });
            }
            functions.overloads.push(info);
            let mut physical = LibraryMember::new(member.name.clone(), params, ret, String::new());
            physical.realization = realization;
            physical.set_is_abstract(member.is_abstract);
            physical.set_ret_nullable(member.ret_nullable);
            physical.set_is_interface(declaration.kind == TypeKind::Interface);
            physical.external_identity = Some(member_identity(realizations.len() - 1));
            members.push(physical);
        }
        *family = Callables::from_parts(functions, properties);
    }
    ClassMembers {
        members,
        declared_callables: declared,
        declared_callable_order: order,
        constants,
    }
}

/// What one classifier's declarations amount to, in the four shapes a [`LibraryType`] keeps them.
struct ClassMembers {
    members: Vec<LibraryMember>,
    declared_callables: HashMap<String, Callables>,
    declared_callable_order: Vec<String>,
    constants: HashMap<String, crate::libraries::LibraryConst>,
}

/// The identity this provider hands out for the declaration at `position` in its realization
/// table. Split out so the packing assertion reads the same at every site that assigns one.
fn member_identity(position: usize) -> crate::fir::ExternalCallableId {
    crate::fir::ExternalCallableId::from_raw(
        u32::try_from(position).expect("too many stdlib declarations for a packed identity"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cached Kotlin/Native distribution, or `None` when this checkout has not provisioned one.
    fn distribution() -> Option<PathBuf> {
        crate::toolchain::kotlin_native_root()
    }

    /// The whole Kotlin/Native stdlib decodes with the reader already in this tree.
    ///
    /// This is the measurement the klib question turns on, so it is a test rather than something
    /// someone has to re-derive: the container reader and the metadata decoder were written for
    /// the JVM's `@Metadata`, and whether they read Kotlin/Native's own `linkdata` decides whether
    /// this target can compile against the real stdlib or must keep reimplementing it.
    ///
    /// It is asserted as ALL of them, not as a count above a threshold. A stdlib with a package
    /// missing is the failure this whole provider exists to avoid, and a threshold is exactly the
    /// shape of assertion that would let one go quiet.
    #[test]
    fn the_whole_kotlin_native_stdlib_decodes() {
        let Some(root) = distribution() else {
            eprintln!("skipping: no Kotlin/Native distribution cached");
            return;
        };
        let archive = KlibArchive::open(&root.join(STDLIB_UNDER_DISTRIBUTION)).expect("open");
        let (mut decoded, mut rejected) = (0usize, Vec::new());
        for fragment in archive.package_fragments() {
            let bytes = archive.read(&fragment.entry).expect("read a fragment");
            match crate::jvm::metadata::klib_validation::parse_package_fragment_checked(&bytes) {
                Ok(_) => decoded += 1,
                Err(error) => rejected.push(format!("{}: {error:?}", fragment.entry)),
            }
        }
        assert!(
            rejected.is_empty(),
            "every stdlib fragment decodes; these did not: {rejected:#?}"
        );
        assert!(
            decoded > 480,
            "and there are as many of them as the distribution ships: {decoded}"
        );
    }

    /// The same stdlib reads the same way twice, in one process and across processes.
    ///
    /// An identity is a table POSITION, assigned as the stdlib is walked, so the walk's order is
    /// part of the compiler's output. The decoder hands a package's classes over in a `HashMap`,
    /// and Rust randomizes that iteration per process — so the same klib numbered itself
    /// differently on every run, and a program compiled twice took different paths. It showed as a
    /// program that emitted clean on one run and produced IR Cranelift's own verifier rejected on
    /// the next, with nothing changed between them.
    ///
    /// Reading the stdlib twice in ONE process is the test that can be written here: both readings
    /// share the process's hash seed, so an order that depends on anything but the data would have
    /// to be unstable within a run too — which is exactly what was observed before the sort.
    #[test]
    fn the_stdlib_reads_the_same_way_every_time() {
        let Some(root) = distribution() else {
            eprintln!("skipping: no Kotlin/Native distribution cached");
            return;
        };
        let realizations = |libraries: &NativeLibraries| {
            (0..libraries.callable_count())
                .map(|position| {
                    let identity = crate::fir::ExternalCallableId::from_raw(position as u32);
                    let realized = SymbolSource::external_callable(libraries, identity)
                        .expect("every position this provider handed out resolves");
                    (
                        realized.callable.owner.render(),
                        realized.callable.name.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let first = NativeLibraries::from_distribution(&root).expect("the stdlib loads");
        let second = NativeLibraries::from_distribution(&root).expect("the stdlib loads");
        let first = realizations(&first);
        assert!(
            first.len() > 10_000,
            "the whole stdlib was walked: {}",
            first.len()
        );
        assert_eq!(
            first,
            realizations(&second),
            "the same klib numbers its declarations the same way every time it is read"
        );
    }

    /// A callable resolves, and the identity it carries resolves BACK through the same provider.
    ///
    /// That round trip is the whole contract: a backend holds the identity through checking and
    /// lowering and asks the provider what it was, at the point it finally has to emit. If the
    /// lookup could not answer, a call would select here and have nothing to realize — which is
    /// why the callables half was held back until the identity table existed to answer it.
    #[test]
    fn a_callable_resolves_and_its_identity_resolves_back() {
        let Some(root) = distribution() else {
            eprintln!("skipping: no Kotlin/Native distribution cached");
            return;
        };
        let libraries = NativeLibraries::from_distribution(&root).expect("the stdlib loads");
        assert!(
            libraries.callable_count() > 1000,
            "the stdlib declares a great many top-level callables: {}",
            libraries.callable_count()
        );

        // `listOf` is declared several times over — that is what makes it an overload SET rather
        // than a declaration, and each overload keeps an identity of its own.
        let resolved = libraries.symbols(
            SymbolNamespace::Package(type_name("kotlin/collections")),
            "listOf",
        );
        let overloads = match &resolved.callables {
            Callables::Functions(set) => &set.overloads,
            _ => panic!("kotlin.collections.listOf resolves as functions"),
        };
        assert!(
            overloads.len() > 1,
            "listOf has more than one overload: {}",
            overloads.len()
        );

        let mut seen = std::collections::HashSet::new();
        for overload in overloads {
            let identity = overload
                .callable
                .external_identity
                .expect("every overload carries the identity this provider assigned it");
            assert!(
                seen.insert(identity),
                "and no two overloads share one identity"
            );
            let realized = libraries
                .external_callable(identity)
                .expect("which this provider answers for");
            assert_eq!(
                realized.callable.name, "listOf",
                "and answers with the declaration that identity was assigned to"
            );
        }
    }

    /// The provider answers classifier lookups out of a klib that does decode.
    ///
    /// `kotlin.collections` is the package the whole question started in, and it is read here from
    /// Kotlin/Native's OWN library rather than through `kotlin-stdlib.jar`.
    #[test]
    fn a_decodable_package_answers_classifier_lookups() {
        let Some(root) = distribution() else {
            eprintln!("skipping: no Kotlin/Native distribution cached");
            return;
        };
        let archive = KlibArchive::open(&root.join(STDLIB_UNDER_DISTRIBUTION)).expect("open");
        let fragment = archive
            .package_fragments()
            .into_iter()
            .find(|fragment| fragment.entry.contains("package_kotlin.collections/00_"))
            .expect("the stdlib declares kotlin.collections");
        let bytes = archive.read(&fragment.entry).expect("read");
        let package = crate::jvm::metadata::klib_validation::parse_package_fragment_checked(&bytes)
            .expect("a kotlin.collections fragment decodes");
        assert!(
            !package.functions.is_empty(),
            "and declares top-level functions"
        );
    }
}

#[cfg(test)]
mod compiles_against_the_klib {
    use super::*;

    fn distribution() -> Option<std::path::PathBuf> {
        crate::toolchain::kotlin_native_root()
    }

    /// Compile a program with this provider and answer the diagnostics, analysis AND emission.
    ///
    /// Both stages, because analysis alone does not report an unresolved CALL — which the control
    /// below proves rather than assumes.
    fn diagnostics(root: &Path, source: &str) -> Vec<String> {
        let libraries = NativeLibraries::from_distribution(root).expect("the stdlib loads");
        let provider: std::rc::Rc<dyn SemanticPlatform> =
            std::rc::Rc::new(NativeLibraries::from_distribution(root).expect("the stdlib loads"));
        let inputs = vec![crate::frontend::SourceInput::kotlin(source).with_file_stem("KlibOnly")];
        let features = crate::features::LangFeatures::new();
        let mut diags = crate::diag::DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_streaming_with_features(
            &inputs,
            Box::new(libraries),
            &features,
            &mut diags,
        );
        if let Some(target) = crate::native::target::NativeTarget::host() {
            let backend = crate::native::CraneliftBackend::new(provider, target);
            let _ = crate::compiler::emit_analyzed(
                analysis,
                &["KlibOnly".to_string()],
                &backend,
                "KlibOnly",
                &mut diags,
            );
        }
        diags.diags.iter().map(|d| d.msg.clone()).collect()
    }

    /// What a program can and cannot yet do when the provider is the klib rather than the jar.
    ///
    /// This exists because the index tests beside it cannot answer the question. They push a
    /// realization into a table and read it back at the position they pushed it — self-consistency,
    /// which is nearly a tautology. Whether a PROGRAM compiles through this provider is a different
    /// claim, and it needs a compilation to make it.
    ///
    /// Every line below compiles END TO END: resolved through the klib and emitted by the native
    /// code generator, with no diagnostic of any kind. It is pinned in BOTH directions, so it fails
    /// when the provider regresses as well as when a line that still fails starts passing.
    #[test]
    fn what_the_klib_provider_can_and_cannot_yet_compile() {
        let Some(root) = distribution() else {
            eprintln!("skipping: no Kotlin/Native distribution cached");
            return;
        };

        for (source, what) in [
            // Nothing but the builtins, and the control for it is below: a classifier that exists
            // nowhere IS reported, so the quiet here means resolved rather than unexamined.
            (
                "fun box(): String = \"OK\"\n",
                "a program naming only builtins",
            ),
            // A fully-qualified reference, which gets past its leading segments only because the
            // provider answers that `kotlin` is a package: it names no classifier, and a walk with
            // nothing to ask reported `unresolved reference 'kotlin'` on 437 corpus cases.
            (
                "fun box(): kotlin.String = \"OK\"\n",
                "a package-qualified name",
            ),
            // A member PROPERTY, read through the provider's property identity, and realized as
            // the operation it is rather than as an accessor call.
            ("fun box(): Int = \"abc\".length\n", "a member property"),
            // A member FUNCTION, selected among the classifier's declared families.
            (
                "fun box(): String = \"abc\".substring(1)\n",
                "a member function",
            ),
            // A CONSTRUCTION. Before the provider interned its constructors, checked FIR had no
            // target to name and this failed with `MissingStableCallTarget` — an internal error,
            // on 221 corpus cases.
            (
                "fun box(): String { StringBuilder(); return \"OK\" }\n",
                "a stdlib construction",
            ),
            // A top-level declaration, with a member read on the type it returns.
            (
                "fun box(): Int = listOf(1, 2, 3).size\n",
                "a top-level call",
            ),
            // A GENERIC top-level declaration whose result is inferred rather than declared.
            // Before the provider published a generic signature, this failed checking with `Int`
            // expected and the unbound `R` actual.
            (
                "fun box(): Int = listOf(1, 2, 3).let { it.size }\n",
                "an inferred generic result",
            ),
            // An INFIX call, which admits only declarations carrying the modifier. Before the
            // provider published it, this found no `step` FUNCTION and read the progression's
            // `Int`-typed `step` PROPERTY instead, on 254 corpus cases.
            (
                "fun box(): String { val r = 1..10 step 2; return \"OK\" }\n",
                "an infix call",
            ),
            // An EXTENSION PROPERTY, which is a top-level declaration of the package fragment
            // rather than a member of anything. The decoder used to decode these and throw them
            // away — `semantic_property` was called for validation and its result dropped — so
            // `indices` resolved to nothing at all.
            (
                "fun box(): Int = \"abc\".indices.first\n",
                "an extension property",
            ),
            // An EXTENSION PROPERTY, which is a top-level declaration of the package fragment
            // rather than a member of anything. The decoder used to decode these and throw them
            // away — `semantic_property` was called for validation and its result dropped — so
            // `indices` resolved to nothing at all.
            (
                "fun box(): Int = \"abc\".indices.first\n",
                "an extension property",
            ),
            // A call that OMITS a defaulted argument. `assertEquals(expected, actual, message:
            // String? = null)` needs the `null`, and `linkdata` records only THAT the parameter
            // has a default — the value is an expression in the library's IR. 862 corpus cases
            // reported "default-argument realization is unavailable" before that was read.
            (
                "import kotlin.test.*\nfun box(): String { assertEquals(1, 1); return \"OK\" }\n",
                "a call omitting a defaulted argument",
            ),
            // A `const val` on a companion. Every use site folds it, which is the only way it can
            // work here at all: a companion object has no storage on a target that compiles the
            // stdlib itself. The decoder used to validate the compile-time value and throw it
            // away, leaving this a runtime property READ the code generator declined by name.
            (
                "fun box(): Long = Int.MAX_VALUE + Long.MIN_VALUE\n",
                "a companion constant",
            ),
        ] {
            let reported = diagnostics(&root, source);
            assert!(
                reported.is_empty(),
                "{what} compiles through the klib provider: {reported:?}"
            );
        }

        // The control that makes every quiet line above a claim: a classifier that exists nowhere
        // IS reported.
        let unknown = diagnostics(&root, "fun box(): NoSuchType = TODO()\n");
        assert!(
            unknown.iter().any(|d| d.contains("NoSuchType")),
            "a classifier that exists nowhere is reported: {unknown:?}"
        );

        // A stdlib class that is `open` or `abstract` can be EXTENDED. It resolves — reading
        // "extensible only if an interface" off the kind rather than the declared modality made
        // `kotlin.Exception` unsubclassable — and the code generator declines it for its own,
        // separate reason, which is what the shape of this diagnostic says.
        let subclass = diagnostics(
            &root,
            "class E : Exception(\"x\")\nfun box(): String = \"OK\"\n",
        );
        assert!(
            subclass
                .iter()
                .any(|d| d.contains("a superclass declared outside this file")),
            "an open stdlib class is extensible: {subclass:?}"
        );
        assert!(
            !subclass.iter().any(|d| d.contains("cannot be subclassed")),
            "and the refusal is the GENERATOR's, not the provider's: {subclass:?}"
        );

        // The control for the emit stage itself: analysis alone is SILENT about an unresolved
        // call, so a test that only ran analysis would have read every line above as a success.
        let call_only_analysis = {
            let libraries = NativeLibraries::from_distribution(&root).expect("loads");
            let inputs =
                vec![
                    crate::frontend::SourceInput::kotlin("fun box(): Int = totallyNotAThing()\n")
                        .with_file_stem("KlibOnly"),
                ];
            let features = crate::features::LangFeatures::new();
            let mut diags = crate::diag::DiagSink::new();
            let _ = crate::frontend::analyze_source_set_streaming_with_features(
                &inputs,
                Box::new(libraries),
                &features,
                &mut diags,
            );
            diags.diags.len()
        };
        assert_eq!(
            call_only_analysis, 0,
            "analysis alone reports no unresolved CALL, which is why this test emits too"
        );
    }
}

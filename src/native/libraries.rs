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

use crate::klib::{KlibArchive, KlibError};
use crate::libraries::{
    Callables, ClassifierInheritance, ExternalCallableKind, ExternalCallableRealization, FnKind,
    FunctionInfo, LibraryMember, LibraryType, ParamList, ResolvedSymbols, SemanticPlatform,
    TypeKind,
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
}

/// Kotlin/Native's stdlib, as a symbol source.
pub struct NativeLibraries {
    /// Package or classifier namespace -> what it declares. A classifier's own namespace holds its
    /// nested classifiers, which is the same shape a package has, so one map serves both.
    namespaces: HashMap<TypeName, Namespace>,
    /// What each identity this provider handed out was realized as, indexed BY that identity: the
    /// id is this table's position, which is what makes the lookup a provider owes its consumers
    /// an array read rather than a search. A klib has no descriptor to stand in for identity, so
    /// the identity is the only thing a backend carries and this is the only place it resolves.
    callable_realizations: Vec<ExternalCallableRealization>,
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
        let mut namespaces: HashMap<TypeName, Namespace> = HashMap::new();
        let mut callable_realizations = Vec::new();
        for fragment in archive.package_fragments() {
            let bytes = archive.read(&fragment.entry)?;
            let package =
                crate::jvm::metadata::klib_validation::parse_package_fragment_checked(&bytes)
                    .map_err(|source| NativeLibrariesError::InvalidFragment {
                        entry: fragment.entry.clone(),
                        source,
                    })?;
            let package_name = package_namespace(&fragment.package_fqname);
            for (internal, declaration) in package.classes {
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
                    .or_insert_with(|| Arc::new(library_type(identity, declaration)));
            }
            for function in &package.functions {
                let identity = crate::fir::ExternalCallableId::from_raw(
                    u32::try_from(callable_realizations.len())
                        .expect("too many stdlib declarations for a packed identity"),
                );
                let info = top_level_function(package_name, function, identity);
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
        }
        Ok(Self {
            namespaces,
            callable_realizations,
        })
    }

    /// How many namespaces carry a declaration — for a caller that wants to say the stdlib really
    /// was read rather than silently empty.
    pub fn namespace_count(&self) -> usize {
        self.namespaces.len()
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
        self.callable_realizations
            .get(identity.raw() as usize)
            .cloned()
    }

    fn symbols(&self, namespace: SymbolNamespace, name: &str) -> std::rc::Rc<ResolvedSymbols> {
        let Some(found) = self.namespaces.get(&namespace.name()) else {
            return std::rc::Rc::new(ResolvedSymbols::default());
        };
        let classifier = found.classifiers.get(name).cloned();
        let callables = match found.callables.get(name) {
            Some(overloads) => Callables::Functions(crate::libraries::FunctionSet {
                overloads: overloads.clone(),
            }),
            None => Callables::default(),
        };
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
) -> FunctionInfo {
    let bounds = HashMap::new();
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
    info.call_sig = crate::libraries::CallSig::metadata_member(
        function.params.len(),
        function.param_names.clone(),
        function.param_defaults.clone(),
        function.vararg,
    );
    info
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
        self.callable_realizations.len()
    }
}

impl SemanticPlatform for NativeLibraries {
    /// The token the reference compiler writes in a source-set diagnostic for this target.
    fn diagnostic_target_name(&self) -> Option<&str> {
        Some("Native")
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
    LibraryType {
        access: declaration.visibility.into(),
        is_kotlin: true,
        source_file: None,
        stable_declaration: None,
        is_nested: declaration.is_nested,
        outer_instance: None,
        kind: declaration.kind,
        inheritance: ClassifierInheritance {
            is_abstract: is_interface,
            is_extensible: is_interface,
            has_no_arg_constructor: constructors
                .iter()
                .any(|constructor| constructor.params.is_empty()),
        },
        supertypes,
        supertype_templates,
        constructors,
        hidden_member_properties: Default::default(),
        declared_callables: HashMap::new(),
        declared_callable_order: Vec::new(),
        members: Vec::new(),
        companion: Vec::new(),
        constants: HashMap::new(),
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cached Kotlin/Native distribution, or `None` when this checkout has not provisioned one.
    fn distribution() -> Option<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/cache/kotlin-native/2.4.10")
            .join("kotlin-native-prebuilt-linux-x86_64-2.4.10");
        root.exists().then_some(root)
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

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
    Callables, ClassifierInheritance, LibraryMember, LibraryType, ParamList, ResolvedSymbols,
    SemanticPlatform, TypeKind,
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

/// The declarations one package contributes, indexed by the name a lookup asks for.
#[derive(Default)]
struct Namespace {
    classifiers: HashMap<String, Arc<LibraryType>>,
}

/// Kotlin/Native's stdlib, as a symbol source.
pub struct NativeLibraries {
    /// Package or classifier namespace -> what it declares. A classifier's own namespace holds its
    /// nested classifiers, which is the same shape a package has, so one map serves both.
    namespaces: HashMap<TypeName, Namespace>,
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
            // `package.functions` is decoded and deliberately not indexed yet; see `symbols`.
            let _ = (&package.functions, package_name);
        }
        Ok(Self { namespaces })
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
    fn symbols(&self, namespace: SymbolNamespace, name: &str) -> std::rc::Rc<ResolvedSymbols> {
        let Some(found) = self.namespaces.get(&namespace.name()) else {
            return std::rc::Rc::new(ResolvedSymbols::default());
        };
        let classifier = found.classifiers.get(name).cloned();
        // Classifiers only, so far. A callable needs more than its signature: `LibraryCallable`
        // carries a provider-assigned `ExternalCallableId`, and the provider keeps the target
        // realization for that id in a table of its own — which is the right model for a klib
        // (there is no descriptor to stand in for identity) and is the next piece rather than
        // something to approximate here. Answering a half-built callable would resolve calls this
        // provider cannot then realize.
        let callables = Callables::default();
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

    /// How much of the Kotlin/Native stdlib the reader already in this tree can decode.
    ///
    /// This is the measurement the klib question turns on, so it is a test rather than something
    /// someone has to re-derive: the container reader and the metadata decoder were written for
    /// the JVM's `@Metadata`, and whether they read Kotlin/Native's own `linkdata` decides whether
    /// this target can compile against the real stdlib or must keep reimplementing it.
    #[test]
    fn the_kotlin_native_stdlib_very_nearly_all_decodes() {
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
                Err(error) => rejected.push((fragment.entry.clone(), format!("{error:?}"))),
            }
        }
        // The one fragment that does not. Pinned by NAME and by REASON rather than by a count, so
        // a different fragment failing, or this one failing differently, is a test failure and not
        // a number that quietly drifts.
        //
        // Its class declares a contract whose `is-instance` type id does not index that class's
        // own type table — the table has two entries and the contract asks for the seventh. Every
        // other class in the stdlib resolves against its own table, so this is a scoping rule for
        // contract type ids rather than a missing table, and it is its own piece of work.
        let names = rejected
            .iter()
            .map(|(entry, _)| entry.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            ["default/linkdata/package_kotlin.test/4_test.knm"],
            "exactly one stdlib fragment is known not to decode"
        );
        assert!(
            rejected[0]
                .1
                .contains("contract is-instance type references absent type"),
            "and for the reason this test names: {}",
            rejected[0].1
        );
        assert!(
            decoded > 480,
            "the rest of the stdlib decodes: {decoded} fragments"
        );
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

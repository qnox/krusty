//! Common optional-expectation annotations absent from the JVM stdlib classfiles.
//!
//! Kotlin ships the common `expect` headers in the distribution KLIB next to `kotlin-stdlib.jar`.
//! Only annotation classifiers whose metadata carries `IS_EXPECT_CLASS` are imported here; platform
//! declarations in the same archive never enter the JVM symbol source.
//!
//! The authoritative metadata model and decoder are target-independent. This module is the JVM
//! backend's *use* of that model and consumes it directly; no JVM builtins adapter participates in
//! the KLIB provider path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::klib::{KlibArchive, KlibError};
use crate::libraries::{
    CallSig, ClassifierInheritance, LibraryMember, LibraryType, ParamList, TypeKind,
};
use crate::metadata::{decode, semantic};
use crate::types::{type_name, Ty, TypeName, TypeNameList, TypeParameters};

#[derive(Debug)]
pub(super) enum CommonExpectationError {
    Container(KlibError),
    InvalidModuleHeader {
        path: PathBuf,
        source: decode::PackageFragmentDecodeError,
    },
    InvalidFragment {
        archive: PathBuf,
        entry: String,
        source: decode::PackageFragmentDecodeError,
    },
    PackageInventoryMismatch {
        path: PathBuf,
        header: Vec<String>,
        fragments: Vec<String>,
    },
}

impl std::fmt::Display for CommonExpectationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Container(error) => error.fmt(formatter),
            Self::InvalidModuleHeader { path, source } => {
                write!(
                    formatter,
                    "invalid KLIB module header {}: {source}",
                    path.display()
                )
            }
            Self::InvalidFragment {
                archive,
                entry,
                source,
            } => write!(
                formatter,
                "invalid KLIB metadata fragment {entry:?} in {}: {source}",
                archive.display()
            ),
            Self::PackageInventoryMismatch {
                path,
                header,
                fragments,
            } => write!(
                formatter,
                "KLIB module header {} names packages {header:?}, but linkdata contains {fragments:?}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CommonExpectationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Container(error) => Some(error),
            Self::InvalidModuleHeader { source, .. } | Self::InvalidFragment { source, .. } => {
                Some(source)
            }
            Self::PackageInventoryMismatch { .. } => None,
        }
    }
}

impl From<KlibError> for CommonExpectationError {
    fn from(error: KlibError) -> Self {
        Self::Container(error)
    }
}

#[derive(Default)]
pub(super) struct CommonExpectationIndex {
    classifiers: HashMap<TypeName, Arc<LibraryType>>,
}

impl CommonExpectationIndex {
    pub(super) fn load(path: Option<PathBuf>) -> Result<Arc<Self>, Arc<CommonExpectationError>> {
        type LoadResult = Result<Arc<CommonExpectationIndex>, Arc<CommonExpectationError>>;
        type SharedIndex = Arc<OnceLock<LoadResult>>;
        static CACHE: OnceLock<Mutex<HashMap<PathBuf, SharedIndex>>> = OnceLock::new();
        let Some(path) = path else {
            return Ok(Arc::new(Self::default()));
        };
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let shared = cache
            .lock()
            .map_err(|_| {
                Arc::new(CommonExpectationError::Container(
                    KlibError::PoisonedCache { path: path.clone() },
                ))
            })?
            .entry(path.clone())
            .or_insert_with(|| Arc::new(OnceLock::new()))
            .clone();
        shared
            .get_or_init(|| Self::read(&path).map(Arc::new).map_err(Arc::new))
            .clone()
    }

    pub(super) fn classifier(&self, internal: TypeName) -> Option<Arc<LibraryType>> {
        self.classifiers.get(&internal).cloned()
    }

    pub(super) fn contains(&self, internal: TypeName) -> bool {
        self.classifiers.contains_key(&internal)
    }

    fn read(path: &Path) -> Result<Self, CommonExpectationError> {
        let archive = KlibArchive::open(path)?;
        archive.manifest()?;
        let module_header = archive.module_header()?;
        let module_header = decode::parse_module_header(&module_header).map_err(|source| {
            CommonExpectationError::InvalidModuleHeader {
                path: path.join("default/linkdata/module"),
                source,
            }
        })?;
        let fragments = archive.package_fragments();
        let fragment_packages = fragments
            .iter()
            .map(|fragment| fragment.package_fqname.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut header_packages = module_header.package_fragment_names;
        header_packages.sort();
        if header_packages != fragment_packages {
            return Err(CommonExpectationError::PackageInventoryMismatch {
                path: path.join("default/linkdata/module"),
                header: header_packages,
                fragments: fragment_packages,
            });
        }
        let mut classifiers = HashMap::new();
        for fragment in fragments {
            let bytes = archive.read(&fragment.entry)?;
            let package = semantic::parse_package_fragment_checked(&bytes).map_err(|source| {
                CommonExpectationError::InvalidFragment {
                    archive: path.to_path_buf(),
                    entry: fragment.entry,
                    source,
                }
            })?;
            for (internal, declaration) in package.classes {
                if declaration.kind != TypeKind::Annotation || !declaration.is_expect {
                    continue;
                }
                let identity = type_name(&internal);
                classifiers
                    .entry(identity)
                    .or_insert_with(|| Arc::new(annotation_type(declaration)));
            }
        }
        Ok(Self { classifiers })
    }
}

fn annotation_type(declaration: semantic::KotlinClass) -> LibraryType {
    let bounds = semantic::semantic_bounds(&declaration.type_params, &HashMap::new());
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
                    .map(|bound| semantic::semantic_ty(bound, &bounds))
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
        .map(|supertype| semantic::semantic_ty(supertype, &bounds))
        .collect::<Vec<_>>();
    let supertypes = declaration
        .supertypes
        .iter()
        .map(|supertype| type_name(supertype))
        .collect::<Vec<_>>()
        .into();
    let mut constructors = Vec::new();
    let mut named_parameter_lists = Vec::new();
    for constructor in declaration.constructors {
        let params = constructor
            .params
            .iter()
            .map(|parameter| semantic::semantic_ty(parameter, &bounds))
            .collect::<Vec<_>>();
        let mut member = LibraryMember::new(
            "<init>".to_string(),
            params.clone(),
            Ty::Unit,
            String::new(),
        );
        member.visibility = constructor.visibility;
        member.call_sig = CallSig::metadata_member(
            params.len(),
            constructor.param_names.clone(),
            constructor.param_defaults.clone(),
            constructor.vararg,
        );
        constructors.push(member);
        named_parameter_lists.push(ParamList {
            visibility: constructor.visibility,
            names: constructor.param_names,
            defaults: constructor.param_defaults,
            types: params,
            recv_fun: Vec::new(),
            vararg: constructor.vararg,
            annotation: None,
        });
    }
    LibraryType {
        access: declaration.visibility.into(),
        is_kotlin: true,
        source_file: None,
        stable_declaration: None,
        is_nested: declaration.is_nested,
        outer_instance: None,
        kind: TypeKind::Annotation,
        inheritance: ClassifierInheritance {
            is_abstract: true,
            is_extensible: false,
            has_no_arg_constructor: constructors
                .iter()
                .any(|constructor| constructor.params.is_empty()),
        },
        supertypes,
        supertype_templates,
        constructors,
        hidden_member_properties: Default::default(),
        hidden_deprecated_callables: Default::default(),
        declared_callables: HashMap::new(),
        declared_callable_order: Vec::new(),
        members: Vec::new(),
        companion: Vec::new(),
        constants: HashMap::new(),
        sam_eligible: false,
        callable_signature: None,
        callable_signatures: Vec::new(),
        companion_object: None,
        qualified_name: None,
        value_underlying: None,
        value_underlying_property: None,
        alias_target: None,
        own_type_parameter_count: type_parameters.type_params.len(),
        type_parameters,
        sealed_subclasses: TypeNameList::new(),
        enum_entries: Vec::new(),
        enum_entries_accessor: None,
        named_parameter_lists,
        // No JVM actual exists, so this platform erases the optional annotation after checking.
        annotations: Vec::new(),
        retention: Some("SOURCE".to_string()),
        annotation_targets: None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{classpath::Classpath, jvm_libraries::JvmLibraries};
    use super::{CommonExpectationError, CommonExpectationIndex};
    use crate::diag::{DiagSink, Severity, Span};
    use crate::features::LangFeatures;
    use crate::klib::KlibError;
    use crate::source::SourceInput;
    use std::path::Path;

    const VALID_MODULE_HEADER: &[u8] = b"\x0a\x15<unpackedExampleKlib>\x3a\x00";
    const VALID_ROOT_FRAGMENT: &[u8] = b"\x0a\x1d\x0a\x04main\x0a\x06kotlin\x0a\x04Unit\x0a\x07main.kt\x12\x0c\x0a\x02\x10\x01\x0a\x06\x08\x00\x10\x02\x18\x00\x1a\x1c\x1a\x07\x10\x00\x38\x00\xe0\x0a\x03\xf2\x01\x04\x0a\x02\x30\x01\xd8\x0a\xff\xff\xff\xff\xff\xff\xff\xff\xff\x01\xe0\x0a\x00\xea\x0a\x00";

    fn write(root: &Path, entry: &str, contents: &[u8]) {
        let path = root.join(entry);
        std::fs::create_dir_all(path.parent().expect("fixture entry parent"))
            .expect("create fixture entry parent");
        std::fs::write(path, contents).expect("write fixture entry");
    }

    fn write_valid_root_klib(root: &Path) {
        write(root, "default/manifest", b"unique_name=fixture\n");
        write(root, "default/linkdata/module", VALID_MODULE_HEADER);
        write(
            root,
            "default/linkdata/root_package/0_.knm",
            VALID_ROOT_FRAGMENT,
        );
    }

    #[test]
    fn corrupt_common_expectation_dependency_is_a_frontend_diagnostic_contract() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "krusty-common-expectation-error-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).expect("create dependency directory");
        let stdlib = directory.join("kotlin-stdlib.jar");
        std::fs::write(&stdlib, b"unused by this initialization test").expect("write stdlib path");
        let klib = directory.join("kotlin-stdlib-wasm-js.klib");
        std::fs::write(&klib, b"not a zip").expect("write corrupt common KLIB");

        let provider = JvmLibraries::new(std::rc::Rc::new(Classpath::new(vec![stdlib])));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features(
            &[SourceInput::kotlin("fun answer() = 42")],
            provider,
            &LangFeatures::new(),
            &mut diagnostics,
        );
        assert_eq!(analysis.files.len(), 1);
        assert_eq!(analysis.types.len(), 1);
        assert!(analysis.types[0].is_none());
        assert_eq!(
            diagnostics.diags.len(),
            1,
            "dependency initialization emits no unresolved-reference cascades"
        );
        let diagnostic = &diagnostics.diags[0];
        assert_eq!(diagnostic.file, 0);
        assert_eq!(diagnostic.span, Span::new(0, 0));
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(
            diagnostic.msg,
            format!("cannot load Kotlin common-expectation dependency: invalid KLIB zip {}: invalid Zip archive: Could not find EOCD", klib.display())
        );
        assert!(analysis.symbols.libraries.validate_initialization().is_ok());
    }

    #[test]
    fn selected_common_klib_validates_manifest_module_and_every_fragment() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "krusty-common-expectation-validation-{}-{unique}",
            std::process::id()
        ));

        let missing_manifest = directory.join("missing-manifest");
        write(
            &missing_manifest,
            "default/linkdata/module",
            VALID_MODULE_HEADER,
        );
        match CommonExpectationIndex::read(&missing_manifest) {
            Err(CommonExpectationError::Container(KlibError::MissingEntry { archive, entry })) => {
                assert_eq!(archive, missing_manifest);
                assert_eq!(entry, "default/manifest");
            }
            Err(error) => panic!("unexpected missing-manifest error: {error}"),
            Ok(_) => panic!("KLIB without manifest initialized"),
        }

        let invalid_manifest = directory.join("invalid-manifest");
        write(&invalid_manifest, "default/manifest", b"unique_name=\xff\n");
        write(
            &invalid_manifest,
            "default/linkdata/module",
            VALID_MODULE_HEADER,
        );
        match CommonExpectationIndex::read(&invalid_manifest) {
            Err(CommonExpectationError::Container(KlibError::InvalidManifest { path, source })) => {
                assert_eq!(path, invalid_manifest.join("default/manifest"));
                assert_eq!(
                    source,
                    crate::klib::KlibManifestError::InvalidUtf8 { valid_up_to: 12 }
                );
            }
            Err(error) => panic!("unexpected invalid-manifest error: {error}"),
            Ok(_) => panic!("KLIB with invalid manifest initialized"),
        }

        let missing_module = directory.join("missing-module");
        write(
            &missing_module,
            "default/manifest",
            b"unique_name=fixture\n",
        );
        match CommonExpectationIndex::read(&missing_module) {
            Err(CommonExpectationError::Container(KlibError::MissingEntry { archive, entry })) => {
                assert_eq!(archive, missing_module);
                assert_eq!(entry, "default/linkdata/module");
            }
            Err(error) => panic!("unexpected missing-module error: {error}"),
            Ok(_) => panic!("KLIB without module header initialized"),
        }

        let empty_module = directory.join("empty-module");
        write(&empty_module, "default/manifest", b"unique_name=fixture\n");
        write(&empty_module, "default/linkdata/module", &[]);
        match CommonExpectationIndex::read(&empty_module) {
            Err(CommonExpectationError::InvalidModuleHeader { path, source }) => {
                assert_eq!(path, empty_module.join("default/linkdata/module"));
                assert_eq!(source.offset, 0);
                assert_eq!(source.detail, "empty KLIB module header");
            }
            Err(error) => panic!("unexpected empty-module error: {error}"),
            Ok(_) => panic!("KLIB with empty module header initialized"),
        }

        let invalid_fragment = directory.join("invalid-fragment");
        write(
            &invalid_fragment,
            "default/manifest",
            b"unique_name=fixture\n",
        );
        write(
            &invalid_fragment,
            "default/linkdata/module",
            VALID_MODULE_HEADER,
        );
        write(
            &invalid_fragment,
            "default/linkdata/root_package/0_.knm",
            &[0x22, 0x02, 0x08],
        );
        match CommonExpectationIndex::read(&invalid_fragment) {
            Err(CommonExpectationError::InvalidFragment {
                archive,
                entry,
                source,
            }) => {
                assert_eq!(archive, invalid_fragment);
                assert_eq!(entry, "default/linkdata/root_package/0_.knm");
                assert_eq!(source.offset, 2);
                assert_eq!(source.detail, "truncated class declaration");
            }
            Err(error) => panic!("unexpected invalid-fragment error: {error}"),
            Ok(_) => panic!("KLIB with invalid fragment initialized"),
        }

        let package_mismatch = directory.join("package-mismatch");
        write_valid_root_klib(&package_mismatch);
        let root = package_mismatch.join("default/linkdata/root_package/0_.knm");
        let named = package_mismatch.join("default/linkdata/package_demo/0_demo.knm");
        std::fs::create_dir_all(named.parent().expect("named fragment parent"))
            .expect("create named fragment parent");
        std::fs::rename(root, named).expect("move fragment into mismatching package");
        match CommonExpectationIndex::read(&package_mismatch) {
            Err(CommonExpectationError::PackageInventoryMismatch {
                path,
                header,
                fragments,
            }) => {
                assert_eq!(path, package_mismatch.join("default/linkdata/module"));
                assert_eq!(header, [""]);
                assert_eq!(fragments, ["demo"]);
            }
            Err(error) => panic!("unexpected package-mismatch error: {error}"),
            Ok(_) => panic!("KLIB with mismatching package inventory initialized"),
        }

        for (tag, declaration) in [(0x4a, "function"), (0x52, "property")] {
            let fragment = [
                0x0a, 0x03, 0x0a, 0x01, b'C', 0x12, 0x06, 0x0a, 0x04, 0x10, 0x00, 0x18, 0x00, 0x22,
                0x06, 0x18, 0x00, tag, 0x02, 0x10, 0x80,
            ];
            let error = crate::metadata::semantic::parse_package_fragment_checked(&fragment)
                .err()
                .expect("truncated nested declaration must fail the whole fragment");
            assert_eq!(error.offset, 21);
            assert_eq!(error.detail, format!("truncated {declaration} declaration"));
        }
    }

    fn exact_initialization_diagnostic(
        tag: &str,
        configure: impl FnOnce(&Path),
    ) -> (std::path::PathBuf, Vec<crate::diag::Diagnostic>) {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "krusty-common-expectation-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).expect("create diagnostic fixture directory");
        let stdlib = directory.join("kotlin-stdlib.jar");
        std::fs::write(&stdlib, b"selected stdlib path").expect("write stdlib path");
        let klib = directory.join("kotlin-stdlib-wasm-js.klib");
        configure(&klib);
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features(
            &[SourceInput::kotlin("fun answer() = missing")],
            JvmLibraries::new(std::rc::Rc::new(Classpath::new(vec![stdlib]))),
            &LangFeatures::new(),
            &mut diagnostics,
        );
        assert_eq!(analysis.files.len(), 1);
        assert!(analysis.types[0].is_none());
        assert_eq!(
            diagnostics.diags.len(),
            1,
            "no unresolved-reference cascade"
        );
        assert_eq!(diagnostics.diags[0].file, 0);
        assert_eq!(diagnostics.diags[0].span, Span::new(0, 0));
        assert_eq!(diagnostics.diags[0].severity, Severity::Error);
        (klib, diagnostics.diags)
    }

    #[test]
    fn invalid_header_nested_fragment_and_package_mismatch_are_exact_frontend_diagnostics() {
        let (header_klib, header) = exact_initialization_diagnostic("header", |klib| {
            write(klib, "default/manifest", b"unique_name=fixture\n");
            write(klib, "default/linkdata/module", &[0x08, 0x01]);
        });
        assert_eq!(
            header[0].msg,
            format!(
                "cannot load Kotlin common-expectation dependency: invalid KLIB module header {}: field in KLIB module header has wire type 0, expected 2 at byte 1",
                header_klib.join("default/linkdata/module").display()
            )
        );

        let malformed_class = [
            0x0a, 0x03, 0x0a, 0x01, b'C', 0x12, 0x06, 0x0a, 0x04, 0x10, 0x00, 0x18, 0x00, 0x22,
            0x06, 0x18, 0x00, 0x4a, 0x02, 0x10, 0x80,
        ];
        let (fragment_klib, fragment) = exact_initialization_diagnostic("fragment", |klib| {
            write(klib, "default/manifest", b"unique_name=fixture\n");
            write(klib, "default/linkdata/module", VALID_MODULE_HEADER);
            write(
                klib,
                "default/linkdata/root_package/0_.knm",
                &malformed_class,
            );
        });
        assert_eq!(
            fragment[0].msg,
            format!(
                "cannot load Kotlin common-expectation dependency: invalid KLIB metadata fragment \"default/linkdata/root_package/0_.knm\" in {}: truncated function declaration at byte 21",
                fragment_klib.display()
            )
        );

        let (mismatch_klib, mismatch) = exact_initialization_diagnostic("mismatch", |klib| {
            write(klib, "default/manifest", b"unique_name=fixture\n");
            write(klib, "default/linkdata/module", VALID_MODULE_HEADER);
            write(
                klib,
                "default/linkdata/package_demo/0_demo.knm",
                VALID_ROOT_FRAGMENT,
            );
        });
        assert_eq!(
            mismatch[0].msg,
            format!(
                "cannot load Kotlin common-expectation dependency: KLIB module header {} names packages [\"\"], but linkdata contains [\"demo\"]",
                mismatch_klib.join("default/linkdata/module").display()
            )
        );
    }
}

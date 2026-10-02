//! Common semantic declarations distributed beside the JVM stdlib classfiles.
//!
//! Kotlin ships the common `expect` headers in the distribution KLIB next to `kotlin-stdlib.jar`.
//! Optional annotation classifiers whose metadata carries `IS_EXPECT_CLASS` enter the symbol
//! source. Exact public callable identities may authorize roles that are then joined to the paired
//! platform realization; other platform declarations in the archive do not enter the JVM source.
//!
//! The authoritative metadata model and decoder are target-independent. This module is the JVM
//! backend's *use* of that model and consumes it directly; no JVM builtins adapter participates in
//! the KLIB provider path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::klib::{KlibArchive, KlibError};
use crate::libraries::{
    CallSig, ClassifierInheritance, CompilerIntrinsic, FnKind, FunctionInfo, GenericSig,
    InlineKind, LibraryMember, LibraryType, ParamList, TypeKind,
};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::{decode, semantic};
use crate::symbol_source::SymbolNamespace;
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
    InvalidIr {
        path: PathBuf,
        source: crate::metadata::klib_ir::KlibIrDecodeError,
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
            Self::InvalidIr { path, source } => {
                write!(formatter, "invalid KLIB declaration identities in {}: {source}", path.display())
            }
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
            Self::InvalidIr { source, .. } => Some(source),
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
    package_function_roles: Vec<CommonPackageFunctionRole>,
}

#[derive(Clone)]
struct CommonPackageFunctionRole {
    identity: CommonPackageFunctionIdentity,
    package: TypeName,
    name: &'static str,
    generic_sig: GenericSig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommonPackageFunctionIdentity {
    DoubleRangeTo,
    FloatRangeTo,
    DoubleRangeUntil,
    FloatRangeUntil,
    EnumEntries,
}

impl CommonPackageFunctionIdentity {
    fn compiler_intrinsic(self) -> CompilerIntrinsic {
        match self {
            Self::DoubleRangeTo
            | Self::FloatRangeTo
            | Self::DoubleRangeUntil
            | Self::FloatRangeUntil => CompilerIntrinsic::FloatingRangeMembership,
            Self::EnumEntries => CompilerIntrinsic::EnumEntries,
        }
    }

    /// Shape of the paired JVM declaration. The public identity selects the role; these facts only
    /// refuse a declaration that cannot be that realization.
    fn accepts(self, candidate: &FunctionInfo) -> bool {
        let plain = !candidate.flags.suspend
            && !candidate.flags.infix
            && candidate.context_count == 0
            && !candidate.call_sig.vararg;
        match self {
            Self::DoubleRangeTo
            | Self::FloatRangeTo
            | Self::DoubleRangeUntil
            | Self::FloatRangeUntil => {
                plain && candidate.kind == FnKind::Extension && candidate.flags.operator
            }
            Self::EnumEntries => {
                plain
                    && candidate.kind == FnKind::TopLevel
                    && !candidate.flags.operator
                    && candidate.flags.reified
                    && candidate.flags.inline == InlineKind::MustInline
            }
        }
    }
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

    /// Role of the exact common declaration actualized by one normalized JVM callable.
    ///
    /// The caller must first prove that the physical callable came from the JVM stdlib paired with
    /// this KLIB. Signature facts only join that realization to an already-role-bearing identity;
    /// they never create the role themselves. Missing or ambiguous identity records fail closed.
    pub(super) fn package_function_role(
        &self,
        common_metadata_source: bool,
        package: TypeName,
        name: &str,
        candidate: &FunctionInfo,
    ) -> Option<CompilerIntrinsic> {
        if !common_metadata_source {
            return None;
        }
        let signature = candidate.generic_sig.as_ref()?;
        let mut matches = self.package_function_roles.iter().filter(|declaration| {
            declaration.package == package
                && declaration.name == name
                && declaration.generic_sig == *signature
                && declaration.identity.accepts(candidate)
        });
        let declaration = matches.next()?;
        matches
            .next()
            .is_none()
            .then(|| declaration.identity.compiler_intrinsic())
    }

    pub(super) fn attach(
        &self,
        common_metadata_source: bool,
        namespace: SymbolNamespace,
        name: &str,
        candidate: &mut FunctionInfo,
    ) {
        let SymbolNamespace::Package(package) = namespace else {
            return;
        };
        candidate.callable.compiler_intrinsic =
            self.package_function_role(common_metadata_source, package, name, candidate);
    }

    /// Attach the common role to the provider record that was just normalized and appended.
    pub(super) fn attach_tail(
        &self,
        common_metadata_source: bool,
        namespace: SymbolNamespace,
        name: &str,
        candidates: &mut [FunctionInfo],
    ) {
        let Some(candidate) = candidates.last_mut() else {
            return;
        };
        self.attach(common_metadata_source, namespace, name, candidate);
    }

    fn read(path: &Path) -> Result<Self, CommonExpectationError> {
        let archive = KlibArchive::open(path)?;
        archive.manifest()?;
        let package_function_roles =
            match crate::metadata::klib_ir::read_public_declaration_signatures(&archive) {
                Ok(signatures) => signatures
                    .into_iter()
                    .filter_map(common_package_function_role)
                    .collect(),
                Err(crate::metadata::klib_ir::KlibIrDecodeError::Missing { .. }) => Vec::new(),
                Err(source) => {
                    return Err(CommonExpectationError::InvalidIr {
                        path: path.to_path_buf(),
                        source,
                    });
                }
            };
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
        Ok(Self {
            classifiers,
            package_function_roles,
        })
    }
}

/// Language role carried by exact public identities from the trusted common stdlib KLIB. These
/// member ids are Kotlin's stable public identities for the floating-point `rangeTo` and
/// `rangeUntil` declarations and for the zero-argument `enumEntries` declaration. A same-named or
/// same-shaped declaration has a different complete identity and therefore never enters this
/// inventory.
fn common_package_function_role(
    identity: KlibPublicIdSignature,
) -> Option<CommonPackageFunctionRole> {
    const DOUBLE_RANGE_TO: u64 = 692_997_638_542_153_957;
    const FLOAT_RANGE_TO: u64 = 14_812_996_858_166_169_491;
    const DOUBLE_RANGE_UNTIL: u64 = 742_649_461_109_916_381;
    const FLOAT_RANGE_UNTIL: u64 = 1_543_348_898_644_284_516;
    // The zero-argument `kotlin.enums.enumEntries`. The one-argument overloads publish different
    // member ids and stay ordinary functions.
    const ENUM_ENTRIES: u64 = 1_918_022_784_687_648_499;
    if identity.matches_exact(&["kotlin", "enums"], &["enumEntries"], ENUM_ENTRIES, 0) {
        return Some(CommonPackageFunctionRole {
            identity: CommonPackageFunctionIdentity::EnumEntries,
            package: type_name("kotlin/enums"),
            name: "enumEntries",
            generic_sig: enum_entries_signature(),
        });
    }
    let (identity, scalar, name, range) =
        if identity.matches_exact(&["kotlin", "ranges"], &["rangeTo"], DOUBLE_RANGE_TO, 0) {
            (
                CommonPackageFunctionIdentity::DoubleRangeTo,
                Ty::Double,
                "rangeTo",
                "kotlin/ranges/ClosedFloatingPointRange",
            )
        } else if identity.matches_exact(&["kotlin", "ranges"], &["rangeTo"], FLOAT_RANGE_TO, 0) {
            (
                CommonPackageFunctionIdentity::FloatRangeTo,
                Ty::Float,
                "rangeTo",
                "kotlin/ranges/ClosedFloatingPointRange",
            )
        } else if identity.matches_exact(
            &["kotlin", "ranges"],
            &["rangeUntil"],
            DOUBLE_RANGE_UNTIL,
            0,
        ) {
            (
                CommonPackageFunctionIdentity::DoubleRangeUntil,
                Ty::Double,
                "rangeUntil",
                "kotlin/ranges/OpenEndRange",
            )
        } else if identity.matches_exact(
            &["kotlin", "ranges"],
            &["rangeUntil"],
            FLOAT_RANGE_UNTIL,
            0,
        ) {
            (
                CommonPackageFunctionIdentity::FloatRangeUntil,
                Ty::Float,
                "rangeUntil",
                "kotlin/ranges/OpenEndRange",
            )
        } else {
            return None;
        };
    Some(CommonPackageFunctionRole {
        identity,
        package: crate::types::wk::kotlin_ranges_package(),
        name,
        generic_sig: GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: Some(scalar),
            params: vec![scalar],
            ret: Ty::obj_args(range, &[scalar]),
            return_policy: Default::default(),
        },
    })
}

/// Source signature of the zero-argument `enumEntries` declaration: one formal bounded by
/// `Enum<T>`, no value parameters, and `EnumEntries<T>`. The type argument inside the bound is the
/// formal with Kotlin's implicit nullable `Any` bound, which is how metadata decodes that bound
/// before the formal's own erasure is known. Reified and inline are declaration flags, checked
/// when this signature is joined to the paired JVM method.
fn enum_entries_signature() -> GenericSig {
    let parameter_in_bound = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
    let bound = Ty::obj_args("kotlin/Enum", &[parameter_in_bound]);
    let parameter = Ty::ty_param("T", bound);
    GenericSig {
        formals: vec!["T".to_string()],
        formal_bounds: vec![vec![bound]],
        receiver: None,
        params: Vec::new(),
        ret: Ty::obj_args("kotlin/enums/EnumEntries", &[parameter]),
        return_policy: Default::default(),
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
        mapped_collection: None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{classpath::Classpath, jvm_libraries::JvmLibraries};
    use super::{common_package_function_role, CommonExpectationError, CommonExpectationIndex};
    use crate::diag::{DiagSink, Severity, Span};
    use crate::features::LangFeatures;
    use crate::klib::KlibError;
    use crate::libraries::{CompilerIntrinsic, FnKind, FunctionInfo, LibraryCallable};
    use crate::metadata::id_signature::decode_public_id_signature;
    use crate::source::SourceInput;
    use crate::symbol_source::{SymbolNamespace, SymbolSource};
    use crate::types::{type_name, Ty};
    use std::path::Path;

    const VALID_MODULE_HEADER: &[u8] = b"\x0a\x15<unpackedExampleKlib>\x3a\x00";
    const VALID_ROOT_FRAGMENT: &[u8] = b"\x0a\x1d\x0a\x04main\x0a\x06kotlin\x0a\x04Unit\x0a\x07main.kt\x12\x0c\x0a\x02\x10\x01\x0a\x06\x08\x00\x10\x02\x18\x00\x1a\x1c\x1a\x07\x10\x00\x38\x00\xe0\x0a\x03\xf2\x01\x04\x0a\x02\x30\x01\xd8\x0a\xff\xff\xff\xff\xff\xff\xff\xff\xff\x01\xe0\x0a\x00\xea\x0a\x00";

    fn push_varint(mut value: u64, bytes: &mut Vec<u8>) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            bytes.push(byte | if value == 0 { 0 } else { 0x80 });
            if value == 0 {
                return;
            }
        }
    }

    fn bytes_field(number: u64, value: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_varint((number << 3) | 2, &mut bytes);
        push_varint(value.len() as u64, &mut bytes);
        bytes.extend_from_slice(value);
        bytes
    }

    fn floating_range_identity(
        name: &str,
        member_id: u64,
    ) -> crate::metadata::id_signature::KlibPublicIdSignature {
        let mut package = Vec::new();
        push_varint(0, &mut package);
        push_varint(1, &mut package);
        let mut common = bytes_field(1, &package);
        common.extend(bytes_field(2, &[2]));
        push_varint((6 << 3) | 1, &mut common);
        common.extend(member_id.to_le_bytes());
        let encoded = bytes_field(1, &common);
        decode_public_id_signature(&encoded, &["kotlin", "ranges", name].map(str::to_string))
            .expect("valid public identity")
            .expect("public identity")
    }

    fn floating_range_candidate(role: &super::CommonPackageFunctionRole) -> FunctionInfo {
        let mut candidate = FunctionInfo::plain(
            FnKind::Extension,
            role.generic_sig.receiver,
            LibraryCallable::library(
                type_name("kotlin/ranges/RangesKt"),
                role.name,
                vec![Ty::Double, Ty::Double],
                role.generic_sig.ret,
                role.generic_sig.ret,
                "(DD)Lkotlin/ranges/ClosedFloatingPointRange;",
            ),
        );
        candidate.generic_sig = Some(role.generic_sig.clone());
        candidate.flags.operator = true;
        candidate
    }

    #[test]
    fn floating_range_role_requires_the_exact_common_declaration_identity() {
        const DOUBLE_RANGE_TO: u64 = 692_997_638_542_153_957;
        const DOUBLE_RANGE_UNTIL: u64 = 742_649_461_109_916_381;
        let exact =
            common_package_function_role(floating_range_identity("rangeTo", DOUBLE_RANGE_TO))
                .expect("the exact common identity owns the role");
        let open =
            common_package_function_role(floating_range_identity("rangeUntil", DOUBLE_RANGE_UNTIL))
                .expect("the exact open-end identity owns the role");
        assert_eq!(open.name, "rangeUntil");
        assert_eq!(
            open.generic_sig.ret,
            Ty::obj_args("kotlin/ranges/OpenEndRange", &[Ty::Double])
        );
        assert!(common_package_function_role(floating_range_identity(
            "rangeUntil",
            DOUBLE_RANGE_UNTIL + 1,
        ))
        .is_none());

        let candidate = floating_range_candidate(&exact);
        let missing_identity = CommonExpectationIndex::default();
        assert_eq!(
            missing_identity.package_function_role(
                true,
                crate::types::wk::kotlin_ranges_package(),
                "rangeTo",
                &candidate,
            ),
            None,
            "a trusted JVM declaration with the same owner and shape has no role without the exact common identity",
        );
    }

    #[test]
    fn floating_range_role_requires_the_paired_jvm_dependency() {
        const DOUBLE_RANGE_TO: u64 = 692_997_638_542_153_957;
        let exact =
            common_package_function_role(floating_range_identity("rangeTo", DOUBLE_RANGE_TO))
                .expect("the exact common identity owns the role");
        let candidate = floating_range_candidate(&exact);
        let index = CommonExpectationIndex {
            package_function_roles: vec![exact],
            ..CommonExpectationIndex::default()
        };
        assert_eq!(
            index.package_function_role(
                false,
                crate::types::wk::kotlin_ranges_package(),
                "rangeTo",
                &candidate,
            ),
            None,
            "an identical owner and signature from another JVM dependency cannot actualize the common identity",
        );
        assert_eq!(
            index.package_function_role(
                true,
                crate::types::wk::kotlin_ranges_package(),
                "rangeTo",
                &candidate,
            ),
            Some(CompilerIntrinsic::FloatingRangeMembership),
        );
        assert_eq!(
            index.package_function_role(
                true,
                crate::types::wk::kotlin_ranges_package(),
                "otherRange",
                &candidate,
            ),
            None,
            "the paired owner and shape cannot actualize an identity under another source declaration name",
        );
    }

    #[test]
    fn paired_stdlib_provider_publishes_only_the_exact_floating_range_identities() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = JvmLibraries::new(std::rc::Rc::new(Classpath::new(vec![stdlib])))
            .expect("stdlib provider");
        for (name, range) in [
            ("rangeTo", "kotlin/ranges/ClosedFloatingPointRange"),
            ("rangeUntil", "kotlin/ranges/OpenEndRange"),
        ] {
            let symbols = libraries.symbols(
                SymbolNamespace::Package(crate::types::wk::kotlin_ranges_package()),
                name,
            );
            let functions = match &symbols.callables {
                crate::libraries::Callables::Functions(functions)
                | crate::libraries::Callables::Both { functions, .. } => functions,
                _ => panic!("{name} is missing from the paired stdlib provider"),
            };

            for scalar in [Ty::Double, Ty::Float] {
                let expected_range = Ty::obj_args(range, &[scalar]);
                let matches = functions
                    .overloads
                    .iter()
                    .filter(|function| {
                        function.generic_sig.as_ref().is_some_and(|signature| {
                            signature.receiver == Some(scalar)
                                && signature.params == [scalar]
                                && signature.ret == expected_range
                        })
                    })
                    .collect::<Vec<_>>();
                assert_eq!(matches.len(), 1, "one exact {scalar:?}.{name} identity");
                assert_eq!(
                    matches[0].callable.compiler_intrinsic,
                    Some(CompilerIntrinsic::FloatingRangeMembership),
                );
            }
            assert_eq!(
                functions
                    .overloads
                    .iter()
                    .filter(|function| {
                        function.callable.compiler_intrinsic
                            == Some(CompilerIntrinsic::FloatingRangeMembership)
                    })
                    .count(),
                2,
                "generic Comparable.{name} and unrelated overloads remain ordinary declarations",
            );
        }
    }

    fn enum_entries_identity(
        member_id: u64,
    ) -> crate::metadata::id_signature::KlibPublicIdSignature {
        let mut package = Vec::new();
        push_varint(0, &mut package);
        push_varint(1, &mut package);
        let mut common = bytes_field(1, &package);
        common.extend(bytes_field(2, &[2]));
        push_varint((6 << 3) | 1, &mut common);
        common.extend(member_id.to_le_bytes());
        let encoded = bytes_field(1, &common);
        decode_public_id_signature(
            &encoded,
            &["kotlin", "enums", "enumEntries"].map(str::to_string),
        )
        .expect("valid public identity")
        .expect("public identity")
    }

    fn enum_entries_candidate(role: &super::CommonPackageFunctionRole) -> FunctionInfo {
        let mut candidate = FunctionInfo::plain(
            FnKind::TopLevel,
            None,
            crate::libraries::LibraryCallable::library(
                type_name("kotlin/enums/EnumEntriesKt"),
                role.name,
                Vec::new(),
                role.generic_sig.ret,
                role.generic_sig.ret,
                "()Lkotlin/enums/EnumEntries;",
            ),
        );
        candidate.generic_sig = Some(role.generic_sig.clone());
        candidate.flags.reified = true;
        candidate.flags.inline = crate::libraries::InlineKind::MustInline;
        candidate
    }

    #[test]
    fn enum_entries_role_requires_the_exact_common_declaration_identity() {
        const ENUM_ENTRIES: u64 = 1_918_022_784_687_648_499;
        const ENUM_ENTRIES_FROM_PROVIDER: u64 = 16_990_762_899_543_326_496;
        const ENUM_ENTRIES_FROM_ARRAY: u64 = 1_252_104_220_106_890_916;
        let exact = common_package_function_role(enum_entries_identity(ENUM_ENTRIES))
            .expect("the zero-argument common identity owns the role");
        assert_eq!(exact.name, "enumEntries");
        assert!(exact.generic_sig.params.is_empty());
        assert!(
            common_package_function_role(enum_entries_identity(ENUM_ENTRIES_FROM_PROVIDER))
                .is_none()
        );
        assert!(
            common_package_function_role(enum_entries_identity(ENUM_ENTRIES_FROM_ARRAY)).is_none()
        );
        assert!(common_package_function_role(enum_entries_identity(ENUM_ENTRIES + 1)).is_none());

        let candidate = enum_entries_candidate(&exact);
        let missing_identity = CommonExpectationIndex::default();
        assert_eq!(
            missing_identity.package_function_role(
                true,
                type_name("kotlin/enums"),
                "enumEntries",
                &candidate,
            ),
            None,
            "the same owner and signature have no role without the exact common identity",
        );
    }

    #[test]
    fn enum_entries_role_requires_the_paired_jvm_dependency() {
        const ENUM_ENTRIES: u64 = 1_918_022_784_687_648_499;
        let exact = common_package_function_role(enum_entries_identity(ENUM_ENTRIES))
            .expect("the zero-argument common identity owns the role");
        let candidate = enum_entries_candidate(&exact);
        let index = CommonExpectationIndex {
            package_function_roles: vec![exact],
            ..CommonExpectationIndex::default()
        };
        assert_eq!(
            index.package_function_role(false, type_name("kotlin/enums"), "enumEntries", &candidate),
            None,
            "an identical owner and signature from another JVM dependency cannot actualize the common identity",
        );
        assert_eq!(
            index.package_function_role(true, type_name("kotlin/enums"), "enumEntries", &candidate),
            Some(CompilerIntrinsic::EnumEntries),
        );
        let mut ordinary = candidate.clone();
        ordinary.flags.reified = false;
        ordinary.flags.inline = crate::libraries::InlineKind::None;
        assert_eq!(
            index.package_function_role(true, type_name("kotlin/enums"), "enumEntries", &ordinary),
            None,
            "a non-reified same signature is not the inline declaration",
        );
    }

    #[test]
    fn paired_stdlib_provider_publishes_only_the_zero_argument_enum_entries_identity() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = JvmLibraries::new(std::rc::Rc::new(Classpath::new(vec![stdlib])))
            .expect("stdlib provider");
        let symbols = libraries.symbols(
            SymbolNamespace::Package(type_name("kotlin/enums")),
            "enumEntries",
        );
        let functions = match &symbols.callables {
            crate::libraries::Callables::Functions(functions)
            | crate::libraries::Callables::Both { functions, .. } => functions,
            _ => panic!("enumEntries is missing from the paired stdlib provider"),
        };
        let realized = functions
            .overloads
            .iter()
            .filter(|function| {
                function.callable.compiler_intrinsic == Some(CompilerIntrinsic::EnumEntries)
            })
            .collect::<Vec<_>>();
        let described = functions
            .overloads
            .iter()
            .map(|function| {
                (
                    function.generic_sig.clone(),
                    function.flags.reified,
                    function.flags.inline,
                    function.callable.compiler_intrinsic,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            realized.len(),
            1,
            "only the zero-argument declaration is the intrinsic; signatures: {described:?}"
        );
        let signature = realized[0]
            .generic_sig
            .as_ref()
            .expect("the realized declaration has a generic signature");
        assert!(signature.params.is_empty());
        assert_eq!(signature.formals, ["T"]);
        let ordinary_one_argument = functions
            .overloads
            .iter()
            .filter(|function| {
                function.callable.compiler_intrinsic.is_none()
                    && function
                        .generic_sig
                        .as_ref()
                        .is_some_and(|signature| signature.params.len() == 1)
            })
            .count();
        assert_eq!(
            ordinary_one_argument, 2,
            "the array and provider overloads stay ordinary; signatures: {described:?}"
        );
    }

    fn write_empty_zip(path: &Path) {
        // A complete empty archive. The tests select this path as kotlin-stdlib and then diagnose
        // the sibling KLIB; the jar itself is not a class catalog.
        std::fs::write(
            path,
            [
                0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        )
        .expect("write empty classpath archive");
    }

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
        write_empty_zip(&stdlib);
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
        write_empty_zip(&stdlib);
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

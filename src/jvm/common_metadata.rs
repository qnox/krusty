//! Common optional-expectation annotations absent from the JVM stdlib classfiles.
//!
//! Kotlin ships the common `expect` headers in the distribution KLIB next to `kotlin-stdlib.jar`.
//! Only annotation classifiers whose metadata carries `IS_EXPECT_CLASS` are imported here; platform
//! declarations in the same archive never enter the JVM symbol source.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::libraries::{
    CallSig, ClassifierInheritance, LibraryMember, LibraryType, ParamList, TypeKind,
};
use crate::types::{type_name, Ty, TypeName, TypeNameList, TypeParameters};

#[derive(Default)]
pub(super) struct CommonExpectationIndex {
    classifiers: HashMap<TypeName, Arc<LibraryType>>,
}

impl CommonExpectationIndex {
    pub(super) fn load(path: Option<PathBuf>) -> Arc<Self> {
        type SharedIndex = Arc<OnceLock<Arc<CommonExpectationIndex>>>;
        static CACHE: OnceLock<Mutex<HashMap<PathBuf, SharedIndex>>> = OnceLock::new();
        let Some(path) = path else {
            return Arc::new(Self::default());
        };
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let shared = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(path.clone())
            .or_insert_with(|| Arc::new(OnceLock::new()))
            .clone();
        shared.get_or_init(|| Arc::new(Self::read(&path))).clone()
    }

    pub(super) fn classifier(&self, internal: TypeName) -> Option<Arc<LibraryType>> {
        self.classifiers.get(&internal).cloned()
    }

    pub(super) fn contains(&self, internal: TypeName) -> bool {
        self.classifiers.contains_key(&internal)
    }

    fn read(path: &Path) -> Self {
        let Ok(file) = std::fs::File::open(path) else {
            return Self::default();
        };
        let Ok(mut archive) = zip::ZipArchive::new(file) else {
            return Self::default();
        };
        let mut classifiers = HashMap::new();
        for index in 0..archive.len() {
            let mut bytes = Vec::new();
            {
                let Ok(mut entry) = archive.by_index(index) else {
                    continue;
                };
                let name = entry.name();
                if !name.starts_with("default/linkdata/package_") || !name.ends_with(".knm") {
                    continue;
                }
                if entry.read_to_end(&mut bytes).is_err() {
                    continue;
                }
            }
            for (internal, declaration) in super::metadata::parse_package_fragment(&bytes).classes {
                if declaration.kind != TypeKind::Annotation || !declaration.is_expect {
                    continue;
                }
                let identity = type_name(&internal);
                classifiers
                    .entry(identity)
                    .or_insert_with(|| Arc::new(annotation_type(declaration)));
            }
        }
        Self { classifiers }
    }
}

fn annotation_type(declaration: super::metadata::BuiltinClass) -> LibraryType {
    let bounds = super::classpath::builtin_bounds(&declaration.type_params, &HashMap::new());
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
                    .map(|bound| super::classpath::builtin_ty(bound, &bounds))
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
        .map(|supertype| super::classpath::builtin_ty(supertype, &bounds))
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
            .map(|parameter| super::classpath::builtin_ty(parameter, &bounds))
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
        declared_callables: HashMap::new(),
        declared_callable_order: Vec::new(),
        members: Vec::new(),
        companion: Vec::new(),
        constants: HashMap::new(),
        sam_eligible: false,
        callable_signature: None,
        callable_signatures: Vec::new(),
        companion_object: None,
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
        retention: Some("SOURCE".to_string()),
        annotation_targets: None,
    }
}

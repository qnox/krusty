//! `@OptionalExpectation` annotation classes published by JVM dependencies.
//!
//! A multiplatform library compiled for the JVM keeps each optional annotation that has no JVM
//! actual (`kotlin.js.JsStatic`, `kotlin.native.CName`, …) in the optional-annotation section of its
//! `META-INF/<module>.kotlin_module` rather than as a class file. kotlinc's
//! `OptionalAnnotationClassesProvider` reads that section from every binary classpath root and
//! publishes the classes below the class-file provider, so a real class of the same identity always
//! wins. The checker then restricts their use to common sources.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::libraries::TypeKind;
use crate::libraries::{CallSig, ClassifierInheritance, LibraryMember, LibraryType, ParamList};
use crate::metadata::semantic;
use crate::types::{type_name, Ty, TypeName, TypeNameList, TypeParameters};

/// One optional annotation class decoded from a classpath entry's module file.
#[derive(Clone)]
pub(super) struct OptionalAnnotationClass {
    identity: TypeName,
    declaration: Arc<semantic::KotlinClass>,
}

/// Decode the optional annotation classes of one `.kotlin_module` file. kotlinc's
/// `isOptionalAnnotationClass` is the membership rule: an `expect` annotation class annotated with
/// `kotlin.OptionalExpectation`.
pub(super) fn module_optional_annotations(
    bytes: &[u8],
) -> Result<Vec<OptionalAnnotationClass>, semantic::PackageFragmentDecodeError> {
    let package = semantic::parse_module_optional_annotations(bytes)?;
    let marker = type_name("kotlin/OptionalExpectation");
    Ok(package
        .classes
        .into_iter()
        .filter(|(_, declaration)| {
            declaration.kind == TypeKind::Annotation
                && declaration.is_expect
                && declaration
                    .annotations
                    .iter()
                    .any(|annotation| annotation.identity == marker)
        })
        .map(|(internal, declaration)| OptionalAnnotationClass {
            identity: type_name(&internal),
            declaration: Arc::new(declaration),
        })
        .collect())
}

/// The optional annotation classes of a whole classpath, by identity.
#[derive(Default)]
pub(super) struct OptionalAnnotationIndex {
    classifiers: HashMap<TypeName, Arc<LibraryType>>,
    /// Every package enclosing one of [`Self::classifiers`]. A JVM classpath may hold no class
    /// file in such a package (`kotlin.native`), yet a qualified reference walks through it.
    packages: HashSet<TypeName>,
}

impl OptionalAnnotationIndex {
    /// Every optional annotation class `classpath` publishes. An unreadable module file fails
    /// platform initialization rather than hiding the classes it declares.
    pub(super) fn load(
        classpath: &super::classpath::Classpath,
    ) -> Result<Self, crate::libraries::PlatformInitializationError> {
        classpath
            .optional_annotation_classes()
            .map(Self::new)
            .map_err(|message| crate::libraries::PlatformInitializationError { message })
    }

    /// `classes` in classpath order. kotlinc keys its provider map by class id while walking the
    /// loaded modules in that order, so a later module's class replaces an earlier one.
    pub(super) fn new(classes: impl IntoIterator<Item = OptionalAnnotationClass>) -> Self {
        let mut classifiers = HashMap::new();
        let mut packages = HashSet::new();
        for class in classes {
            let mut package = class.identity.parent();
            while let Some(current) = package {
                packages.insert(current);
                package = current.parent();
            }
            classifiers.insert(
                class.identity,
                Arc::new(annotation_type(&class.declaration)),
            );
        }
        Self {
            classifiers,
            packages,
        }
    }

    /// Whether `name` is a package directly inside `parent` that encloses an optional annotation.
    pub(super) fn has_package(&self, parent: TypeName, name: &str) -> bool {
        crate::types::existing_type_name_child(parent, name)
            .is_some_and(|package| self.packages.contains(&package))
    }

    pub(super) fn classifier(&self, internal: TypeName) -> Option<Arc<LibraryType>> {
        self.classifiers.get(&internal).cloned()
    }

    pub(super) fn contains(&self, internal: TypeName) -> bool {
        self.classifiers.contains_key(&internal)
    }
}

fn annotation_type(declaration: &semantic::KotlinClass) -> LibraryType {
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
    for constructor in &declaration.constructors {
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
            names: constructor.param_names.clone(),
            defaults: constructor.param_defaults.clone(),
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
        annotations: Vec::new(),
        retention: Some(declared_retention(declaration).to_string()),
        annotation_targets: Some(declared_targets(declaration)),
        mapped_collection: None,
        annotation_element_defaults: Vec::new(),
    }
}

/// The class's `@kotlin.annotation.Retention`, as a classpath retention name. Kotlin's default is
/// `RUNTIME`; `BINARY` is the class-file `CLASS` policy.
fn declared_retention(declaration: &semantic::KotlinClass) -> &'static str {
    let retention = type_name("kotlin/annotation/Retention");
    let entry = declaration
        .annotations
        .iter()
        .find(|annotation| annotation.identity == retention)
        .and_then(|annotation| annotation.argument("value"));
    match entry {
        Some(semantic::AnnotationArgument::Enum { entry, .. }) if entry == "SOURCE" => "SOURCE",
        Some(semantic::AnnotationArgument::Enum { entry, .. }) if entry == "BINARY" => "CLASS",
        _ => "RUNTIME",
    }
}

/// Where an unprefixed application may land, from the class's `@kotlin.annotation.Target`; a class
/// that declares none is applicable everywhere.
fn declared_targets(declaration: &semantic::KotlinClass) -> crate::types::AnnotationTargets {
    let target = type_name("kotlin/annotation/Target");
    let Some(allowed) = declaration
        .annotations
        .iter()
        .find(|annotation| annotation.identity == target)
        .and_then(|annotation| annotation.argument("allowedTargets"))
    else {
        return crate::types::AnnotationTargets::DEFAULT;
    };
    let entries = match allowed {
        semantic::AnnotationArgument::Array(elements) => elements.as_slice(),
        single => std::slice::from_ref(single),
    };
    crate::types::AnnotationTargets::kotlin(entries.iter().filter_map(|element| match element {
        semantic::AnnotationArgument::Enum { class, entry } => {
            crate::types::KotlinTarget::of_entry(*class, entry)
        }
        _ => None,
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::jvm::classpath::Classpath;
    use crate::types::{type_name, AnnotationTargets, KotlinTarget};

    /// The stdlib publishes its JS- and Native-only optional annotations through
    /// `META-INF/kotlin-stdlib.kotlin_module`, with their declared retention and targets.
    #[test]
    fn the_stdlib_module_publishes_its_optional_annotations() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let classes = Classpath::new(vec![stdlib])
            .optional_annotation_classes()
            .expect("the stdlib module decodes");
        let identities = classes
            .iter()
            .map(|class| class.identity)
            .collect::<HashSet<_>>();
        let expected = [
            "kotlin/js/JsName",
            "kotlin/js/JsSymbol",
            "kotlin/js/JsFileName",
            "kotlin/js/JsExport",
            "kotlin/js/JsExport$Ignore",
            "kotlin/js/JsExport$Default",
            "kotlin/js/JsStatic",
            "kotlin/js/JsNoRuntime",
            "kotlin/native/CName",
            "kotlin/native/FreezingIsDeprecated",
            "kotlin/native/ObjCName",
            "kotlin/native/ObjCEnum",
            "kotlin/native/HidesFromObjC",
            "kotlin/native/HiddenFromObjC",
            "kotlin/native/RefinesInSwift",
            "kotlin/native/ShouldRefineInSwift",
            "kotlin/native/concurrent/ThreadLocal",
            "kotlin/native/concurrent/SharedImmutable",
        ]
        .into_iter()
        .map(type_name)
        .collect::<HashSet<_>>();
        assert_eq!((classes.len(), identities), (expected.len(), expected));

        let index = super::OptionalAnnotationIndex::new(classes);
        let js_static = index
            .classifier(type_name("kotlin/js/JsStatic"))
            .expect("JsStatic is an optional annotation class");
        assert_eq!(
            (js_static.retention.as_deref(), js_static.annotation_targets),
            (
                Some("CLASS"),
                Some(AnnotationTargets::kotlin([
                    KotlinTarget::Function,
                    KotlinTarget::Property,
                    KotlinTarget::PropertyGetter,
                    KotlinTarget::PropertySetter,
                ]))
            )
        );
    }
}

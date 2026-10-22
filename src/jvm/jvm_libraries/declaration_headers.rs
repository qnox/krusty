//! The declaration headers of classpath classifiers: a Kotlin class's from its metadata, a Kotlin
//! builtin's from `.kotlin_builtins`, and a Java class's from its class file, in Kotlin terms.

use super::*;
use crate::jvm::classreader::ClassInfo;
use crate::jvm::jvm_class_map;
use crate::libraries::{
    ClassifierAccess, DeclarationHeader, DeclarationLanguage, HeaderSupertype, HeaderTypeParameter,
    TypeKind,
};
use crate::metadata::semantic::KotlinModality;
use crate::types::{AnnotationValue, KotlinTarget, ResolvedAnnotation, TyKind, TypeVariance};

const ACC_PUBLIC: u16 = 0x0001;
const ACC_PRIVATE: u16 = 0x0002;
const ACC_PROTECTED: u16 = 0x0004;
const ACC_STATIC: u16 = 0x0008;
const ACC_FINAL: u16 = 0x0010;
const ACC_INTERFACE: u16 = 0x0200;
const ACC_ABSTRACT: u16 = 0x0400;
const ACC_ANNOTATION: u16 = 0x2000;
const ACC_ENUM: u16 = 0x4000;

impl JvmLibraries {
    /// The header of `classifier`: a class file with Kotlin class metadata, else a Kotlin builtin,
    /// else a Java class file. A Kotlin builtin name never reads the JVM class it is mapped to.
    pub(super) fn classpath_declaration_header(
        &self,
        classifier: TypeName,
    ) -> Option<DeclarationHeader> {
        // A function classifier is declared by its JVM runtime interface.
        if let Some(function) = crate::jvm::function_classifiers::classifier(classifier) {
            if function.is_reflective() || function.is_suspend() {
                return None;
            }
            let runtime = crate::types::type_name_child(
                type_name("kotlin/jvm/functions"),
                &format!("Function{}", function.arity()),
            );
            let class = self.cp.find_name(runtime)?;
            return class
                .meta
                .class_kind
                .is_some()
                .then(|| kotlin_header(&class));
        }
        let class = self.cp.find_name(classifier);
        if let Some(class) = class.as_deref() {
            if class.meta.class_kind.is_some() {
                return Some(kotlin_header(class));
            }
        }
        if let Some(builtin) = self.builtin_header(classifier) {
            return Some(builtin);
        }
        let class = class.filter(|class| class.this_class == classifier)?;
        Some(self.java_header(&class))
    }

    fn builtin_header(&self, classifier: TypeName) -> Option<DeclarationHeader> {
        let canonical = self.cp.builtin_classifier_name(classifier)?;
        let (kind, access, _, flags) = self.cp.builtin_classifier_shape_name(canonical)?;
        let signature = self.cp.builtin_class_signature_name(canonical)?;
        let variances = self.cp.builtin_class_variances_name(canonical)?;
        let mut supertypes: Vec<HeaderSupertype> = signature
            .supertypes
            .iter()
            .map(|&ty| HeaderSupertype { ty, alias: None })
            .collect();
        // The JVM class a builtin is realized as contributes `java.io.Serializable`.
        if jvm_class_map::mapped_builtin_is_java_serializable(canonical) {
            supertypes.push(HeaderSupertype {
                ty: Ty::obj("java/io/Serializable"),
                alias: None,
            });
        }
        Some(DeclarationHeader {
            language: DeclarationLanguage::Kotlin,
            kind,
            access,
            modality: modality(flags, false),
            is_data: false,
            is_fun: false,
            is_inner: false,
            is_value: false,
            type_parameters: type_parameters(signature.formals, signature.formal_bounds, variances),
            supertypes,
            annotations: Vec::new(),
        })
    }

    fn java_header(&self, class: &ClassInfo) -> DeclarationHeader {
        let kind = if class.access & ACC_ANNOTATION != 0 {
            TypeKind::Annotation
        } else if class.access & ACC_INTERFACE != 0 {
            TypeKind::Interface
        } else if class.access & ACC_ENUM != 0 {
            TypeKind::Enum
        } else {
            TypeKind::Class
        };
        // A nested class states its own access in its `InnerClasses` entry.
        let nested = class
            .inner_classes
            .iter()
            .find(|entry| entry.outer.is_some() && class.this_class.matches(&entry.inner));
        let flags = nested.map_or(class.access, |entry| entry.access);
        let access = if flags & ACC_PUBLIC != 0 {
            ClassifierAccess::Public
        } else if flags & ACC_PROTECTED != 0 {
            ClassifierAccess::Protected
        } else if flags & ACC_PRIVATE != 0 {
            ClassifierAccess::Private
        } else {
            ClassifierAccess::PackagePrivate
        };
        let (formals, bounds, declared) = match class.signature.as_deref() {
            Some(signature) => parse_class_gsig(signature).unwrap_or_default(),
            None => (
                Vec::new(),
                Vec::new(),
                class
                    .super_class
                    .into_iter()
                    .chain(class.interfaces.iter())
                    .map(Ty::obj_name)
                    .collect(),
            ),
        };
        let bounds = bounds
            .into_iter()
            .map(|bounds| match bounds.is_empty() {
                true => vec![Ty::obj("kotlin/Any")],
                false => bounds.into_iter().map(kotlin_view).collect(),
            })
            .collect();
        let variances = vec![TypeVariance::Invariant; formals.len()];
        DeclarationHeader {
            language: DeclarationLanguage::Java,
            kind,
            access,
            modality: modality(class.access, false),
            is_data: false,
            is_fun: false,
            is_inner: nested.is_some_and(|entry| entry.access & ACC_STATIC == 0)
                && kind == TypeKind::Class,
            is_value: false,
            type_parameters: type_parameters(formals, bounds, variances),
            supertypes: java_supertypes(declared),
            annotations: class.annotations.iter().map(kotlin_annotation).collect(),
        }
    }
}

fn kotlin_header(class: &ClassInfo) -> DeclarationHeader {
    let meta = &class.meta;
    let parameters = &meta.class_type_parameters;
    DeclarationHeader {
        language: DeclarationLanguage::Kotlin,
        kind: meta.class_kind.unwrap_or(TypeKind::Class),
        access: meta
            .class_visibility
            .map_or(ClassifierAccess::Public, ClassifierAccess::from),
        modality: modality(class.access, !meta.sealed_subclasses.is_empty()),
        is_data: meta.is_data,
        is_fun: meta.is_fun_interface,
        is_inner: meta.is_inner,
        is_value: meta.inline.is_some(),
        type_parameters: type_parameters(
            parameters.type_params.clone(),
            parameters.type_param_bounds.clone(),
            parameters.type_param_variances.clone(),
        ),
        supertypes: meta
            .class_supertypes
            .iter()
            .zip(&meta.class_supertype_aliases)
            .map(|(&ty, &alias)| HeaderSupertype { ty, alias })
            .collect(),
        // The compiler writes `Metadata` and a Java mirror of each Kotlin meta-annotation; neither
        // is part of the declaration.
        annotations: class
            .annotations
            .iter()
            .filter(|annotation| {
                ![
                    "kotlin/Metadata",
                    "java/lang/annotation/Documented",
                    "java/lang/annotation/Retention",
                    "java/lang/annotation/Target",
                ]
                .into_iter()
                .any(|compiler_written| annotation.annotation == type_name(compiler_written))
            })
            .cloned()
            .collect(),
    }
}

fn modality(flags: u16, sealed: bool) -> KotlinModality {
    if sealed {
        KotlinModality::Sealed
    } else if flags & ACC_ABSTRACT != 0 {
        KotlinModality::Abstract
    } else if flags & ACC_FINAL != 0 {
        KotlinModality::Final
    } else {
        KotlinModality::Open
    }
}

fn type_parameters(
    names: Vec<String>,
    bounds: Vec<Vec<Ty>>,
    variances: Vec<TypeVariance>,
) -> Vec<HeaderTypeParameter> {
    names
        .into_iter()
        .zip(bounds)
        .zip(variances)
        .map(|((name, bounds), variance)| HeaderTypeParameter {
            name,
            variance,
            reified: false,
            bounds,
        })
        .collect()
}

/// A Java class's supertypes in Kotlin terms: `Object` left out, a mapped class as its Kotlin
/// classifier, and a mapped collection interface as its mutable Kotlin form, after the others.
fn java_supertypes(declared: Vec<Ty>) -> Vec<HeaderSupertype> {
    let mut supertypes = Vec::new();
    let mut collections = Vec::new();
    for ty in declared {
        let TyKind::Obj(name, arguments) = ty.kind() else {
            continue;
        };
        // A generic signature already names a mapped class by its Kotlin classifier; the mapping
        // starts from the JVM class either way.
        let name = jvm_class_map::to_jvm_type_name(name);
        if name == type_name("java/lang/Object") {
            continue;
        }
        let arguments: Vec<Ty> = arguments
            .iter()
            .map(|&argument| kotlin_view(argument))
            .collect();
        match jvm_class_map::jvm_collection_to_kotlin_mutable_type_name(name) {
            Some(mutable) => collections.push(Ty::obj_args_name(mutable, &arguments)),
            None => supertypes.push(Ty::obj_args_name(
                jvm_class_map::to_kotlin_type_name(name),
                &arguments,
            )),
        }
    }
    supertypes
        .into_iter()
        .chain(collections)
        .map(|ty| HeaderSupertype { ty, alias: None })
        .collect()
}

/// A type a Java signature names, with each mapped class as its Kotlin classifier.
fn kotlin_view(ty: Ty) -> Ty {
    match ty.kind() {
        TyKind::Obj(name, arguments) => {
            let arguments: Vec<Ty> = arguments
                .iter()
                .map(|&argument| kotlin_view(argument))
                .collect();
            Ty::obj_args_name(jvm_class_map::to_kotlin_type_name(name), &arguments)
        }
        TyKind::PlatformNullable(inner) | TyKind::Nullable(inner) => kotlin_view(inner),
        TyKind::InProjection(inner) => Ty::in_projection(kotlin_view(inner)),
        TyKind::OutProjection(inner) => Ty::out_projection(kotlin_view(inner)),
        TyKind::StarProjection(inner) => Ty::star_projection(kotlin_view(inner)),
        _ => ty,
    }
}

/// A Java annotation class's meta-annotations as their Kotlin counterparts, as Kotlin reads them.
fn kotlin_annotation(annotation: &ResolvedAnnotation) -> ResolvedAnnotation {
    let class = annotation.annotation;
    let (annotation_class, arguments) = if class == type_name("java/lang/annotation/Documented") {
        ("kotlin/annotation/MustBeDocumented", Vec::new())
    } else if class == type_name("java/lang/annotation/Retention") {
        (
            "kotlin/annotation/Retention",
            annotation
                .arguments
                .iter()
                .filter_map(|(_, value)| match value {
                    AnnotationValue::Enum(_, policy) => Some((
                        "value".to_string(),
                        AnnotationValue::Enum(
                            type_name("kotlin/annotation/AnnotationRetention"),
                            match policy.as_str() {
                                "CLASS" => "BINARY",
                                policy => policy,
                            }
                            .to_string(),
                        ),
                    )),
                    _ => None,
                })
                .collect(),
        )
    } else if class == type_name("java/lang/annotation/Target") {
        let mut targets: Vec<KotlinTarget> = Vec::new();
        for (_, value) in &annotation.arguments {
            let AnnotationValue::Array(entries) = value else {
                continue;
            };
            for entry in entries {
                if let AnnotationValue::Enum(_, element) = entry {
                    for &target in KotlinTarget::of_java_element_type(element) {
                        if !targets.contains(&target) {
                            targets.push(target);
                        }
                    }
                }
            }
        }
        let target_class = type_name("kotlin/annotation/AnnotationTarget");
        (
            "kotlin/annotation/Target",
            vec![(
                "allowedTargets".to_string(),
                AnnotationValue::Array(
                    targets
                        .into_iter()
                        .map(|target| {
                            AnnotationValue::Enum(target_class, target.entry_name().to_string())
                        })
                        .collect(),
                ),
            )],
        )
    } else {
        return annotation.clone();
    };
    ResolvedAnnotation {
        annotation: type_name(annotation_class),
        arguments,
        facts: annotation.facts,
        written: None,
    }
}

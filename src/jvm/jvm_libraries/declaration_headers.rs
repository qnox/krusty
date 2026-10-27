//! The declaration headers of classpath classifiers: a Kotlin class's from its metadata, a Kotlin
//! builtin's from `.kotlin_builtins`, and a Java class's from its class file, in Kotlin terms.

use super::*;
use crate::jvm::classreader::ClassInfo;
use crate::jvm::jvm_class_map;
use crate::libraries::{
    ClassifierAccess, DeclarationHeader, DeclarationLanguage, HeaderSignature, HeaderSupertype,
    HeaderTypeParameter, TypeKind,
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
                .then(|| self.kotlin_header(&class));
        }
        let class = self.cp.find_name(classifier);
        if let Some(class) = class.as_deref() {
            // The container the compiler generates for a repeatable annotation is not a
            // declaration Kotlin sees.
            if is_repeatable_container(class) {
                return None;
            }
            if class.meta.class_kind.is_some() {
                return Some(self.kotlin_header(class));
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
            modality: modality(flags),
            is_data: false,
            is_fun: false,
            is_inner: false,
            is_value: false,
            signature: Some(HeaderSignature {
                type_parameters: type_parameters(
                    signature.formals,
                    signature.formal_bounds,
                    variances,
                ),
                supertypes,
            }),
            annotations: Some(Vec::new()),
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
        DeclarationHeader {
            language: DeclarationLanguage::Java,
            kind,
            access,
            modality: modality(class.access),
            is_data: false,
            is_fun: false,
            is_inner: nested.is_some_and(|entry| entry.access & ACC_STATIC == 0)
                && kind == TypeKind::Class,
            is_value: false,
            signature: java_signature(class),
            annotations: Some(class.annotations.iter().map(kotlin_annotation).collect()),
        }
    }
}

impl JvmLibraries {
    fn kotlin_header(&self, class: &ClassInfo) -> DeclarationHeader {
        let meta = &class.meta;
        let parameters = &meta.class_type_parameters;
        DeclarationHeader {
            language: DeclarationLanguage::Kotlin,
            kind: meta.class_kind.unwrap_or(TypeKind::Class),
            access: meta
                .class_visibility
                .map_or(ClassifierAccess::Public, ClassifierAccess::from),
            modality: meta
                .class_modality
                .unwrap_or_else(|| modality(class.access)),
            is_data: meta.is_data,
            is_fun: meta.is_fun_interface,
            is_inner: meta.is_inner,
            is_value: meta.inline.is_some(),
            signature: Some(HeaderSignature {
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
            }),
            annotations: class
                .annotations
                .iter()
                .filter(|annotation| !is_compiler_written(annotation.annotation))
                .map(|annotation| self.applications(annotation))
                .collect::<Option<Vec<_>>>()
                .map(|applications| applications.into_iter().flatten().collect()),
        }
    }

    /// The applications an annotation of a Kotlin class file stands for: the repeated ones a
    /// generated container holds, each on its own, or the annotation itself. `None` for a
    /// container whose value is not an array of annotations, which is no evidence of what the
    /// declaration repeats.
    fn applications(&self, annotation: &ResolvedAnnotation) -> Option<Vec<ResolvedAnnotation>> {
        let container = self
            .cp
            .find_name(annotation.annotation)
            .is_some_and(|class| is_repeatable_container(&class));
        if !container {
            return Some(vec![annotation.clone()]);
        }
        let [(_, AnnotationValue::Array(repeated))] = annotation.arguments.as_slice() else {
            return None;
        };
        repeated
            .iter()
            .map(|value| match value {
                AnnotationValue::Annotation {
                    internal,
                    values,
                    written,
                } => Some(ResolvedAnnotation {
                    annotation: *internal,
                    arguments: values.clone(),
                    facts: Default::default(),
                    written: written.clone(),
                }),
                _ => None,
            })
            .collect()
    }
}

/// The compiler writes `Metadata` and a Java mirror of each Kotlin meta-annotation on a Kotlin
/// class; none is part of the declaration.
fn is_compiler_written(annotation: TypeName) -> bool {
    [
        "kotlin/Metadata",
        "java/lang/annotation/Documented",
        "java/lang/annotation/Repeatable",
        "java/lang/annotation/Retention",
        "java/lang/annotation/Target",
    ]
    .into_iter()
    .any(|compiler_written| annotation == type_name(compiler_written))
}

/// Whether `class` is the container the compiler generates for a repeatable Kotlin annotation.
fn is_repeatable_container(class: &ClassInfo) -> bool {
    class.annotations.iter().any(|annotation| {
        annotation.annotation == type_name("kotlin/jvm/internal/RepeatableContainer")
    })
}

/// The modality JVM access flags state, for a declaration without Kotlin metadata.
fn modality(flags: u16) -> KotlinModality {
    if flags & ACC_ABSTRACT != 0 {
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

/// A Java class's type parameters and supertypes: from its generic signature when it states one,
/// else its raw superclass and interfaces. `None` for a signature that cannot be read, which is
/// not evidence of a class without type parameters.
fn java_signature(class: &ClassInfo) -> Option<HeaderSignature> {
    let (formals, bounds, declared) = match class.signature.as_deref() {
        Some(signature) => parse_class_gsig(signature)?,
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
    Some(HeaderSignature {
        type_parameters: type_parameters(formals, bounds, variances),
        supertypes: java_supertypes(declared),
    })
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
            None => supertypes.push(Ty::obj_args_name(kotlin_classifier(name), &arguments)),
        }
    }
    supertypes
        .into_iter()
        .chain(collections)
        .map(|ty| HeaderSupertype { ty, alias: None })
        .collect()
}

/// The Kotlin classifier a JVM class realizes: a mapped class's, or the function classifier of a
/// fixed-arity `kotlin.jvm.functions` interface.
fn kotlin_classifier(name: TypeName) -> TypeName {
    let function = name
        .parent()
        .filter(|package| package.matches("kotlin/jvm/functions"))
        .and_then(|_| {
            crate::jvm::function_classifiers::classifier_name_in(
                type_name("kotlin"),
                name.segment_ref(),
            )
        });
    function.unwrap_or_else(|| jvm_class_map::to_kotlin_type_name(name))
}

/// A type a Java signature names, with each mapped class as its Kotlin classifier.
fn kotlin_view(ty: Ty) -> Ty {
    match ty.kind() {
        TyKind::Obj(name, arguments) => {
            let arguments: Vec<Ty> = arguments
                .iter()
                .map(|&argument| kotlin_view(argument))
                .collect();
            Ty::obj_args_name(kotlin_classifier(name), &arguments)
        }
        TyKind::PlatformNullable(inner)
        | TyKind::MutabilityFlexible(inner)
        | TyKind::Nullable(inner) => kotlin_view(inner),
        TyKind::InProjection(inner) => Ty::in_projection(kotlin_view(inner)),
        TyKind::OutProjection(inner) => Ty::out_projection(kotlin_view(inner)),
        TyKind::StarProjection(inner) => Ty::star_projection(kotlin_view(inner)),
        _ => ty,
    }
}

/// A Java class's annotations that Kotlin maps, `@Deprecated` and the meta-annotations, as their
/// Kotlin counterparts, as Kotlin reads them.
fn kotlin_annotation(annotation: &ResolvedAnnotation) -> ResolvedAnnotation {
    let class = annotation.annotation;
    let (annotation_class, arguments) = if class == type_name("java/lang/Deprecated") {
        (
            "kotlin/Deprecated",
            vec![(
                "message".to_string(),
                AnnotationValue::string("Deprecated in Java"),
            )],
        )
    } else if class == type_name("java/lang/annotation/Documented") {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn libraries() -> Option<JvmLibraries> {
        crate::toolchain::stdlib_jar()?;
        JvmLibraries::new(std::rc::Rc::new(
            crate::toolchain::stdlib_and_jdk_classpath(),
        ))
        .ok()
    }

    #[test]
    fn a_java_class_states_its_generic_signature() {
        let Some(libraries) = libraries() else {
            return;
        };
        let header = libraries
            .classpath_declaration_header(type_name("java/util/ArrayList"))
            .expect("java.util.ArrayList");
        let signature = header.signature.expect("a readable signature");
        let names: Vec<&str> = signature
            .type_parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect();
        assert_eq!(names, ["E"]);
    }

    #[test]
    fn an_unreadable_generic_signature_is_not_a_nongeneric_class() {
        let Some(libraries) = libraries() else {
            return;
        };
        let mut class = (*libraries
            .cp
            .find_name(type_name("java/util/ArrayList"))
            .expect("java.util.ArrayList"))
        .clone();
        class.signature = Some("<E:".to_string());
        let header = libraries.java_header(&class);
        assert_eq!(header.signature, None);
        assert_eq!(header.kind, TypeKind::Class);
        assert_eq!(header.access, ClassifierAccess::Public);
    }

    fn repeatable_container(values: Vec<AnnotationValue>) -> ResolvedAnnotation {
        ResolvedAnnotation {
            annotation: type_name("kotlin/internal/RequireKotlin").nested_child("Container"),
            arguments: vec![("value".to_string(), AnnotationValue::Array(values))],
            facts: Default::default(),
            written: None,
        }
    }

    fn require_kotlin(version: &str) -> AnnotationValue {
        AnnotationValue::Annotation {
            internal: type_name("kotlin/internal/RequireKotlin"),
            values: vec![("version".to_string(), AnnotationValue::string(version))],
            written: None,
        }
    }

    #[test]
    fn a_repeatable_container_stands_for_each_application_it_holds() {
        let Some(libraries) = libraries() else {
            return;
        };
        let container = repeatable_container(vec![require_kotlin("1.2"), require_kotlin("1.3")]);
        let applications = libraries
            .applications(&container)
            .expect("a well-formed container");
        let versions: Vec<&[(String, AnnotationValue)]> = applications
            .iter()
            .map(|application| application.arguments.as_slice())
            .collect();
        assert_eq!(
            versions,
            [
                &[("version".to_string(), AnnotationValue::string("1.2"))][..],
                &[("version".to_string(), AnnotationValue::string("1.3"))][..],
            ]
        );
        assert!(applications.iter().all(|application| {
            application.annotation == type_name("kotlin/internal/RequireKotlin")
        }));
    }

    #[test]
    fn a_container_holding_a_value_that_is_not_an_annotation_is_unreadable() {
        let Some(libraries) = libraries() else {
            return;
        };
        let mixed = repeatable_container(vec![require_kotlin("1.2"), AnnotationValue::Int(3)]);
        assert_eq!(libraries.applications(&mixed), None);
        let mut not_an_array = repeatable_container(Vec::new());
        not_an_array.arguments = vec![("value".to_string(), require_kotlin("1.2"))];
        assert_eq!(libraries.applications(&not_an_array), None);
    }
}

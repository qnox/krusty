//! The classifiers of the classpath as KSP shows them to a processor: the header a host asks for
//! by name when a processor first reaches one, from the declaration header krusty's library
//! provider reads, and the name kotlinc's class map gives one across the JVM boundary.

use krusty_plugin_ir::{
    KspClass, KspClassKind, KspClasspath, KspLibraryClass, KspModifier, KspNameMapping, KspOrigin,
    KspType, KspTypeDeclaration,
};

use super::annotations::{short_name, Annotations};
use super::type_model::{class_type, qualified_name, TypeParameterIds, TypeParameterScope};
use crate::frontend::SourceSetAnalysis;
use crate::libraries::{
    ClassifierAccess, DeclarationHeader, DeclarationLanguage, HeaderSupertype, TypeKind,
};
use crate::types::{wk, Ty, TyKind, TypeName};

/// The classpath of an analysis, answering a host's questions about it while a round runs.
pub struct KspLibraries<'a> {
    analysis: &'a SourceSetAnalysis,
}

impl<'a> KspLibraries<'a> {
    pub fn new(analysis: &'a SourceSetAnalysis) -> Self {
        KspLibraries { analysis }
    }
}

impl KspClasspath for KspLibraries<'_> {
    fn class(&self, qualified_name: &str) -> Option<KspLibraryClass> {
        // KSP finds a classifier by name, never the typealias naming one.
        let class = self.analysis.library_classifier(qualified_name)?;
        let header = self.analysis.symbols.libraries.declaration_header(class)?;
        Some(self.library_class(class, &header))
    }

    fn mapped_name(&self, mapping: KspNameMapping, name: &str) -> Option<String> {
        let classifier = self.analysis.library_classifier(name)?;
        let libraries = &self.analysis.symbols.libraries;
        let mapped = match mapping {
            KspNameMapping::KotlinToJava => libraries.platform_class(classifier),
            KspNameMapping::JavaToKotlin => libraries.mapped_classifier(classifier),
        }?;
        Some(qualified_name(mapped))
    }
}

impl KspLibraries<'_> {
    fn library_class(&self, class: TypeName, header: &DeclarationHeader) -> KspLibraryClass {
        let qualified = qualified_name(class);
        let ids = TypeParameterIds::default();
        let no_parameters = Vec::new();
        let parameters = header
            .signature
            .as_ref()
            .map_or(&no_parameters, |signature| &signature.type_parameters);
        let scope = TypeParameterScope::default().with(
            &ids,
            &qualified,
            parameters
                .iter()
                .map(|parameter| (parameter.name.as_str(), parameter.name.as_str())),
        );
        let origin = match header.language {
            DeclarationLanguage::Kotlin => KspOrigin::KotlinLib,
            DeclarationLanguage::Java => KspOrigin::JavaLib,
        };
        // A class file whose generic signature cannot be read has neither query answered.
        let type_parameters = header.signature.as_ref().and_then(|signature| {
            signature
                .type_parameters
                .iter()
                .map(|parameter| {
                    // KSP shows an unbounded Kotlin parameter bounded by `Any?`.
                    let bounds = match parameter.bounds.is_empty() {
                        true => vec![Ty::nullable(Ty::obj_name(wk::any()))],
                        false => parameter.bounds.clone(),
                    };
                    scope.type_parameter(
                        &parameter.name,
                        parameter.variance,
                        parameter.reified,
                        bounds,
                    )
                })
                .collect()
        });
        let annotations = Annotations::new(self.analysis);
        KspLibraryClass {
            class: KspClass {
                simple_name: short_name(class),
                qualified_name: qualified,
                kind: kind(header.kind),
                line: 0,
                annotations: header
                    .annotations
                    .iter()
                    .flatten()
                    .map(|annotation| annotations.ksp_annotation(annotation, false, true, false))
                    .collect(),
                modifiers: modifiers(header, self.converts_lambdas(class, header)),
                type_parameters,
                super_types: header
                    .signature
                    .as_ref()
                    .and_then(|signature| super_types(class, &signature.supertypes, &scope)),
                declarations: Vec::new(),
                declarations_complete: false,
                // A library class's JVM signature is not described yet.
                jvm_signature: None,
            },
            package_name: class.package().replace('/', "."),
            parent: class.nested_owner().map(qualified_name),
            origin,
            annotations_complete: header.annotations.is_some(),
        }
    }
}

impl KspLibraries<'_> {
    /// Whether KSP reports `fun`: a Kotlin `fun interface`, or a Java interface Kotlin converts a
    /// lambda to.
    fn converts_lambdas(&self, class: TypeName, header: &DeclarationHeader) -> bool {
        match header.language {
            DeclarationLanguage::Kotlin => header.is_fun,
            DeclarationLanguage::Java => {
                header.kind == TypeKind::Interface && self.analysis.converts_lambdas_to(class)
            }
        }
    }
}

fn kind(kind: TypeKind) -> KspClassKind {
    match kind {
        TypeKind::Class => KspClassKind::Class,
        TypeKind::Interface => KspClassKind::Interface,
        TypeKind::Annotation => KspClassKind::AnnotationClass,
        TypeKind::Enum => KspClassKind::EnumClass,
        TypeKind::Object => KspClassKind::Object,
    }
}

/// The modifiers KSP reports for a library classifier, which it derives from the declaration's
/// facts. `None` for a declaration whose facts the oracle has not pinned yet (an inner class, or a
/// Java package-private one).
fn modifiers(header: &DeclarationHeader, is_fun: bool) -> Option<Vec<KspModifier>> {
    if header.is_inner {
        return None;
    }
    let mut modifiers = vec![match header.access {
        ClassifierAccess::Public => KspModifier::Public,
        ClassifierAccess::Internal => KspModifier::Internal,
        ClassifierAccess::Protected => KspModifier::Protected,
        ClassifierAccess::Private => KspModifier::Private,
        ClassifierAccess::PackagePrivate => return None,
    }];
    match header.kind {
        // KSP reports a sealed classifier as sealed only, and a value class as `inline`.
        TypeKind::Interface if header.is_sealed() => modifiers.push(KspModifier::Sealed),
        TypeKind::Interface => {
            modifiers.push(KspModifier::Abstract);
            if is_fun {
                modifiers.push(KspModifier::Fun);
            }
        }
        TypeKind::Enum => modifiers.extend([KspModifier::Enum, KspModifier::Final]),
        TypeKind::Annotation | TypeKind::Object => modifiers.push(KspModifier::Final),
        TypeKind::Class if header.is_sealed() => modifiers.push(KspModifier::Sealed),
        TypeKind::Class if header.is_value => {
            modifiers.extend([KspModifier::Final, KspModifier::Inline]);
        }
        TypeKind::Class => {
            if header.is_data {
                modifiers.push(KspModifier::Data);
            }
            modifiers.push(if header.is_abstract() {
                KspModifier::Abstract
            } else if header.is_final() {
                KspModifier::Final
            } else {
                KspModifier::Open
            });
        }
    }
    Some(modifiers)
}

/// The direct supertypes in declaration order, each named as the declaration wrote it, and
/// `kotlin.Any` when a classifier other than `kotlin.Any` declares none. `None` while a supertype
/// is written as a typealias with arguments, which the header does not carry yet.
fn super_types(
    class: TypeName,
    supertypes: &[HeaderSupertype],
    scope: &TypeParameterScope,
) -> Option<Vec<KspType>> {
    let declared = supertypes
        .iter()
        .map(|supertype| match supertype.alias {
            None => scope.ty(supertype.ty),
            Some(alias) => match supertype.ty.kind() {
                TyKind::Obj(_, arguments) if arguments.is_empty() => Some(KspType {
                    declaration: KspTypeDeclaration::TypeAlias {
                        qualified_name: qualified_name(alias),
                        package_name: alias.package().replace('/', "."),
                    },
                    nullable: false,
                    arguments: Vec::new(),
                }),
                _ => None,
            },
        })
        .collect::<Option<Vec<_>>>()?;
    if declared.is_empty() && class != wk::any() {
        return Some(vec![class_type("kotlin.Any")]);
    }
    Some(declared)
}

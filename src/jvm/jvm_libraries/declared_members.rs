//! The members classpath classifiers declare, in Kotlin terms: a Kotlin builtin's from
//! `.kotlin_builtins`, and an annotation class's from its Kotlin metadata or its Java class file.
//! The members of other classifiers are not read yet.

use super::classpath_annotation_classes::{
    is_java_annotation_element, java_annotation_element_type,
};
use super::*;
use crate::jvm::classreader::ClassInfo;
use crate::libraries::{
    DeclaredConstructor, DeclaredMembers, DeclaredParameter, DeclaredProperty, ParamList, TypeKind,
};
use crate::metadata::semantic::KotlinModality;
use crate::types::Visibility;

const ACC_SYNTHETIC: u16 = 0x1000;
const ACC_ANNOTATION: u16 = 0x2000;

impl JvmLibraries {
    /// The members `classifier` declares, read from where its header is: a class file with Kotlin
    /// class metadata, else a Kotlin builtin, else a Java class file.
    pub(super) fn classpath_declared_members(
        &self,
        classifier: TypeName,
    ) -> Option<DeclaredMembers> {
        if crate::jvm::function_classifiers::classifier(classifier).is_some() {
            return None;
        }
        let class = self.cp.find_name(classifier);
        if let Some(class) = class.as_deref() {
            if class.meta.class_kind.is_some() {
                return kotlin_annotation_members(classifier, class);
            }
        }
        if let Some(builtin) = self.cp.builtin_classifier_name(classifier) {
            return self.cp.builtin_declared_members(builtin);
        }
        let class = class.filter(|class| class.this_class == classifier)?;
        java_annotation_members(classifier, &class)
    }
}

/// A Kotlin annotation class's members: a property for each parameter of its primary
/// constructor, the constructor, and its nested classifiers.
fn kotlin_annotation_members(classifier: TypeName, class: &ClassInfo) -> Option<DeclaredMembers> {
    let meta = &class.meta;
    if meta.class_kind != Some(TypeKind::Annotation) || !meta.class_functions.is_empty() {
        return None;
    }
    let constructor = meta
        .constructors
        .iter()
        .find(|constructor| constructor.primary)?;
    let parameters = &constructor.params;
    let properties = meta
        .class_properties
        .iter()
        .map(|property| {
            let index = parameters
                .names
                .iter()
                .position(|name| *name == property.name)?;
            // A property's accessor and the method its property-targeted annotations are written
            // on carry what the property states.
            let annotated = [&property.getter, &property.annotation_method]
                .into_iter()
                .flatten()
                .any(|method| {
                    class
                        .method(&method.name, &method.desc)
                        .is_some_and(|method| !method.annotations.is_empty())
                });
            Some(DeclaredProperty {
                name: property.name.clone(),
                ty: *parameters.types.get(index)?,
                is_var: property.is_var,
                visibility: property.visibility,
                modality: match property.is_final {
                    true => KotlinModality::Final,
                    false => KotlinModality::Open,
                },
                from_java_method: false,
                annotated,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(DeclaredMembers {
        functions: Vec::new(),
        properties,
        constructors: vec![DeclaredConstructor {
            parameters: declared_parameters(parameters)?,
            visibility: parameters.visibility,
        }],
        nested: meta
            .nested_class_names
            .iter()
            .map(|name| crate::types::type_name_nested_child(classifier, name))
            .collect(),
    })
}

/// A Java annotation class's members: a property Kotlin reads off each element, the constructor
/// Kotlin reads off them, whose array `value` is a `vararg`, and its nested classifiers in
/// `InnerClasses` order. A class that also declares a constant is not read yet.
fn java_annotation_members(classifier: TypeName, class: &ClassInfo) -> Option<DeclaredMembers> {
    if class.access & ACC_ANNOTATION == 0 || !class.fields.is_empty() {
        return None;
    }
    let mut properties = Vec::new();
    let mut parameters = Vec::new();
    for method in class.methods.iter() {
        if !is_java_annotation_element(method) {
            continue;
        }
        let ty = java_annotation_element_type(method)?;
        let is_vararg = method.name == "value" && ty.array_elem().is_some();
        parameters.push(DeclaredParameter::new(
            method.name.clone(),
            ty,
            is_vararg,
            method.has_annotation_default,
        )?);
        properties.push(DeclaredProperty {
            name: method.name.clone(),
            ty,
            is_var: false,
            visibility: Visibility::Public,
            modality: KotlinModality::Final,
            from_java_method: true,
            annotated: !method.annotations.is_empty(),
        });
    }
    let nested = class
        .inner_classes
        .iter()
        .filter(|entry| {
            entry.access & ACC_SYNTHETIC == 0
                && entry
                    .outer
                    .as_deref()
                    .is_some_and(|outer| class.this_class.matches(outer))
        })
        .map(|entry| {
            let name = entry.name.as_deref()?;
            Some(crate::types::type_name_nested_child(classifier, name))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(DeclaredMembers {
        functions: Vec::new(),
        properties,
        constructors: vec![DeclaredConstructor {
            parameters,
            visibility: Visibility::Public,
        }],
        nested,
    })
}

/// The parameters of a constructor's metadata parameter list, a `vararg` one as its element.
fn declared_parameters(parameters: &ParamList) -> Option<Vec<DeclaredParameter>> {
    parameters
        .names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            DeclaredParameter::new(
                name.clone(),
                *parameters.types.get(index)?,
                parameters.vararg == Some(index),
                parameters.defaults.as_slice().get(index).copied()?,
            )
        })
        .collect()
}

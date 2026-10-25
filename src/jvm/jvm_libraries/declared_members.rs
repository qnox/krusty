//! The members classpath classifiers declare, in Kotlin terms: a Kotlin builtin's from
//! `.kotlin_builtins`, and an annotation class's from its Kotlin metadata or its Java class file.
//! The members of other classifiers are not read yet.

use super::classpath_annotation_classes::{
    is_java_annotation_element, java_annotation_element_type,
};
use super::*;
use crate::jvm::classreader::{ClassInfo, FieldSig, MethodAnnotations};
use crate::jvm::jvm_class_map;
use crate::libraries::{
    DeclaredConstructor, DeclaredMembers, DeclaredParameter, DeclaredProperty, ParamList, TypeKind,
};
use crate::metadata::semantic::KotlinModality;
use crate::types::{wk, TyKind, Visibility};

const ACC_SYNTHETIC: u16 = 0x1000;
const ACC_ANNOTATION: u16 = 0x2000;
const ACC_PUBLIC_STATIC_FINAL: u16 = 0x0019;

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
        let class = class
            .filter(|class| class.this_class == classifier && class.access & ACC_ANNOTATION != 0)?;
        let constants = class
            .fields
            .iter()
            .filter(|field| field.access & ACC_SYNTHETIC == 0)
            .map(|field| self.java_constant(field))
            .collect::<Option<Vec<_>>>()?;
        let annotated_element = class
            .methods
            .iter()
            .any(|method| is_java_annotation_element(method) && !method.annotations.is_empty());
        let element_annotations = match annotated_element {
            true => self.cp.class_method_annotations(classifier)?,
            false => Vec::new(),
        };
        java_annotation_members(classifier, &class, constants, &element_annotations)
    }

    /// The property Kotlin reads off a constant a Java interface declares, which is public, static
    /// and final. `None` for a generic one, whose type is not described yet.
    fn java_constant(&self, field: &FieldSig) -> Option<DeclaredProperty> {
        if field.access & ACC_PUBLIC_STATIC_FINAL != ACC_PUBLIC_STATIC_FINAL
            || field.signature.is_some()
        {
            return None;
        }
        Some(DeclaredProperty {
            name: field.name.clone(),
            ty: self.java_field_type(declared_desc_to_ty(&field.descriptor))?,
            is_var: false,
            visibility: Visibility::Public,
            modality: KotlinModality::Final,
            from_java_method: false,
            getter_annotations: Vec::new(),
            annotated: field.annotated || field.nullability.is_some(),
        })
    }

    /// The type Kotlin reads a Java field's erased type as, as declared: a mapped class as its
    /// Kotlin classifier and a reference array as `Array<out E>`. `None` for a raw generic class
    /// or a collection interface, whose Kotlin faces are not described yet.
    fn java_field_type(&self, ty: Ty) -> Option<Ty> {
        match ty.kind() {
            TyKind::Obj(name, arguments) if name == wk::array() => {
                let [element] = arguments.as_slice() else {
                    return None;
                };
                Some(Ty::obj_args_name(
                    wk::array(),
                    &[Ty::out_projection(self.java_field_type(*element)?)],
                ))
            }
            TyKind::Obj(name, arguments) if arguments.is_empty() => {
                let generic = self.cp.find_name(name).is_some_and(|class| {
                    class
                        .signature
                        .as_deref()
                        .is_some_and(|s| s.starts_with('<'))
                });
                let collection =
                    jvm_class_map::jvm_collection_to_kotlin_mutable_type_name(name).is_some();
                (!generic && !collection)
                    .then(|| Ty::obj_name(jvm_class_map::to_kotlin_type_name(name)))
            }
            TyKind::Obj(..) => None,
            _ => Some(ty),
        }
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
                getter_annotations: Vec::new(),
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
        static_properties: Vec::new(),
        nested: meta
            .nested_class_names
            .iter()
            .map(|name| crate::types::type_name_nested_child(classifier, name))
            .collect(),
    })
}

/// A Java annotation class's members: a property Kotlin reads off each element, whose getter
/// states what the element's method states (`element_annotations`), the constructor Kotlin reads
/// off them, whose array `value` is a `vararg`, its constants, and its nested classifiers in
/// `InnerClasses` order.
fn java_annotation_members(
    classifier: TypeName,
    class: &ClassInfo,
    constants: Vec<DeclaredProperty>,
    element_annotations: &[MethodAnnotations],
) -> Option<DeclaredMembers> {
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
        let getter_annotations = element_annotations
            .iter()
            .find(|read| read.name == method.name && read.descriptor == method.descriptor)
            .map_or_else(Vec::new, |read| read.annotations.clone());
        // Kotlin reads a Java annotation it maps as its Kotlin counterpart, and a nullability
        // qualifier into the type; neither is described yet.
        let annotated = method.return_nullability.is_some()
            || getter_annotations.len() < method.annotations.len()
            || getter_annotations
                .iter()
                .any(|annotation| kotlin_maps_java_annotation(annotation.annotation));
        properties.push(DeclaredProperty {
            name: method.name.clone(),
            ty,
            is_var: false,
            visibility: Visibility::Public,
            modality: KotlinModality::Final,
            from_java_method: true,
            getter_annotations,
            annotated,
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
        static_properties: constants,
        nested,
    })
}

/// Whether Kotlin reads the Java annotation class `annotation` as a Kotlin one: `Deprecated` and
/// the meta-annotations.
fn kotlin_maps_java_annotation(annotation: TypeName) -> bool {
    annotation == crate::types::type_name("java/lang/Deprecated")
        || annotation
            .parent()
            .is_some_and(|package| package.matches("java/lang/annotation"))
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

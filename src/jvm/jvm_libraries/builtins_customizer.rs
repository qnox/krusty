//! JVM-only additions to Kotlin's builtin classifier model.
//!
//! These declarations are absent from the common `Array` source and are not backend guesses:
//! kotlinc's `JvmBuiltInsCustomizer` adds `Cloneable`/`Serializable` as array supertypes, adds
//! `Serializable` to every mapped builtin whose Java class implements it, and publishes a public,
//! covariant `clone()` declaration on every array classifier. Keeping that transformation
//! here means every consumer sees one ordinary [`LibraryType`]; resolver and lowerer need no JVM or
//! array-specific lookup path.

use crate::libraries::{ClassifierAccess, LibraryMember, LibraryType, Visibility};
use crate::types::{type_name, Ty, TypeName, TypeNameList};

use super::builtin_classifier_shapes::{builtin_library_type, BuiltinGenericShape};

#[derive(Default)]
pub(super) struct JvmBuiltInsCustomizer;

impl JvmBuiltInsCustomizer {
    pub(super) fn classifier_name(&self, package: TypeName, name: &str) -> Option<TypeName> {
        if !package.matches("kotlin") {
            return None;
        }
        let recognized =
            name == "Cloneable" || name == "Array" || Ty::primitive_array_element(name).is_some();
        recognized.then(|| crate::types::type_name_child(package, name))
    }

    pub(super) fn customize(
        &self,
        internal: TypeName,
        base: Option<LibraryType>,
    ) -> Option<LibraryType> {
        let mut classifier = if internal.matches("kotlin/Cloneable") {
            base.unwrap_or_else(Self::cloneable_classifier)
        } else if Ty::obj_name(internal).is_array() {
            base.unwrap_or_else(LibraryType::declaration_header)
        } else {
            base?
        };

        if internal.matches("kotlin/Cloneable") {
            Self::install_cloneable_clone(&mut classifier);
        }
        if Ty::obj_name(internal).is_array() {
            Self::install_array_platform_shape(internal, &mut classifier);
        } else if crate::jvm::jvm_class_map::mapped_builtin_is_java_serializable(internal) {
            Self::install_supertype(&mut classifier, type_name("java/io/Serializable"));
        }
        Some(classifier)
    }

    fn install_supertype(classifier: &mut LibraryType, name: TypeName) {
        if !classifier.supertypes.contains_name(name) {
            classifier.supertypes.push_name(name);
        }
        if !classifier
            .supertype_templates
            .iter()
            .any(|ty| ty.obj_internal() == Some(name))
        {
            classifier.supertype_templates.push(Ty::obj_name(name));
        }
    }

    /// JVM realization of an intrinsic builtin companion. The semantic companion identity comes
    /// from `.kotlin_builtins`; this mapping supplies only the platform class that stores its value.
    pub(super) fn intrinsic_companion_realization(
        &self,
        owner: TypeName,
        companion: TypeName,
    ) -> Option<TypeName> {
        (owner.parent() == Some(type_name("kotlin")) && companion.nested_owner() == Some(owner))
            .then(|| {
                let segment = format!("{}CompanionObject", owner.segment_ref());
                crate::types::type_name_child(type_name("kotlin/jvm/internal"), &segment)
            })
    }

    fn cloneable_classifier() -> LibraryType {
        let mut supertypes = TypeNameList::new();
        supertypes.push("kotlin/Any");
        builtin_library_type(
            crate::libraries::TypeKind::Interface,
            ClassifierAccess::Public,
            false,
            supertypes,
            Vec::new(),
            Vec::new(),
            BuiltinGenericShape {
                type_params: Vec::new(),
                type_param_variances: Vec::new(),
                supertype_templates: vec![Ty::obj("kotlin/Any")],
            },
        )
    }

    fn clone_member(ret: Ty, visibility: Visibility) -> LibraryMember {
        let physical_ret = Ty::obj("kotlin/Any");
        let mut clone = LibraryMember::new(
            "clone".to_string(),
            Vec::new(),
            ret,
            "()Ljava/lang/Object;".to_string(),
        );
        // `Array.clone()` is a source-level JVM builtin, physically realized by the protected method
        // on `Object`. The selected callable carries that complete realization into emission.
        clone.owner = Some(crate::types::wk::java_object());
        clone.physical_ret = physical_ret;
        clone.visibility = visibility;
        clone
    }

    fn install_cloneable_clone(classifier: &mut LibraryType) {
        if let Some(clone) = classifier
            .members
            .iter_mut()
            .find(|member| member.name == "clone" && member.params.is_empty())
        {
            *clone = Self::clone_member(Ty::obj("kotlin/Any"), Visibility::Protected);
        } else {
            classifier.members.push(Self::clone_member(
                Ty::obj("kotlin/Any"),
                Visibility::Protected,
            ));
        }
    }

    fn install_array_platform_shape(internal: TypeName, classifier: &mut LibraryType) {
        for supertype in ["kotlin/Cloneable", "java/io/Serializable"] {
            Self::install_supertype(classifier, type_name(supertype));
        }

        let arguments = classifier
            .type_params
            .iter()
            .map(|formal| Ty::ty_param(formal, Ty::obj("kotlin/Any")))
            .collect::<Vec<_>>();
        let ret = Ty::obj_args_name(internal, &arguments);
        let replacement = Self::clone_member(ret, Visibility::Public);
        if let Some(clone) = classifier
            .members
            .iter_mut()
            .find(|member| member.name == "clone" && member.params.is_empty())
        {
            *clone = replacement;
        } else {
            classifier.members.push(replacement);
        }
    }
}

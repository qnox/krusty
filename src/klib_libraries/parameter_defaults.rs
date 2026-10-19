//! Constant default arguments, read from each declaration's serialized IR.
//!
//! Metadata says only that a parameter has a default; its value is the parameter's IR default
//! expression. A constant default is published as a [`DefaultValue`], which a call site that omits
//! the argument passes itself. Any other default (a call, a reference to another parameter) is
//! not a closed value and stays unpublished, so such a call is rejected rather than miscompiled.

use crate::fx_hash::{HashMap, HashSet};

use crate::libraries::DefaultValue;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrExprId, KlibIrExprKind, KlibIrFunction};
use crate::metadata::klib_ir::{KlibIrConstant, KlibIrModuleTrees, KlibIrSignature};

/// The top-level declarations, by package and simple name, whose IR a compilation decodes for
/// defaults. A member's defaults live in the tree of the top-level classifier that encloses it.
#[derive(Default)]
pub(super) struct DefaultSelection {
    by_module: HashMap<String, HashMap<Vec<String>, HashSet<String>>>,
    /// Exact top-level function identities whose selected metadata came from the typed platform
    /// stdlib input. A manifest cannot add an identity to this set.
    authoritative_functions: HashSet<(String, KlibPublicIdSignature)>,
}

impl DefaultSelection {
    pub(super) fn insert(&mut self, module: Option<&str>, signature: &KlibPublicIdSignature) {
        let Some(module) = module else {
            return;
        };
        let Some(top_level) = signature.declaration().segments().first() else {
            return;
        };
        self.by_module
            .entry(module.to_owned())
            .or_default()
            .entry(signature.package().segments().to_vec())
            .or_default()
            .insert(top_level.clone());
    }

    pub(super) fn select_authoritative_function(
        &mut self,
        module: Option<&str>,
        signature: &KlibPublicIdSignature,
    ) {
        if let Some(module) = module {
            self.authoritative_functions
                .insert((module.to_owned(), signature.clone()));
        }
    }

    fn selected_authoritative_function(&self, module: &str, signature: &KlibIrSignature) -> bool {
        matches!(
            signature,
            KlibIrSignature::Public(public)
                if self.authoritative_functions.contains(&(module.to_owned(), public.clone()))
        )
    }
}

/// The constant default of each value parameter, by the declaration's linkable identity. Only
/// declarations with at least one constant default are listed.
#[derive(Default)]
pub(super) struct ParameterDefaults {
    defaults: HashMap<(String, KlibIrSignature), Vec<Option<DefaultValue>>>,
    /// Every identity a library already declared, constant defaults or not.
    declared: HashSet<(String, KlibIrSignature)>,
}

impl ParameterDefaults {
    /// Read the defaults of every function `trees` declares.
    pub(super) fn add_library(
        &mut self,
        module: &str,
        trees: &KlibIrModuleTrees,
        selection: &DefaultSelection,
        compiler_builtin_owner: bool,
    ) {
        for signature in trees.function_signatures() {
            let Some((arena, function)) = trees.function(signature) else {
                continue;
            };
            let defaults = value_parameter_defaults(arena, function);
            self.record_selected(
                module,
                signature.clone(),
                defaults,
                selection,
                compiler_builtin_owner,
            );
        }
    }

    fn record_selected(
        &mut self,
        module: &str,
        signature: KlibIrSignature,
        defaults: Vec<Option<DefaultValue>>,
        selection: &DefaultSelection,
        compiler_builtin_owner: bool,
    ) {
        // Defaults follow the exact declaration selected by the inventory. Ordinary repeated
        // identities remain first-wins. Only a function whose selected declaration is from the
        // typed platform stdlib may replace a dependency-first record, including clearing a
        // conflicting constant when the authoritative IR has no closed constant default.
        let selected_authoritative = selection.selected_authoritative_function(module, &signature);
        // A dependency tree can be decoded because another overload under the same top-level name
        // is defaulted. It still cannot contribute defaults for an exact signature whose selected
        // declaration belongs to the platform stdlib, even if that stdlib has no indexed IR body.
        if !compiler_builtin_owner && selected_authoritative {
            return;
        }
        let replace = compiler_builtin_owner && selected_authoritative;
        let identity = (module.to_owned(), signature);
        if !self.declared.insert(identity.clone()) && !replace {
            return;
        }
        if defaults.iter().any(Option::is_some) {
            self.defaults.insert(identity, defaults);
        } else if replace {
            self.defaults.remove(&identity);
        }
    }

    /// The constant defaults of the declaration under `signature`, one per value parameter;
    /// empty when it has none.
    pub(super) fn of(
        &self,
        module: Option<&str>,
        signature: &KlibIrSignature,
    ) -> Vec<Option<DefaultValue>> {
        let Some(module) = module else {
            return Vec::new();
        };
        self.defaults
            .get(&(module.to_owned(), signature.clone()))
            .cloned()
            .unwrap_or_default()
    }
}

/// The default of one value parameter as its library serialized it: the IR expression, and the
/// closed value it denotes when it is a constant.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct KlibParameterDefault {
    pub(crate) expression: KlibIrExprId,
    pub(crate) constant: Option<DefaultValue>,
}

/// The default of value parameter `parameter` of `function`, a declaration of `arena`'s tree;
/// `None` when it declares none. Receivers and context parameters are not value parameters.
pub(crate) fn parameter_default(
    arena: &KlibIrArena,
    function: &KlibIrFunction,
    parameter: usize,
) -> Option<KlibParameterDefault> {
    let expression = function.regular_parameters.get(parameter)?.default_value?;
    let constant = match &arena.expr(expression).kind {
        KlibIrExprKind::Const(constant) => Some(constant_value(constant)),
        _ => None,
    };
    Some(KlibParameterDefault {
        expression,
        constant,
    })
}

/// One entry per value parameter: its constant default, if it has one.
fn value_parameter_defaults(
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Vec<Option<DefaultValue>> {
    (0..function.regular_parameters.len())
        .map(|parameter| parameter_default(arena, function, parameter)?.constant)
        .collect()
}

fn constant_value(constant: &KlibIrConstant) -> DefaultValue {
    match constant {
        KlibIrConstant::Null => DefaultValue::Null,
        KlibIrConstant::Boolean(value) => DefaultValue::Bool(*value),
        KlibIrConstant::Char(value) => DefaultValue::Char(*value),
        KlibIrConstant::Byte(value) => DefaultValue::Int(i64::from(*value)),
        KlibIrConstant::Short(value) => DefaultValue::Int(i64::from(*value)),
        KlibIrConstant::Int(value) => DefaultValue::Int(i64::from(*value)),
        KlibIrConstant::Long(value) => DefaultValue::Long(*value),
        KlibIrConstant::Float(value) => DefaultValue::Float(*value),
        KlibIrConstant::Double(value) => DefaultValue::Double(*value),
        KlibIrConstant::String(value) => DefaultValue::Str(value.as_str().into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::klib_libraries::classifier_records::classifier_record;
    use crate::klib_libraries::external_identities::ExternalIdentities;
    use crate::klib_libraries::inventory::PackageInventory;
    use crate::libraries::TypeKind;
    use crate::metadata::id_signature::KlibAccessorIdSignature;
    use crate::metadata::semantic::{
        KotlinClass, KotlinConstructor, KotlinFunction, KotlinFunctionTypeShape, KotlinModality,
        KotlinPackage, KotlinType,
    };
    use crate::types::{type_name, Visibility};

    fn class(internal: &str) -> KotlinType {
        KotlinType::Class {
            internal: internal.to_owned(),
            args: Vec::new(),
            nullable: false,
            shape: KotlinFunctionTypeShape::default(),
        }
    }

    fn defaulted_box() -> KotlinPackage {
        let classifier = KotlinClass {
            supertypes: vec!["kotlin/Any".to_owned()],
            supertype_tys: vec![class("kotlin/Any")],
            members: Vec::new(),
            functions: Vec::new(),
            properties: Vec::new(),
            type_aliases: Vec::new(),
            constructors: vec![KotlinConstructor {
                is_primary: true,
                params: vec![class("kotlin/Int")],
                param_names: vec!["value".to_owned()],
                param_defaults: vec![true],
                vararg: None,
                visibility: Visibility::Public,
            }],
            companion_name: None,
            type_params: Vec::new(),
            kind: TypeKind::Class,
            is_fun_interface: false,
            visibility: Visibility::Public,
            is_expect: false,
            enum_entries: Vec::new(),
            has_enum_entries: false,
            sealed_subclasses: Vec::new(),
            inline_class_property: None,
            modality: KotlinModality::Final,
            is_nested: false,
            is_inner: false,
            metadata_flags: 0,
            annotations: Vec::new(),
            nullable_member_returns: Vec::new(),
        };
        KotlinPackage {
            classes: [("fixture/Box".to_owned(), classifier)]
                .into_iter()
                .collect(),
            ..KotlinPackage::default()
        }
    }

    fn function_with_default(name: &str, parameter: &str, defaulted: bool) -> KotlinFunction {
        KotlinFunction {
            name: name.to_owned(),
            receiver: None,
            param_names: vec!["value".to_owned()],
            param_defaults: vec![defaulted],
            inline_modifiers: vec![crate::types::InlineParameterModifier::None],
            params: vec![class(parameter)],
            ret: class("kotlin/Unit"),
            formals: Vec::new(),
            vararg: None,
            visibility: Visibility::Public,
            modality: KotlinModality::Final,
            is_inline: false,
            has_reified_type_params: false,
            is_suspend: false,
            is_operator: false,
            is_infix: false,
            is_synthesized: false,
            is_expect: false,
            is_external: false,
            is_static: false,
            context_count: 0,
            context_kinds: Vec::new(),
            annotations: Vec::new(),
            annotation_applications: Vec::new(),
            return_value_status: Default::default(),
            contract: None,
        }
    }

    fn selected_authoritative_default() -> (DefaultSelection, KlibIrSignature) {
        let inventory = PackageInventory::from_package_inputs(vec![
            (
                vec!["fixture".to_owned()],
                KotlinPackage {
                    functions: vec![function_with_default("selected", "kotlin/Int", true)],
                    ..KotlinPackage::default()
                },
                false,
                false,
                Some("dependency".to_owned()),
            ),
            (
                vec!["fixture".to_owned()],
                KotlinPackage {
                    functions: vec![function_with_default("selected", "kotlin/Int", true)],
                    ..KotlinPackage::default()
                },
                true,
                false,
                Some("stdlib".to_owned()),
            ),
        ])
        .expect("the duplicate function is signable");
        let signature = KlibIrSignature::Public(
            inventory
                .functions(type_name("fixture"), "selected")
                .next()
                .expect("the authoritative function is selected")
                .signature
                .clone(),
        );
        (inventory.default_selection(), signature)
    }

    fn same_name_overload_selection() -> (DefaultSelection, KlibIrSignature, KlibIrSignature) {
        let inventory = PackageInventory::from_package_inputs(vec![
            (
                vec!["fixture".to_owned()],
                KotlinPackage {
                    functions: vec![
                        function_with_default("shared", "kotlin/Int", true),
                        function_with_default("shared", "kotlin/String", true),
                    ],
                    ..KotlinPackage::default()
                },
                false,
                false,
                Some("dependency".to_owned()),
            ),
            (
                vec!["fixture".to_owned()],
                KotlinPackage {
                    // The selected authoritative overload declares no default. The other
                    // dependency overload makes the coarse top-level `shared` tree eligible.
                    functions: vec![function_with_default("shared", "kotlin/Int", false)],
                    ..KotlinPackage::default()
                },
                true,
                false,
                Some("stdlib".to_owned()),
            ),
        ])
        .expect("the overloads are signable");
        let authoritative = inventory
            .functions(type_name("fixture"), "shared")
            .find(|function| function.compiler_builtin_owner)
            .expect("the authoritative overload is selected")
            .signature
            .clone();
        let dependency = inventory
            .functions(type_name("fixture"), "shared")
            .find(|function| !function.compiler_builtin_owner)
            .expect("the dependency overload remains selected")
            .signature
            .clone();
        (
            inventory.default_selection(),
            KlibIrSignature::Public(authoritative),
            KlibIrSignature::Public(dependency),
        )
    }

    #[test]
    fn constructor_defaults_are_published_on_the_ordinary_callable() {
        let mut inventory = PackageInventory::from_package_inputs(vec![(
            vec!["fixture".to_owned()],
            defaulted_box(),
            false,
            false,
            Some("fixture".to_owned()),
        )])
        .expect("the fixture is signable");
        let identity = type_name("fixture/Box");
        let signature = inventory
            .classifier(identity)
            .expect("the class is published")
            .constructors[0]
            .signature
            .clone();
        let mut defaults = ParameterDefaults::default();
        defaults.defaults.insert(
            ("fixture".to_owned(), KlibIrSignature::Public(signature)),
            vec![Some(DefaultValue::Int(7))],
        );
        inventory.attach_defaults(&defaults);

        let published = classifier_record(&inventory, &ExternalIdentities::default(), identity)
            .expect("the class record is published");
        assert_eq!(
            published.constructors[0].default_values,
            [Some(DefaultValue::Int(7))]
        );
    }

    #[test]
    fn repeated_identity_keeps_the_first_librarys_defaults() {
        let inventory = PackageInventory::from_packages(vec![(
            vec!["fixture".to_owned()],
            defaulted_box(),
            false,
        )])
        .expect("the fixture is signable");
        let signature = KlibIrSignature::Public(
            inventory
                .classifier(type_name("fixture/Box"))
                .expect("the class is published")
                .constructors[0]
                .signature
                .clone(),
        );
        let mut defaults = ParameterDefaults::default();
        defaults.record_selected(
            "fixture",
            signature.clone(),
            vec![Some(DefaultValue::Int(7))],
            &DefaultSelection::default(),
            false,
        );
        defaults.record_selected(
            "fixture",
            signature.clone(),
            vec![Some(DefaultValue::Int(9))],
            &DefaultSelection::default(),
            false,
        );
        assert_eq!(
            defaults.of(Some("fixture"), &signature),
            [Some(DefaultValue::Int(7))]
        );
    }

    #[test]
    fn equal_signatures_keep_each_modules_own_default() {
        let mut ordinary = defaulted_box();
        ordinary
            .classes
            .get_mut("fixture/Box")
            .expect("fixture class")
            .visibility = Visibility::Internal;
        let mut friend = defaulted_box();
        friend
            .classes
            .get_mut("fixture/Box")
            .expect("fixture class")
            .visibility = Visibility::Internal;
        let mut inventory = PackageInventory::from_package_inputs(vec![
            (
                vec!["fixture".to_owned()],
                ordinary,
                false,
                false,
                Some("ordinary".to_owned()),
            ),
            (
                vec!["fixture".to_owned()],
                friend,
                false,
                true,
                Some("friend".to_owned()),
            ),
        ])
        .expect("equal declarations from distinct modules are signable");
        let signature = KlibIrSignature::Public(
            inventory
                .classifier(type_name("fixture/Box"))
                .expect("the class is published")
                .constructors[0]
                .signature
                .clone(),
        );
        let mut defaults = ParameterDefaults::default();
        defaults.record_selected(
            "ordinary",
            signature.clone(),
            vec![Some(DefaultValue::Int(7))],
            &DefaultSelection::default(),
            false,
        );
        defaults.record_selected(
            "friend",
            signature.clone(),
            vec![Some(DefaultValue::Int(9))],
            &DefaultSelection::default(),
            false,
        );
        inventory.attach_defaults(&defaults);

        assert_eq!(
            defaults.of(Some("ordinary"), &signature),
            [Some(DefaultValue::Int(7))]
        );
        assert_eq!(
            defaults.of(Some("friend"), &signature),
            [Some(DefaultValue::Int(9))]
        );
        let published = classifier_record(
            &inventory,
            &ExternalIdentities::default(),
            type_name("fixture/Box"),
        )
        .expect("the friend classifier is published");
        assert_eq!(
            published.constructors[0].default_values,
            [Some(DefaultValue::Int(9))]
        );
    }

    #[test]
    fn selected_platform_function_uses_only_authoritative_defaults() {
        let (selection, signature) = selected_authoritative_default();
        let mut defaults = ParameterDefaults::default();
        defaults.record_selected(
            "stdlib",
            signature.clone(),
            vec![Some(DefaultValue::Int(7))],
            &selection,
            false,
        );
        assert!(defaults.of(Some("stdlib"), &signature).is_empty());
        defaults.record_selected(
            "stdlib",
            signature.clone(),
            vec![Some(DefaultValue::Int(9))],
            &selection,
            true,
        );
        defaults.record_selected(
            "stdlib",
            signature.clone(),
            vec![Some(DefaultValue::Int(11))],
            &selection,
            false,
        );
        assert_eq!(
            defaults.of(Some("stdlib"), &signature),
            [Some(DefaultValue::Int(9))]
        );
    }

    #[test]
    fn selected_platform_function_without_a_constant_clears_dependency_default() {
        let (selection, signature) = selected_authoritative_default();
        let mut defaults = ParameterDefaults::default();
        // Seed a stale value directly to pin the clearing half of the replacement contract.
        let identity = ("stdlib".to_owned(), signature.clone());
        defaults.declared.insert(identity.clone());
        defaults
            .defaults
            .insert(identity, vec![Some(DefaultValue::Int(7))]);
        defaults.record_selected("stdlib", signature.clone(), vec![None], &selection, true);
        assert!(defaults.of(Some("stdlib"), &signature).is_empty());
    }

    #[test]
    fn same_name_tree_does_not_leak_a_dependency_default_into_authoritative_overload() {
        let (selection, authoritative, dependency) = same_name_overload_selection();
        let mut defaults = ParameterDefaults::default();
        defaults.record_selected(
            "stdlib",
            authoritative.clone(),
            vec![Some(DefaultValue::Int(7))],
            &selection,
            false,
        );
        defaults.record_selected(
            "dependency",
            dependency.clone(),
            vec![Some(DefaultValue::Int(9))],
            &selection,
            false,
        );
        assert!(defaults.of(Some("stdlib"), &authoritative).is_empty());
        assert_eq!(
            defaults.of(Some("dependency"), &dependency),
            [Some(DefaultValue::Int(9))]
        );
    }

    #[test]
    fn platform_archive_does_not_replace_an_unrelated_classifier_default() {
        let inventory = PackageInventory::from_packages(vec![(
            vec!["fixture".to_owned()],
            defaulted_box(),
            false,
        )])
        .expect("the fixture is signable");
        let signature = KlibIrSignature::Public(
            inventory
                .classifier(type_name("fixture/Box"))
                .expect("the class is published")
                .constructors[0]
                .signature
                .clone(),
        );
        let selection = inventory.default_selection();
        let mut defaults = ParameterDefaults::default();
        defaults.record_selected(
            "ordinary",
            signature.clone(),
            vec![Some(DefaultValue::Int(7))],
            &selection,
            false,
        );
        defaults.record_selected(
            "ordinary",
            signature.clone(),
            vec![Some(DefaultValue::Int(9))],
            &selection,
            true,
        );
        assert_eq!(
            defaults.of(Some("ordinary"), &signature),
            [Some(DefaultValue::Int(7))]
        );
    }

    #[test]
    fn platform_archive_does_not_replace_an_accessor_with_a_function_identity() {
        let (selection, signature) = selected_authoritative_default();
        let KlibIrSignature::Public(public) = signature else {
            unreachable!("the fixture selects a public function")
        };
        let accessor = KlibIrSignature::Accessor(KlibAccessorIdSignature::new(
            public,
            "<get-selected>".to_owned(),
            1,
            0,
        ));
        let mut defaults = ParameterDefaults::default();
        defaults.record_selected(
            "stdlib",
            accessor.clone(),
            vec![Some(DefaultValue::Int(7))],
            &selection,
            false,
        );
        defaults.record_selected(
            "stdlib",
            accessor.clone(),
            vec![Some(DefaultValue::Int(9))],
            &selection,
            true,
        );
        assert_eq!(
            defaults.of(Some("stdlib"), &accessor),
            [Some(DefaultValue::Int(7))]
        );
    }
}

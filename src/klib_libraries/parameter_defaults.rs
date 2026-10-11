//! Constant default arguments, read from each declaration's serialized IR.
//!
//! Metadata says only that a parameter has a default; its value is the parameter's IR default
//! expression. A constant default is published as a [`DefaultValue`], which a call site that omits
//! the argument passes itself. Any other default (a call, a reference to another parameter) is
//! not a closed value and stays unpublished, so such a call is rejected rather than miscompiled.

use std::collections::{HashMap, HashSet};

use crate::libraries::DefaultValue;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrExprId, KlibIrExprKind, KlibIrFunction};
use crate::metadata::klib_ir::{KlibIrConstant, KlibIrModuleTrees, KlibIrSignature};

/// The top-level declarations, by package and simple name, whose IR a compilation decodes for
/// defaults. A member's defaults live in the tree of the top-level classifier that encloses it.
#[derive(Default)]
pub(super) struct TopLevelDeclarations {
    by_package: HashMap<Vec<String>, HashSet<String>>,
}

impl TopLevelDeclarations {
    pub(super) fn insert(&mut self, signature: &KlibPublicIdSignature) {
        let Some(top_level) = signature.declaration().segments().first() else {
            return;
        };
        self.by_package
            .entry(signature.package().segments().to_vec())
            .or_default()
            .insert(top_level.clone());
    }

    /// Whether the top-level declaration serialized under `signature` is one of these. A
    /// file-local identity is never one: metadata publishes no declaration under it.
    pub(super) fn contains(&self, signature: &KlibIrSignature) -> bool {
        let public = match signature {
            KlibIrSignature::Public(public) => public,
            KlibIrSignature::Accessor(accessor) => accessor.property(),
            KlibIrSignature::FileLocal { .. } => return false,
        };
        let Some(top_level) = public.declaration().segments().first() else {
            return false;
        };
        self.by_package
            .get(public.package().segments())
            .is_some_and(|names| names.contains(top_level))
    }
}

/// The constant default of each value parameter, by the declaration's linkable identity. Only
/// declarations with at least one constant default are listed.
#[derive(Default)]
pub(super) struct ParameterDefaults {
    defaults: HashMap<KlibIrSignature, Vec<Option<DefaultValue>>>,
    /// Every identity a library already declared, constant defaults or not.
    declared: HashSet<KlibIrSignature>,
}

impl ParameterDefaults {
    /// Read the defaults of every function `trees` declares.
    pub(super) fn add_library(&mut self, trees: &KlibIrModuleTrees) {
        for signature in trees.function_signatures() {
            let Some((arena, function)) = trees.function(signature) else {
                continue;
            };
            let defaults = value_parameter_defaults(arena, function);
            self.record(signature.clone(), defaults);
        }
    }

    fn record(&mut self, signature: KlibIrSignature, defaults: Vec<Option<DefaultValue>>) {
        // The inventory keeps the first library's declaration of a repeated identity, so the
        // defaults are that declaration's, including when none of them is a constant.
        if !self.declared.insert(signature.clone()) {
            return;
        }
        if defaults.iter().any(Option::is_some) {
            self.defaults.insert(signature, defaults);
        }
    }

    /// The constant defaults of the declaration under `signature`, one per value parameter;
    /// empty when it has none.
    pub(super) fn of(&self, signature: &KlibIrSignature) -> Vec<Option<DefaultValue>> {
        self.defaults.get(signature).cloned().unwrap_or_default()
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
    use crate::metadata::semantic::{
        KotlinClass, KotlinConstructor, KotlinFunctionTypeShape, KotlinModality, KotlinPackage,
        KotlinType,
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
            classes: [("fixture/Box".to_owned(), classifier)].into(),
            ..KotlinPackage::default()
        }
    }

    #[test]
    fn constructor_defaults_are_published_on_the_ordinary_callable() {
        let mut inventory =
            PackageInventory::from_packages(vec![(vec!["fixture".to_owned()], defaulted_box())])
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
            KlibIrSignature::Public(signature),
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
        let inventory =
            PackageInventory::from_packages(vec![(vec!["fixture".to_owned()], defaulted_box())])
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
        defaults.record(signature.clone(), vec![Some(DefaultValue::Int(7))]);
        defaults.record(signature.clone(), vec![Some(DefaultValue::Int(9))]);
        assert_eq!(defaults.of(&signature), [Some(DefaultValue::Int(7))]);
    }
}

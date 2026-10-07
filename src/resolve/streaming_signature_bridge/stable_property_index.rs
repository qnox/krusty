//! Reverse lookup from a stable declaration to the symbol-table property or constructor that
//! publishes it.
//!
//! Signature finalization asks for one declaration at a time. Scanning every class for each
//! request is quadratic in the module: a data-class property and its constructor are published
//! once per file, and each publication used to walk the whole class table.

use std::collections::HashMap;

use crate::fir::DeclarationId;
use crate::types::{Ty, TypeName};

use super::{semantic_classifier_self, SymbolTable};

struct PropertyFact {
    parameters: Vec<Ty>,
    ty: Ty,
    receiver: Option<Ty>,
    annotations: Vec<TypeName>,
}

struct ConstructorFact {
    parameters: Vec<Ty>,
    result: Ty,
    annotations: Vec<TypeName>,
    implicit_integer_coercion: Vec<bool>,
}

pub(super) struct StableMemberIndex {
    properties: HashMap<DeclarationId, PropertyFact>,
    constructors: HashMap<DeclarationId, ConstructorFact>,
}

impl StableMemberIndex {
    /// Record the first match in the same order as the former linear scans. A declaration lives in
    /// one table; `or_insert` keeps that first sighting when a later table repeats it.
    pub(super) fn build(table: &SymbolTable) -> Self {
        let mut properties = HashMap::new();
        let mut remember_property = |declaration, fact: PropertyFact| {
            properties.entry(declaration).or_insert(fact);
        };
        for property in table.source_props.values() {
            if let Some(declaration) = property.stable_declaration {
                remember_property(
                    declaration,
                    PropertyFact {
                        parameters: property.context_params.clone(),
                        ty: property.ty,
                        receiver: None,
                        annotations: property.annotations.clone(),
                    },
                );
            }
        }
        for property in table.ext_props.values().flatten() {
            if let Some(declaration) = property.stable_declaration {
                remember_property(
                    declaration,
                    PropertyFact {
                        parameters: property.context_params.clone(),
                        ty: property.ty,
                        receiver: Some(property.receiver),
                        annotations: property.annotations.clone(),
                    },
                );
            }
        }
        for class in table.classes.values() {
            for (name, property) in &class.declared_props {
                let Some(declaration) = property.stable_declaration else {
                    continue;
                };
                let ty = class
                    .generic_property_shapes
                    .get(name)
                    .copied()
                    // Direct nullable type parameters use the legacy class signature's dedicated
                    // nullable-parameter table rather than `generic_property_shapes`. Stable FIR
                    // must nevertheless publish the same symbolic type as the primary constructor,
                    // not the erased `Any?` member-selection view. The constructor's declaration
                    // shape is the authoritative source for a property parameter.
                    .or_else(|| {
                        class
                            .ctor_param_names
                            .iter()
                            .position(|(parameter, _)| parameter == name)
                            .and_then(|ordinal| class.ctor_param_shapes.get(ordinal))
                            .map(|(shape, _)| *shape)
                    })
                    .unwrap_or(property.ty);
                remember_property(
                    declaration,
                    PropertyFact {
                        parameters: property.context_params.clone(),
                        ty,
                        receiver: None,
                        annotations: property.annotations.clone(),
                    },
                );
            }
            for property in class.contextual_props.values().flatten() {
                let Some(declaration) = property.stable_declaration else {
                    continue;
                };
                remember_property(
                    declaration,
                    PropertyFact {
                        parameters: property.context_params.clone(),
                        ty: property.ty,
                        receiver: None,
                        annotations: property.annotations.clone(),
                    },
                );
            }
            for property in class.member_ext_props.values().flatten() {
                let Some(declaration) = property.stable_declaration() else {
                    continue;
                };
                remember_property(
                    declaration,
                    PropertyFact {
                        parameters: property.context_params().to_vec(),
                        ty: property.ret(),
                        receiver: Some(property.receiver_ty()),
                        annotations: property.annotations.clone(),
                    },
                );
            }
        }

        let mut constructors = HashMap::new();
        for class in table.classes.values() {
            if let Some(declaration) = class.primary_constructor_declaration {
                constructors
                    .entry(declaration)
                    .or_insert_with(|| ConstructorFact {
                        parameters: class
                            .ctor_param_shapes
                            .iter()
                            .map(|(parameter, _)| *parameter)
                            .collect(),
                        result: semantic_classifier_self(class),
                        annotations: class.primary_constructor_annotations.clone(),
                        implicit_integer_coercion: class.ctor_implicit_integer_coercion.clone(),
                    });
            }
            for (ordinal, declaration) in
                class.secondary_constructor_declarations.iter().enumerate()
            {
                let Some(declaration) = *declaration else {
                    continue;
                };
                constructors
                    .entry(declaration)
                    .or_insert_with(|| ConstructorFact {
                        parameters: class
                            .secondary_ctor_shapes
                            .get(ordinal)
                            .cloned()
                            .unwrap_or_default(),
                        result: semantic_classifier_self(class),
                        annotations: class
                            .secondary_constructor_annotations
                            .get(ordinal)
                            .cloned()
                            .unwrap_or_default(),
                        implicit_integer_coercion: class
                            .secondary_ctor_call_sigs
                            .get(ordinal)
                            .map(|signature| signature.implicit_integer_coercion.clone())
                            .unwrap_or_default(),
                    });
            }
        }
        Self {
            properties,
            constructors,
        }
    }

    pub(super) fn property(&self, declaration: DeclarationId) -> Option<(Vec<Ty>, Ty, Option<Ty>)> {
        self.properties
            .get(&declaration)
            .map(|property| (property.parameters.clone(), property.ty, property.receiver))
    }

    pub(super) fn property_annotations(&self, declaration: DeclarationId) -> Option<&[TypeName]> {
        self.properties
            .get(&declaration)
            .map(|property| property.annotations.as_slice())
    }

    pub(super) fn constructor(
        &self,
        declaration: DeclarationId,
    ) -> Option<(Vec<Ty>, Ty, Option<Ty>)> {
        self.constructors
            .get(&declaration)
            .map(|constructor| (constructor.parameters.clone(), constructor.result, None))
    }

    pub(super) fn constructor_annotations(
        &self,
        declaration: DeclarationId,
    ) -> Option<&[TypeName]> {
        self.constructors
            .get(&declaration)
            .map(|constructor| constructor.annotations.as_slice())
    }

    pub(super) fn constructor_implicit_integer_coercion(
        &self,
        declaration: DeclarationId,
        ordinal: usize,
    ) -> bool {
        self.constructors
            .get(&declaration)
            .and_then(|constructor| constructor.implicit_integer_coercion.get(ordinal))
            .copied()
            .unwrap_or(false)
    }
}

//! kotlinc's compatible-reference-array cast, before ordinary written-cast null guards.
//!
//! Array layers are compared by their readable elements. At the leaves, the actual type must be
//! a subtype of the expected declaration's erased upper-bound classifier. This is not evidence
//! that a call result is non-null, nor a JVM descriptor/owner-alias comparison.

use crate::assignable::{is_subtype, TyCtx, TypeOracle};
use crate::backend::BackendClassifierSource;
use crate::fx_hash::HashSet;
use crate::ir::{IrEnclosure, IrFile, IrTypeParameter};
use crate::types::{wk, Ty, TyKind, TypeName};

use super::checked_classifiers::CheckedClassifiers;
use super::generic_erasure::declared_primary_bound;
use super::private_static_access::StaticOwner;

/// Borrowed declaration records reachable through the current body's typed lexical ownership.
/// A missing record is not an implicit `Any` bound; only a known empty declaration has that bound.
pub(super) struct TypeParameters<'a>(Vec<&'a IrTypeParameter>);

impl<'a> TypeParameters<'a> {
    pub(super) fn at(
        ir: &'a IrFile,
        functions: [Option<u32>; 2],
        owner: Option<StaticOwner>,
        classifiers: &dyn BackendClassifierSource,
    ) -> Self {
        let mut parameters = Vec::new();
        let mut functions = functions.into_iter().flatten().collect::<Vec<_>>();
        let mut classes = match owner {
            Some(StaticOwner::Class(class)) => vec![class],
            _ => Vec::new(),
        };
        let mut seen_functions = HashSet::default();
        let mut seen_classes = HashSet::default();
        while !functions.is_empty() || !classes.is_empty() {
            if let Some(function) = functions.pop() {
                if !seen_functions.insert(function) {
                    continue;
                }
                if let Some(signature) = ir.signatures.get(&function) {
                    parameters.extend(&signature.type_params);
                }
                if let Some(referenced) = ir.callable_bound_type_parameters.get(&function) {
                    parameters.extend(referenced);
                }
                if let Some(captured) = ir.recorded_lambda_type_parameters(function) {
                    parameters.extend(captured);
                }
                if let Some(class) = ir.functions[function as usize].dispatch_receiver {
                    classes.push(class);
                }
                if let Some(enclosure) = ir.lambda_enclosures.get(&function) {
                    append_enclosure(ir, *enclosure, &mut functions, &mut classes);
                }
                if let Some(root) = super::lifted_names::root_container(ir, function) {
                    functions.push(root);
                }
            } else if let Some(class) = classes.pop() {
                if !seen_classes.insert(class) {
                    continue;
                }
                if let Some(signature) = ir.class_signature_name(class) {
                    parameters.extend(&signature.type_params);
                }
                if let Some(formals) = ir.foreign_template_classifier_parameters(class) {
                    parameters.extend(formals);
                }
                if let Some(outer) = classifiers
                    .classifier(class)
                    .and_then(|fact| fact.outer_instance)
                {
                    classes.push(outer);
                }
                if let Some(enclosure) = ir
                    .classes
                    .iter()
                    .find(|declaration| declaration.fq_name == class)
                    .and_then(|declaration| declaration.enclosure)
                {
                    append_enclosure(ir, enclosure, &mut functions, &mut classes);
                }
            }
        }
        Self(parameters)
    }

    fn named(&self, name: &str) -> Option<&IrTypeParameter> {
        self.0
            .iter()
            .copied()
            .find(|parameter| parameter.semantic_name == name)
    }

    fn erased_classifier(&self, ty: Ty, visiting: &mut HashSet<String>) -> Option<TypeName> {
        match ty.non_null().kind() {
            TyKind::TyParam(name, _) => {
                if !visiting.insert(name.as_str().to_owned()) {
                    return None;
                }
                let parameter = self.named(name.as_str())?;
                let bound = declared_primary_bound(parameter).unwrap_or(Ty::obj_name(wk::any()));
                let result = self.erased_classifier(bound, visiting);
                visiting.remove(name.as_str());
                result
            }
            TyKind::DefinitelyNotNull(inner) => self.erased_classifier(inner, visiting),
            _ => crate::libraries::function_classifiers::supertype_classifier(ty.non_null())
                .kotlin_class_internal(),
        }
    }

    fn actual_upper_bound(&self, ty: Ty, visiting: &mut HashSet<String>) -> Option<Ty> {
        match ty.non_null().kind() {
            TyKind::TyParam(name, _) => {
                if !visiting.insert(name.as_str().to_owned()) {
                    return None;
                }
                let parameter = self.named(name.as_str())?;
                let result = if parameter.bounds.is_empty() {
                    Some(Ty::obj_name(wk::any()))
                } else {
                    let bounds = parameter
                        .bounds
                        .iter()
                        .map(|(bound, _)| self.actual_upper_bound(*bound, visiting))
                        .collect::<Option<Vec<_>>>()?;
                    Some(Ty::intersection(&bounds))
                };
                visiting.remove(name.as_str());
                result
            }
            TyKind::DefinitelyNotNull(inner) => self.actual_upper_bound(inner, visiting),
            TyKind::Intersection(bounds) => {
                let bounds = bounds
                    .iter()
                    .map(|bound| self.actual_upper_bound(*bound, visiting))
                    .collect::<Option<Vec<_>>>()?;
                Some(Ty::intersection(&bounds))
            }
            _ if !ty.mentions_error() && !ty.mentions_pending() => {
                Some(crate::libraries::function_classifiers::supertype_classifier(ty.non_null()))
            }
            _ => None,
        }
    }
}

fn append_enclosure(
    ir: &IrFile,
    enclosure: IrEnclosure,
    functions: &mut Vec<u32>,
    classes: &mut Vec<TypeName>,
) {
    match enclosure {
        IrEnclosure::Function(function) | IrEnclosure::Lambda(function) => functions.push(function),
        IrEnclosure::Constructor { class, .. }
        | IrEnclosure::ClassInitializer(class)
        | IrEnclosure::Classifier(class) => classes.push(ir.classes[class as usize].fq_name),
        IrEnclosure::PropertyAccessor { property, setter } => {
            functions.extend(super::ir_emit::enclosure::realized_property_accessor(
                ir, property, setter,
            ));
        }
        IrEnclosure::File => {}
    }
}

struct ErasedHierarchy<'a, 'b>(&'a CheckedClassifiers<'b>);

impl TypeOracle for ErasedHierarchy<'_, '_> {
    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
        // The target is a raw classifier: no applied generic arguments are needed. Unknown facts
        // prove no relationship. In particular there is no physical JVM builtin-twin retry here.
        ty.kotlin_class_internal()
            .and_then(|class| self.0.classifier(class))
            .map(|fact| fact.supertypes.iter().copied().map(Ty::obj_name).collect())
            .unwrap_or_default()
    }
}

pub(super) fn compatible(
    mut actual: Ty,
    mut expected: Ty,
    parameters: &TypeParameters<'_>,
    classifiers: &CheckedClassifiers<'_>,
) -> bool {
    let mut array = false;
    while actual.non_null().is_reference_array() && expected.non_null().is_reference_array() {
        let (Some(actual_element), Some(expected_element)) = (
            actual.non_null().array_read_elem(),
            expected.non_null().array_read_elem(),
        ) else {
            return false;
        };
        actual = actual_element;
        expected = expected_element;
        array = true;
    }
    if !array {
        return false;
    }
    let Some(expected) = parameters.erased_classifier(expected, &mut HashSet::default()) else {
        return false;
    };
    let Some(actual) = parameters.actual_upper_bound(actual, &mut HashSet::default()) else {
        return false;
    };
    is_subtype(
        &TyCtx::new(),
        &ErasedHierarchy(classifiers),
        actual,
        Ty::obj_name(expected),
    )
}

#[cfg(test)]
mod tests;

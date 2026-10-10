//! Memoized `(namespace, name)` lookups over the signed inventory.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::builtin_realizations;
use super::classifier_records::{associated_callables, classifier_record};
use super::declaration_signatures::{SignedFunction, SignedProperty};
use super::external_identities::ExternalIdentities;
use super::inventory::PackageInventory;
use crate::libraries::{
    declared_function, declared_property, function_classifiers, CallablePlacement, Callables,
    ClassifierDeclaration, ExternalCallableKind, FnKind, FunctionInfo, FunctionSet,
    PropertyAccessorNames, PropertyInfo, PropertySet, ResolvedSymbols,
};
use crate::symbol_source::SymbolNamespace;

/// The records of one namespace, by declaration name.
type NamespaceRecords = HashMap<Box<str>, Rc<ResolvedSymbols>>;

/// One immutable record per lookup key. A record is built at most once, so every lookup of a key
/// returns the same candidates carrying the same identities.
#[derive(Default)]
pub(super) struct SymbolLookups {
    records: RefCell<HashMap<SymbolNamespace, NamespaceRecords>>,
}

impl SymbolLookups {
    pub(super) fn symbols(
        &self,
        namespace: SymbolNamespace,
        name: &str,
        build: impl FnOnce() -> ResolvedSymbols,
    ) -> Rc<ResolvedSymbols> {
        if let Some(record) = self
            .records
            .borrow()
            .get(&namespace)
            .and_then(|records| records.get(name))
        {
            return Rc::clone(record);
        }
        let record = Rc::new(build());
        self.records
            .borrow_mut()
            .entry(namespace)
            .or_default()
            .insert(name.into(), Rc::clone(&record));
        record
    }
}

/// The declarations at one key, normalized into selection candidates: the classifier the
/// namespace declares under `name`, and the callables it declares under `name`. A package
/// declares its top-level functions and properties; a classifier declares the callables named
/// through it with no value operand.
pub(super) fn declared_symbols(
    inventory: &PackageInventory,
    identities: &ExternalIdentities,
    namespace: SymbolNamespace,
    name: &str,
) -> ResolvedSymbols {
    // A probe never interns its leaf: only an already-interned identity can name a published
    // classifier.
    let declared_identity = namespace.existing_classifier(name);
    let alias = declared_identity.and_then(|identity| inventory.type_alias(identity));
    let declared = declared_identity.filter(|identity| inventory.classifier(*identity).is_some());
    let (classifier_name, classifier, classifier_declaration) = match (alias, declared) {
        (Some(alias), _) => {
            let classifier = classifier_record(inventory, identities, alias.target)
                .map(std::sync::Arc::new)
                .or_else(|| {
                    function_classifiers::classifier(alias.target)
                        .map(function_classifiers::synthetic)
                });
            (
                Some(alias.target),
                classifier,
                Some(ClassifierDeclaration::TypeAlias(alias.clone())),
            )
        }
        (None, Some(identity)) => (
            Some(identity),
            classifier_record(inventory, identities, identity).map(std::sync::Arc::new),
            Some(ClassifierDeclaration::Ordinary(identity)),
        ),
        // `FunctionN`, `SuspendFunctionN` and `KFunctionN` are declared by the language, not by
        // any library: no KLIB serializes them.
        (None, None) => match language_function_classifier(namespace, name) {
            Some(function) => (
                Some(function.identity()),
                Some(function_classifiers::synthetic(function)),
                Some(ClassifierDeclaration::Ordinary(function.identity())),
            ),
            None => (None, None, None),
        },
    };
    let (overloads, properties) = match namespace {
        SymbolNamespace::Package(package) => (
            inventory
                .functions(package, name)
                .map(|signed| {
                    published_function(
                        identities,
                        signed,
                        CallablePlacement::Package(package),
                        &Default::default(),
                    )
                })
                .collect(),
            inventory
                .properties(package, name)
                .map(|signed| {
                    published_property(
                        identities,
                        signed,
                        CallablePlacement::Package(package),
                        &Default::default(),
                    )
                })
                .collect(),
        ),
        SymbolNamespace::Classifier(owner) => {
            associated_callables(inventory, identities, owner, name)
        }
    };
    ResolvedSymbols {
        classifier_name,
        classifier_declaration,
        classifier,
        builtin_classifier: false,
        callables: Callables::from_parts(
            FunctionSet { overloads },
            PropertySet {
                overloads: properties,
            },
        ),
        importable_declaration: false,
    }
}

/// The shape a selected function or accessor at `placement` is realized as.
fn realization_kind(placement: CallablePlacement, extension: bool) -> ExternalCallableKind {
    match placement {
        CallablePlacement::Package(_) if extension => ExternalCallableKind::Extension,
        CallablePlacement::Package(_) | CallablePlacement::Associated { .. } => {
            ExternalCallableKind::TopLevel
        }
        // A member extension is still dispatched on an instance of its class.
        CallablePlacement::Member { .. } => ExternalCallableKind::Member,
    }
}

/// Normalize one signed function at `placement` and give it its declaration's identity.
pub(super) fn published_function(
    identities: &ExternalIdentities,
    signed: &SignedFunction,
    placement: CallablePlacement,
    enclosing: &crate::libraries::EnclosingBounds,
) -> FunctionInfo {
    let mut function = declared_function(
        placement,
        &signed.declaration,
        &signed.parameters,
        &signed.type_parameters,
        enclosing,
    );
    // A KLIB declaration's implementation is its serialized IR body, unless it is one of the
    // language's builtins, whose compiler operation the shared rules name by exact declaration.
    if let CallablePlacement::Package(package) = placement {
        builtin_realizations::realize_package_function(package, &mut function);
    }
    let kind = realization_kind(placement, function.kind == FnKind::Extension);
    identities.assign_function(signed, kind, &mut function);
    function
}

/// Normalize one signed property at `placement` and give it and its accessors their
/// declarations' identities.
pub(super) fn published_property(
    identities: &ExternalIdentities,
    signed: &SignedProperty,
    placement: CallablePlacement,
    enclosing: &crate::libraries::EnclosingBounds,
) -> PropertyInfo {
    // Each accessor keeps the name its library declares it under.
    let accessors = PropertyAccessorNames {
        getter: signed.getter.name(),
        setter: signed.setter.as_ref().map(|setter| setter.name()),
    };
    let mut property = declared_property(
        placement,
        &signed.declaration,
        &signed.parameters,
        &signed.type_parameters,
        accessors,
        enclosing,
    );
    // As for functions, the implementation is the serialized IR body of each accessor, unless
    // the property is a builtin.
    if let CallablePlacement::Package(package) = placement {
        builtin_realizations::realize_package_property(package, &mut property);
    }
    let kind = realization_kind(placement, property.receiver.is_some());
    identities.assign_property(signed, kind, &mut property);
    property
}

fn language_function_classifier(
    namespace: SymbolNamespace,
    name: &str,
) -> Option<function_classifiers::FunctionClassifier> {
    let SymbolNamespace::Package(package) = namespace else {
        return None;
    };
    function_classifiers::classifier_name_in(package, name)
        .and_then(function_classifiers::classifier)
}

//! Memoized `(namespace, name)` lookups over the signed inventory.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::external_identities::ExternalIdentities;
use super::inventory::PackageInventory;
use crate::libraries::{
    package_function, package_property, Callables, FunctionSet, PropertyAccessorNames, PropertySet,
    ResolvedSymbols,
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

/// The declarations at one key, normalized into selection candidates.
///
/// Only top-level functions and properties are published so far; a classifier namespace declares
/// nothing yet.
pub(super) fn declared_symbols(
    inventory: &PackageInventory,
    identities: &ExternalIdentities,
    namespace: SymbolNamespace,
    name: &str,
) -> ResolvedSymbols {
    let SymbolNamespace::Package(package) = namespace else {
        return ResolvedSymbols::default();
    };
    let overloads = inventory
        .functions(package, name)
        .map(|signed| {
            let mut function = package_function(
                package,
                &signed.declaration,
                &signed.parameters,
                &signed.type_parameters,
            );
            // A KLIB declaration's implementation is its serialized IR body. Do not replace it
            // with either a compiler intrinsic or an implementation role inferred from a stdlib
            // signature. A genuinely bodyless ABI declaration needs an explicit availability
            // contract from the body provider instead.
            identities.assign_function(&signed.signature, &signed.parameters, &mut function);
            function
        })
        .collect();
    let properties = inventory
        .properties(package, name)
        .map(|signed| {
            // Each accessor keeps the name its library declares it under.
            let accessors = PropertyAccessorNames {
                getter: signed.getter.name(),
                setter: signed.setter.as_ref().map(|setter| setter.name()),
            };
            let mut property = package_property(
                package,
                &signed.declaration,
                &signed.parameters,
                &signed.type_parameters,
                accessors,
            );
            // As for functions, the implementation is the serialized IR body of each accessor.
            identities.assign_property(signed, &mut property);
            property
        })
        .collect();
    ResolvedSymbols {
        callables: Callables::from_parts(
            FunctionSet { overloads },
            PropertySet {
                overloads: properties,
            },
        ),
        ..ResolvedSymbols::default()
    }
}

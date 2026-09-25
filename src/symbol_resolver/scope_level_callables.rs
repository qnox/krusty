//! Borrowed callable views over one scope-tower level.

use crate::libraries::{FunctionInfo, FunctionSet, PropertyInfo, ResolvedSymbols};
use std::rc::Rc;

/// Materialize one scope level's functions when the consumer needs ownership of the full set.
pub(super) fn function_set_from_symbols(symbols: &[Rc<ResolvedSymbols>]) -> FunctionSet {
    let capacity = symbols
        .iter()
        .map(|record| record.callables.functions().len())
        .sum();
    let mut overloads = Vec::with_capacity(capacity);
    for record in symbols {
        overloads.extend(record.callables.functions().iter().cloned());
    }
    FunctionSet { overloads }
}

/// Function overloads borrowed in the same record order as the materialized callable union.
/// Receiver walks filter a level down to applicable extensions, so they copy only retained
/// candidates rather than every overload in every record.
pub(super) fn level_functions(
    symbols: &[Rc<ResolvedSymbols>],
) -> impl Iterator<Item = &FunctionInfo> {
    symbols
        .iter()
        .flat_map(|record| record.callables.functions().iter())
}

/// Property overloads borrowed in record order.
pub(super) fn level_properties(
    symbols: &[Rc<ResolvedSymbols>],
) -> impl Iterator<Item = &PropertyInfo> {
    symbols
        .iter()
        .flat_map(|record| record.callables.properties().iter())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{Callables, FnKind, FunctionSet, LibraryCallable, PropertySet};
    use crate::types::Ty;

    #[test]
    fn overloads_are_borrowed_in_record_order() {
        let function = |owner: &str| {
            FunctionInfo::plain(
                FnKind::Extension,
                Some(Ty::String),
                LibraryCallable::library(
                    owner,
                    "pick",
                    vec![Ty::String],
                    Ty::Unit,
                    Ty::Unit,
                    "(Ljava/lang/String;)V",
                ),
            )
        };
        let record = |owners: &[&str]| {
            Rc::new(ResolvedSymbols {
                classifier_name: None,
                classifier: None,
                callables: Callables::from_parts(
                    FunctionSet {
                        overloads: owners.iter().map(|owner| function(owner)).collect(),
                    },
                    PropertySet::default(),
                ),
                importable_declaration: false,
            })
        };
        let symbols = vec![
            record(&["levels/AKt", "levels/BKt"]),
            record(&[]),
            record(&["levels/CKt"]),
        ];

        let borrowed = level_functions(&symbols).collect::<Vec<_>>();
        let copied = super::super::callables_from_symbols(&symbols);
        assert_eq!(borrowed.len(), copied.functions().len());
        assert!(borrowed
            .iter()
            .zip(copied.functions())
            .all(|(borrowed, copied)| borrowed.callable.owner == copied.callable.owner));
        assert!(std::ptr::eq(
            borrowed[2],
            &symbols[2].callables.functions()[0]
        ));
        assert_eq!(level_properties(&symbols).count(), 0);
    }
}

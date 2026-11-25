//! The runtime declarations native plugins' generated code calls, frozen by resolution.
//!
//! A plugin declares up front which runtime functions it may call ([`RuntimeRequirement`]: the
//! declaring package or classifier and the name). Resolution reads those declarations from the
//! provider once, while the compilation's symbols are built, and keeps every overload it found. The
//! handoff then only selects the exact signature a plugin asks for among the frozen overloads: it
//! never queries a provider, and a request for a function no plugin declared is an error.

use crate::fir::ExternalCallableId;
use crate::libraries::{Callables, FnKind};
use crate::plugins::runtime_declarations::{
    BoundRuntimeFunction, RuntimeDeclaration, RuntimeFunction, RuntimeRequirement,
};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::Ty;
use std::collections::HashMap;

/// One overload the provider declares for a required runtime function.
#[derive(Clone, Debug)]
struct RuntimeOverload {
    kind: FnKind,
    deprecated_hidden: bool,
    params: Vec<Ty>,
    ret: Ty,
    identity: Option<ExternalCallableId>,
}

/// Every overload of every runtime function the compilation's plugins declared they may call.
#[derive(Default)]
pub(crate) struct PluginRuntimeInventory {
    overloads: HashMap<RuntimeRequirement, Vec<RuntimeOverload>>,
}

impl PluginRuntimeInventory {
    /// Read the declarations `required` names from `source`, the only time the provider is asked.
    pub(super) fn freeze(source: &dyn SymbolSource, required: &[RuntimeRequirement]) -> Self {
        let mut overloads = HashMap::new();
        for requirement in required {
            if overloads.contains_key(requirement) {
                continue;
            }
            let (declared, kind) = match requirement.declaration {
                // A package declares its top-level functions in its own namespace.
                RuntimeDeclaration::Package(package) => (
                    source
                        .symbols(SymbolNamespace::Package(package), requirement.name)
                        .callables
                        .clone(),
                    FnKind::TopLevel,
                ),
                // A classifier's members are its normalized declared callables.
                RuntimeDeclaration::Member(owner) => (
                    source
                        .classifier(owner)
                        .and_then(|classifier| {
                            classifier.declared_callables.get(requirement.name).cloned()
                        })
                        .unwrap_or(Callables::None),
                    FnKind::Member,
                ),
                // A classifier's static functions take no receiver: they live in its namespace.
                RuntimeDeclaration::Static(owner) => (
                    source
                        .symbols(SymbolNamespace::Classifier(owner), requirement.name)
                        .callables
                        .clone(),
                    FnKind::TopLevel,
                ),
            };
            let found = declared
                .functions()
                .iter()
                .filter(|candidate| candidate.kind == kind)
                .map(|candidate| RuntimeOverload {
                    kind: candidate.kind,
                    deprecated_hidden: candidate.flags.deprecated_hidden,
                    params: candidate.callable.params.clone(),
                    ret: candidate.callable.ret,
                    identity: candidate.callable.external_identity,
                })
                .collect();
            overloads.insert(*requirement, found);
        }
        Self { overloads }
    }

    /// Bind a plugin's runtime contract to the one frozen overload with its exact signature.
    pub(crate) fn select(
        &self,
        function: &RuntimeFunction,
    ) -> Result<BoundRuntimeFunction, String> {
        let requirement = RuntimeRequirement {
            declaration: function.declaration,
            name: function.name,
        };
        let Some(frozen) = self.overloads.get(&requirement) else {
            return Err(format!(
                "`{}` was not declared by an enabled plugin as a runtime function it calls",
                function.described()
            ));
        };
        let mut candidates = frozen.iter().filter(|overload| {
            !overload.deprecated_hidden
                && overload.params == function.params
                && overload.ret == function.ret
        });
        let (Some(candidate), None) = (candidates.next(), candidates.next()) else {
            return Err(format!(
                "the runtime on the classpath declares no single `{}`",
                function.described()
            ));
        };
        let target = candidate
            .identity
            .ok_or_else(|| format!("`{}` has no dependency identity", function.described()))?;
        debug_assert!(matches!(candidate.kind, FnKind::TopLevel | FnKind::Member));
        Ok(BoundRuntimeFunction {
            target,
            params: function.params.clone(),
            ret: function.ret,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{FunctionInfo, FunctionSet, LibraryCallable, ResolvedSymbols};
    use crate::types::{type_name, TypeName};
    use std::cell::Cell;
    use std::rc::Rc;

    /// Declarations that count every query the provider answers.
    struct Declarations {
        symbols: ResolvedSymbols,
        queries: Cell<usize>,
    }

    impl SymbolSource for Declarations {
        fn symbols(&self, _namespace: SymbolNamespace, _name: &str) -> Rc<ResolvedSymbols> {
            self.queries.set(self.queries.get() + 1);
            Rc::new(self.symbols.clone())
        }
    }

    fn requirement(owner: TypeName) -> RuntimeRequirement {
        RuntimeRequirement {
            declaration: RuntimeDeclaration::Package(owner),
            name: "run",
        }
    }

    fn runtime(owner: TypeName) -> (Declarations, RuntimeFunction) {
        let mut callable =
            LibraryCallable::library(owner, "run", vec![Ty::Int], Ty::Unit, Ty::Unit, "(I)V");
        callable.external_identity = Some(ExternalCallableId::from_raw(7));
        let candidate = FunctionInfo::plain(FnKind::TopLevel, None, callable);
        (
            Declarations {
                symbols: ResolvedSymbols {
                    callables: Callables::Functions(FunctionSet {
                        overloads: vec![candidate],
                    }),
                    ..ResolvedSymbols::default()
                },
                queries: Cell::new(0),
            },
            RuntimeFunction {
                declaration: RuntimeDeclaration::Package(owner),
                name: "run",
                params: vec![Ty::Int],
                ret: Ty::Unit,
                appended: Vec::new(),
            },
        )
    }

    #[test]
    fn runtime_binding_uses_one_exact_declared_candidate() {
        let owner = type_name("example/Runtime");
        let (declared, runtime) = runtime(owner);
        let inventory = PluginRuntimeInventory::freeze(&declared, &[requirement(owner)]);
        assert_eq!(
            inventory.select(&runtime).unwrap().target,
            ExternalCallableId::from_raw(7)
        );
    }

    #[test]
    fn selection_reads_the_frozen_declarations_and_asks_the_provider_nothing() {
        let owner = type_name("example/Runtime");
        let (declared, runtime) = runtime(owner);
        let required = [requirement(owner), requirement(owner)];
        let inventory = PluginRuntimeInventory::freeze(&declared, &required);
        assert_eq!(
            declared.queries.get(),
            1,
            "one query per distinct requirement"
        );
        for _ in 0..3 {
            inventory.select(&runtime).unwrap();
        }
        assert_eq!(
            declared.queries.get(),
            1,
            "selecting never queries the provider"
        );
    }

    #[test]
    fn a_function_no_plugin_declared_is_refused() {
        let owner = type_name("example/Runtime");
        let (declared, runtime) = runtime(owner);
        let inventory = PluginRuntimeInventory::freeze(&declared, &[]);
        assert_eq!(
            inventory.select(&runtime).unwrap_err(),
            "`example.Runtime.run(Int): Unit` was not declared by an enabled plugin as a runtime function it calls"
        );
    }

    #[test]
    fn runtime_binding_rejects_ambiguous_exact_declarations() {
        let owner = type_name("example/Runtime");
        let (mut declared, runtime) = runtime(owner);
        let Callables::Functions(functions) = &mut declared.symbols.callables else {
            unreachable!()
        };
        let mut duplicate = functions.overloads[0].clone();
        duplicate.callable.external_identity = Some(ExternalCallableId::from_raw(8));
        functions.overloads.push(duplicate);
        let inventory = PluginRuntimeInventory::freeze(&declared, &[requirement(owner)]);
        assert!(inventory.select(&runtime).is_err());
    }
}

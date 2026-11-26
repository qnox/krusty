//! The runtime declarations native plugins' generated code calls, frozen by resolution.
//!
//! A plugin declares up front which runtime functions it may call ([`RuntimeRequirement`]: the
//! declaring package or classifier, the name and the exact signature). Resolution reads those
//! declarations from the provider once, while the compilation's symbols are built, and selects each
//! requirement's one overload there. The handoff then only looks the selection up: it never queries
//! a provider or chooses among overloads, and a request for a function no plugin declared is an
//! error.

use crate::fir::ExternalCallableId;
use crate::libraries::{Callables, FnKind};
use crate::plugins::runtime_declarations::{
    BoundRuntimeFunction, RuntimeDeclaration, RuntimeFunction, RuntimeRequirement,
};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::Ty;
use std::collections::HashMap;

/// The runtime function each requirement selected among the provider's declarations, or why none
/// could be.
#[derive(Default)]
pub(crate) struct PluginRuntimeInventory {
    selected: HashMap<RuntimeRequirement, Result<BoundRuntimeFunction, String>>,
}

impl PluginRuntimeInventory {
    /// Select every requirement's declaration from `source`, the only time the provider is asked.
    /// Each declaring package or classifier is queried once, however many signatures it is asked for.
    pub(super) fn freeze(source: &dyn SymbolSource, required: &[RuntimeRequirement]) -> Self {
        let mut declared: HashMap<(RuntimeDeclaration, &str), Vec<Declared>> = HashMap::new();
        let mut selected = HashMap::new();
        for requirement in required {
            if selected.contains_key(requirement) {
                continue;
            }
            let overloads = declared
                .entry((requirement.declaration, requirement.name))
                .or_insert_with(|| declared_overloads(source, requirement));
            selected.insert(requirement.clone(), select(overloads, requirement));
        }
        Self { selected }
    }

    /// The selection frozen for a plugin's runtime contract.
    pub(crate) fn select(
        &self,
        function: &RuntimeFunction,
    ) -> Result<BoundRuntimeFunction, String> {
        self.selected
            .get(&function.requirement())
            .cloned()
            .unwrap_or_else(|| {
                Err(format!(
                    "`{}` was not declared by an enabled plugin as a runtime function it calls",
                    function.described()
                ))
            })
    }
}

/// One overload the provider declares for a required runtime function.
struct Declared {
    deprecated_hidden: bool,
    params: Vec<Ty>,
    ret: Ty,
    identity: Option<ExternalCallableId>,
}

fn declared_overloads(
    source: &dyn SymbolSource,
    requirement: &RuntimeRequirement,
) -> Vec<Declared> {
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
                .and_then(|classifier| classifier.declared_callables.get(requirement.name).cloned())
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
    declared
        .functions()
        .iter()
        .filter(|candidate| candidate.kind == kind)
        .map(|candidate| Declared {
            deprecated_hidden: candidate.flags.deprecated_hidden,
            params: candidate.callable.params.clone(),
            ret: candidate.callable.ret,
            identity: candidate.callable.external_identity,
        })
        .collect()
}

/// The one overload with the requirement's exact signature.
fn select(
    overloads: &[Declared],
    requirement: &RuntimeRequirement,
) -> Result<BoundRuntimeFunction, String> {
    let described = || {
        RuntimeFunction {
            declaration: requirement.declaration,
            name: requirement.name,
            params: requirement.params.clone(),
            ret: requirement.ret,
            appended: Vec::new(),
        }
        .described()
    };
    let mut candidates = overloads.iter().filter(|overload| {
        !overload.deprecated_hidden
            && overload.params == requirement.params
            && overload.ret == requirement.ret
    });
    let (Some(candidate), None) = (candidates.next(), candidates.next()) else {
        return Err(format!(
            "the runtime on the classpath declares no single `{}`",
            described()
        ));
    };
    let target = candidate
        .identity
        .ok_or_else(|| format!("`{}` has no dependency identity", described()))?;
    Ok(BoundRuntimeFunction {
        target,
        params: requirement.params.clone(),
        ret: requirement.ret,
    })
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
        let inventory = PluginRuntimeInventory::freeze(&declared, &[runtime.requirement()]);
        assert_eq!(
            inventory.select(&runtime).unwrap().target,
            ExternalCallableId::from_raw(7)
        );
    }

    #[test]
    fn selection_reads_the_frozen_declarations_and_asks_the_provider_nothing() {
        let owner = type_name("example/Runtime");
        let (declared, runtime) = runtime(owner);
        let required = [runtime.requirement(), runtime.requirement()];
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
        let inventory = PluginRuntimeInventory::freeze(&declared, &[runtime.requirement()]);
        assert_eq!(
            inventory.select(&runtime).unwrap_err(),
            "the runtime on the classpath declares no single `example.Runtime.run(Int): Unit`"
        );
    }
}

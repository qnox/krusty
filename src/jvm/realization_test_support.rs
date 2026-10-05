//! The JVM realization passes, run the way the backend runs them, for tests that inspect common
//! IR after realization.

use super::classpath::Classpath;
use crate::ir::IrFile;

struct ClasspathCallables<'a>(&'a Classpath);

impl crate::symbol_source::SymbolSource for ClasspathCallables<'_> {
    fn external_callable(
        &self,
        identity: crate::fir::ExternalCallableId,
    ) -> Option<crate::libraries::ExternalCallableRealization> {
        self.0.external_callable(identity)
    }
}

/// Run the realization passes the JVM backend runs before emission, in the backend's order.
pub(crate) fn realize_calls(
    ir: &mut IrFile,
    stems: &[&str],
    classpath: &Classpath,
    dependencies: &dyn crate::symbol_source::SymbolSource,
) {
    let stems = stems
        .iter()
        .map(|stem| (*stem).to_owned())
        .collect::<Vec<_>>();
    let mut property_realizations = super::property_realizations::PropertyRealizations::default();
    super::local_properties::realize(ir, &mut property_realizations)
        .expect("every local property access is realized");
    let callables =
        crate::backend::CheckedBackendCallables::freeze(ir, &ClasspathCallables(classpath))
            .expect("every selected dependency callable has a frozen realization");
    let module = crate::backend::BackendModuleFacts::from_classifiers([], [])
        .expect("an empty module classifier snapshot is valid");
    let classifiers = crate::backend::CheckedBackendClassifiers::new(&module, dependencies);
    super::module_calls::realize(
        ir,
        &stems,
        &classifiers,
        &callables,
        &mut property_realizations,
    )
    .expect("every module call is realized");
    let mut default_call_operands = super::default_call_operands::DefaultCallOperands::default();
    super::external_calls::realize(
        ir,
        &classifiers,
        classpath,
        &callables,
        &mut default_call_operands,
    )
    .expect("every dependency call is realized");
}

//! The JVM realization passes, run the way the backend runs them, for tests that inspect common
//! IR after realization.

use krusty::ir::IrFile;
use krusty::jvm::classpath::Classpath;

struct ClasspathCallables<'a>(&'a Classpath);

impl krusty::symbol_source::SymbolSource for ClasspathCallables<'_> {
    fn external_callable(
        &self,
        identity: krusty::fir::ExternalCallableId,
    ) -> Option<krusty::libraries::ExternalCallableRealization> {
        self.0.external_callable(identity)
    }
}

/// Run the realization passes the JVM backend runs before emission, in the backend's order.
pub(crate) fn realize_calls(
    ir: &mut IrFile,
    stems: &[&str],
    classpath: &Classpath,
    dependencies: &dyn krusty::symbol_source::SymbolSource,
) {
    let facades = stems
        .iter()
        .map(|stem| krusty::names::file_part_short_name(stem))
        .collect::<Vec<_>>();
    let mut property_realizations = super::property_realizations::PropertyRealizations::default();
    super::local_properties::realize(ir, &mut property_realizations)
        .expect("every local property access is realized");
    let callables =
        krusty::backend::CheckedBackendCallables::freeze(ir, &ClasspathCallables(classpath))
            .expect("every selected dependency callable has a frozen realization");
    let module = krusty::backend::BackendModuleFacts::from_classifiers([], [])
        .expect("an empty module classifier snapshot is valid");
    let classifiers = krusty::backend::CheckedBackendClassifiers::new(&module, dependencies);
    let file_classes = super::file_facades::FileClasses {
        multifile_facades: vec![None; facades.len()].into(),
        containers: facades.into(),
    };
    super::module_calls::realize(
        ir,
        &file_classes,
        &classifiers,
        &callables,
        &mut property_realizations,
        &mut super::module_calls::SuperAccessors::default(),
        crate::ir_emit::JvmDefaultMode::default(),
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

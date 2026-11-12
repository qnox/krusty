//! The dispatch tables of runtime exception classes.
//!
//! `kotlin.Throwable` and the runtime exception classes this module constructs declare `toString`
//! (`Throwable.toString`: the class's qualified name, then its message) and inherit `kotlin.Any`'s
//! identity `equals` and `hashCode`, whose hash every exception object carries after its table
//! (`crate::identity`).

use super::*;
use krusty::backend::class_names::member_class_names;
use krusty::backend::BackendClassifierSource;
use krusty::types::TypeName;

impl Runtime {
    /// The struct of the runtime exception class `identity`, declared with its table on first use.
    pub(crate) fn exception_class(
        &mut self,
        module: &mut Module,
        source: &dyn BackendClassifierSource,
        identity: TypeName,
    ) -> Result<u32, String> {
        let structure = self.exceptions.ensure_class(module, source, identity)?;
        for (class, structure) in self.exceptions.declared_classes() {
            let names = member_class_names(class);
            let to_string = self.throwable_to_string(module, class, &names);
            let mut init = Code::default();
            for function in [
                self.objects.identity_equals,
                self.identity.hash_code,
                to_string,
            ] {
                module.reference_function(function);
                init.ref_func(function);
            }
            init.ref_null(HeapType::Concrete(self.objects.itable))
                .struct_new(self.objects.any_table);
            let table = module.constant_global(ValType::reference(self.objects.any_table), init);
            self.exceptions.publish_table(structure, table);
        }
        Ok(structure)
    }
}

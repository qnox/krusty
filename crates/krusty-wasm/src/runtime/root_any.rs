//! The dispatch table of directly allocated `kotlin.Any` instances.

use super::*;
use krusty::backend::class_names::{published_classifier, ClassNames};
use krusty::backend::BackendClassifierSource;

impl Runtime {
    pub(crate) fn root_any_table(
        &mut self,
        module: &mut Module,
        source: &dyn BackendClassifierSource,
    ) -> Result<u32, String> {
        if let Some(table) = self.root_any_table {
            return Ok(table);
        }
        let class = krusty::types::wk::any();
        let names = published_classifier(source, class)
            .ok_or("root Any construction without its published classifier")?;
        let class_names = ClassNames {
            qualified: names
                .qualified
                .clone()
                .ok_or("root Any has no qualified name")?,
            simple: names.simple.clone().ok_or("root Any has no simple name")?,
        };
        let to_string = self.identity_to_string(module, class, &class_names);
        let mut init = Code::default();
        for function in [
            self.objects.identity_equals,
            self.identity.hash_code,
            to_string,
        ] {
            module.reference_function(function);
            init.ref_func(function);
        }
        let info = self.objects.type_infos.global(module, class);
        self.objects.type_infos.name(module, class, &names);
        init.ref_null(HeapType::Concrete(self.objects.itable))
            .global_get(info)
            .struct_new(self.objects.any_table);
        let table = module.constant_global(ValType::reference(self.objects.any_table), init);
        self.root_any_table = Some(table);
        Ok(table)
    }
}

//! A dependency enum class in this closed unit, declared as a source enum is: its constants on the
//! class, which the shared enum realization builds, slots and all, when the target lowers the unit.

use super::*;
use crate::ir::{IrClass, IrConstructorTarget, IrEnumEntry};

impl Linker<'_, '_> {
    pub(super) fn materialize_enum_class(
        &mut self,
        ir: &mut IrFile,
        class: super::super::enum_classes::EnumClass,
    ) -> Result<(), KlibBodyDeclineReason> {
        if let Some(&materialized) = self.enum_classes.get(&class.constructor) {
            debug_assert_eq!(ir.classes[materialized as usize].fq_name, class.classifier);
            return Ok(());
        }
        if ir.class_id_by_name(class.classifier).is_some() {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "different dependency identities claim the same materialized classifier".to_owned(),
            ));
        }
        let mut materialized = IrClass::synthetic(class.classifier);
        materialized.declaration_name = Some(class.declaration_name);
        materialized.superclass = crate::types::wk::kotlin_enum();
        materialized.supertypes = vec![class.supertype];
        materialized.is_enum = true;
        materialized.enum_entries = class
            .entries
            .into_iter()
            .enumerate()
            .map(|(ordinal, name)| IrEnumEntry {
                name,
                diagnostic_span: None,
                argument_prelude: Vec::new(),
                args: Vec::new(),
                constructor_parameter_types: Vec::new(),
                constructor: IrConstructorTarget::UNRESTRICTED_PRIMARY,
                default_parameters: Vec::new(),
                decl_line: 0,
                source_order: u32::try_from(ordinal).expect("too many enum constants"),
                subclass: None,
            })
            .collect();
        let id = ir.add_class(materialized);
        ir.record_declared_class_self_type(class.classifier, Ty::obj_name(class.classifier));
        self.enum_classes.insert(class.constructor, id);
        Ok(())
    }
}

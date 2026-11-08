//! A dependency enum declared the way a source enum is, so the shared realization builds it.

use super::*;
use crate::fx_hash::HashMap;
use crate::ir::{IrClass, IrConstructorTarget, IrEnumEntry, IrExpr, IrFunction};
use crate::klib_libraries::{KlibDeclarationBodies, ProvenEnumClass};

impl<'u, 's> Linker<'u, 's> {
    /// Declare `classifier` from its serialized class when this unit is the one that realizes it.
    ///
    /// An earlier unit's enum stays there: this unit's reads keep the `EnumEntry` node, and the
    /// target calls the getter that unit exported. A second read in this unit reuses the class.
    pub(super) fn declare_enum(
        &mut self,
        ir: &mut IrFile,
        classifier: TypeName,
        entry: &str,
    ) -> Result<(), KlibBodyDeclineReason> {
        if self.materialized_elsewhere.contains(&classifier)
            || self.enum_classes.contains_key(&classifier)
        {
            return Ok(());
        }
        let (identity, entries) = match self
            .sources
            .bodies
            .proven_simple_enum(&self.active_module, classifier)
        {
            ProvenEnumClass::Simple { identity, entries } => (identity, entries),
            ProvenEnumClass::None => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "the serialized enum class is not in its producer graph".to_owned(),
                ))
            }
            ProvenEnumClass::Several => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "an enum class is serialized by more than one producer".to_owned(),
                ))
            }
            ProvenEnumClass::Unsupported(reason) => {
                return Err(KlibBodyDeclineReason::UnsupportedDeclaration(reason))
            }
        };
        if !entries.iter().any(|name| name == entry) {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "an enum entry is not a member of its proven class".to_owned(),
            ));
        }
        install_enum(
            ir,
            classifier,
            identity,
            &entries,
            self.enum_classes,
            self.enum_entry_getters,
        )
    }
}

impl DependencyBodyUnit {
    /// Declare a dependency enum a source file reads, when no earlier unit realized it.
    pub(crate) fn declare_loaded_enum(
        &mut self,
        bodies: &KlibDeclarationBodies,
        classifier: TypeName,
    ) -> Result<(), &'static str> {
        if self.materialized_elsewhere.contains(&classifier)
            || self.enum_classes.contains_key(&classifier)
        {
            return Ok(());
        }
        let (identity, entries) = match bodies.proven_loaded_enum(classifier) {
            ProvenEnumClass::Simple { identity, entries } => (identity, entries),
            ProvenEnumClass::None => return Err("the serialized enum class is absent"),
            ProvenEnumClass::Several => {
                return Err("an enum class is serialized by more than one producer")
            }
            ProvenEnumClass::Unsupported(reason) => return Err(reason),
        };
        install_enum(
            &mut self.ir,
            classifier,
            identity,
            &entries,
            &mut self.enum_classes,
            &mut self.enum_entry_getters,
        )
        .map_err(|reason| match reason {
            KlibBodyDeclineReason::UnsupportedDeclaration(reason) => reason,
            KlibBodyDeclineReason::SignatureMismatch(_) => {
                "a dependency enum collides with a class of this unit"
            }
            _ => "an enum class this realization does not construct",
        })
    }
}

fn install_enum(
    ir: &mut IrFile,
    classifier: TypeName,
    identity: KlibDependencyCallable,
    entries: &[String],
    enum_classes: &mut HashMap<TypeName, (KlibDependencyCallable, crate::ir::ClassId)>,
    getters: &mut Vec<(TypeName, String, FunId)>,
) -> Result<(), KlibBodyDeclineReason> {
    if entries.is_empty() {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "an enum class with no entries",
        ));
    }
    if ir.class_id_by_name(classifier).is_some() {
        return Err(KlibBodyDeclineReason::SignatureMismatch(
            "a dependency enum collides with a class of this unit".to_owned(),
        ));
    }
    let enum_type = Ty::obj_name(classifier);
    let mut class = IrClass::synthetic(classifier);
    class.superclass = crate::types::wk::kotlin_enum();
    class.is_enum = true;
    class.enum_entries = entries
        .iter()
        .enumerate()
        .map(|(order, name)| IrEnumEntry {
            name: name.clone(),
            diagnostic_span: None,
            argument_prelude: Vec::new(),
            args: Vec::new(),
            constructor_parameter_types: Vec::new(),
            constructor: IrConstructorTarget::UNRESTRICTED_PRIMARY,
            default_parameters: Vec::new(),
            decl_line: 0,
            source_order: u32::try_from(order).expect("an enum entry index fits u32"),
            subclass: None,
        })
        .collect();
    let class_id = ir.add_class(class);
    ir.record_declared_class_self_type(classifier, enum_type);
    for name in entries {
        let value = ir.add_expr(IrExpr::EnumEntry {
            classifier,
            name: name.as_str().into(),
        });
        ir.set_logical_type(value, enum_type);
        let result = ir.add_expr(IrExpr::Return(Some(value)));
        ir.set_logical_type(result, Ty::Nothing);
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![result],
            value: None,
        });
        let function = ir.add_fun(IrFunction {
            name: format!("enum-entry#{}", getters.len()),
            params: Vec::new(),
            ret: enum_type,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir.synthetic_methods.insert(function);
        getters.push((classifier, name.clone(), function));
    }
    enum_classes.insert(classifier, (identity, class_id));
    Ok(())
}

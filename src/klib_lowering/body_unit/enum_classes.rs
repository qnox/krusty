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
        if already_materialized(
            classifier,
            &identity,
            self.enum_classes,
            self.materialized_elsewhere,
        )
        .map_err(|reason| KlibBodyDeclineReason::SignatureMismatch(reason.to_owned()))?
        {
            return Ok(());
        }
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
        let (identity, entries) = match bodies.proven_loaded_enum(classifier) {
            ProvenEnumClass::Simple { identity, entries } => (identity, entries),
            ProvenEnumClass::None => return Err("the serialized enum class is absent"),
            ProvenEnumClass::Several => {
                return Err("an enum class is serialized by more than one producer")
            }
            ProvenEnumClass::Unsupported(reason) => return Err(reason),
        };
        if already_materialized(
            classifier,
            &identity,
            &self.enum_classes,
            &self.materialized_elsewhere,
        )? {
            return Ok(());
        }
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

/// Physical classifier keys are unqualified by producer. Reusing either a current-unit class or
/// an earlier unit's getter therefore requires the same exact module-qualified declaration.
fn already_materialized(
    classifier: TypeName,
    identity: &KlibDependencyCallable,
    current: &HashMap<TypeName, (KlibDependencyCallable, crate::ir::ClassId)>,
    previous: &HashMap<TypeName, KlibDependencyCallable>,
) -> Result<bool, &'static str> {
    let mut found = false;
    for selected in current
        .get(&classifier)
        .map(|(identity, _)| identity)
        .into_iter()
        .chain(previous.get(&classifier))
    {
        if selected != identity {
            return Err("equal enum classifier names from different KLIB producers");
        }
        found = true;
    }
    Ok(found)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::KlibDeclarationSignature;
    use crate::metadata::id_signature::KlibPublicIdSignature;

    #[test]
    fn enum_reuse_retains_producer_identity_within_and_between_support_units() {
        let classifier = crate::types::type_name("fixture/Enum");
        let first = KlibDependencyCallable {
            module: "first".to_owned(),
            declaration: KlibDeclarationSignature::Public(KlibPublicIdSignature::classifier(
                classifier,
            )),
        };
        let second = KlibDependencyCallable {
            module: "second".to_owned(),
            ..first.clone()
        };
        let mut current = HashMap::default();
        let mut previous = HashMap::default();
        assert_eq!(
            already_materialized(classifier, &first, &current, &previous),
            Ok(false)
        );
        current.insert(classifier, (first.clone(), 0));
        assert_eq!(
            already_materialized(classifier, &first, &current, &previous),
            Ok(true)
        );
        assert_eq!(
            already_materialized(classifier, &second, &current, &previous),
            Err("equal enum classifier names from different KLIB producers")
        );
        let mut unit = DependencyBodyUnit::default();
        previous.insert(classifier, first.clone());
        unit.excluding_materialized(&previous);
        current.clear();
        assert_eq!(
            already_materialized(classifier, &first, &current, &unit.materialized_elsewhere),
            Ok(true)
        );
        assert_eq!(
            already_materialized(classifier, &second, &current, &unit.materialized_elsewhere),
            Err("equal enum classifier names from different KLIB producers")
        );
        // A conflicting record cannot be hidden by an earlier matching record.
        current.insert(classifier, (second, 0));
        assert_eq!(
            already_materialized(classifier, &first, &current, &unit.materialized_elsewhere),
            Err("equal enum classifier names from different KLIB producers")
        );
    }
}

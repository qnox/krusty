//! Materialized dependency storage and its private initialization frame in this closed unit.

use super::*;
use crate::fx_hash::HashSet;
use crate::ir::{IrClass, IrField};

impl<'u, 's> Linker<'u, 's> {
    pub(super) fn materialize_class(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
        selected: KlibBodyCallable<'s>,
    ) -> Result<(), KlibBodyDeclineReason> {
        if self.classes.contains_key(identity) {
            return Ok(());
        }
        self.materialize_declaring_class(ir, selected).map(|_| ())
    }

    /// Materialize the public class declaring `selected`, a constructor or member of it, once.
    /// Answers whether this path materializes that class.
    pub(super) fn materialize_declaring_class(
        &mut self,
        ir: &mut IrFile,
        selected: KlibBodyCallable<'s>,
    ) -> Result<bool, KlibBodyDeclineReason> {
        let identity = selected.identity();
        let bodies = self.sources.bodies;
        let callees = self.sources.callees;
        let module = identity.module.clone();
        // A class installed through another of its members is not prepared again: the member
        // need only belong to it.
        let header = super::super::class_headers::authenticate(selected, bodies)?;
        if let Some(primary) = &header.contract.primary_constructor {
            let constructor = KlibDependencyCallable {
                module: header.contract.module.clone(),
                declaration: crate::libraries::KlibDeclarationSignature::Public(primary.clone()),
            };
            if self.classes.contains_key(&constructor) {
                return Ok(true);
            }
        }
        let Some((prepared, constructor)) = super::super::same_library_class::prepare_public(
            super::super::same_library_class::LocalClassSource {
                bodies,
                module: &module,
                validate_root: &|root| callees.validate_root_constructor(&module, root, bodies),
            },
            selected,
        )?
        else {
            return Ok(false);
        };
        self.install_class(ir, Box::new(prepared), Some(&constructor))?;
        Ok(true)
    }

    /// The class a value class member's body reads its value through: the value class with its
    /// one backing field. Its representation, the value itself wherever the target carries it
    /// unboxed, is the target's; the class records only the declaration's storage.
    pub(super) fn materialize_value_class(
        &mut self,
        ir: &mut IrFile,
        selected: KlibBodyCallable<'s>,
    ) -> Result<super::ValueMember<'s>, KlibBodyDeclineReason> {
        let header = super::super::class_headers::authenticate(selected, self.sources.bodies)?;
        let contract = header.contract;
        if contract.is_builtin_type() {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "a member of a builtin type",
            ));
        }
        if !contract.is_value || header.class.value_class_representation.is_none() {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a value class member's class is not a serialized value class".to_owned(),
            ));
        }
        let mut identities = HashSet::default();
        let mut fields = header
            .class
            .members
            .iter()
            .filter_map(super::super::class_storage::member_field);
        let (Some(field), None) = (fields.next(), fields.next()) else {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "a value class without exactly one backing field",
            ));
        };
        let stored = super::super::class_storage::declared_field(&header, field, &mut identities)?;
        let identity = KlibDependencyCallable {
            module: contract.module.clone(),
            declaration: KlibDeclarationSignature::Public(contract.signature.clone()),
        };
        if let Some(&class) = self.classes.get(&identity) {
            return Ok(super::ValueMember {
                class,
                field,
                contract,
            });
        }
        let classifier = contract.classifier;
        if ir.class_id_by_name(classifier).is_some() {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "different dependency identities claim the same materialized classifier".to_owned(),
            ));
        }
        let mut class = IrClass::synthetic(classifier);
        class.declaration_name = Some(super::super::klib_types::classifier_declaration(
            &contract.signature,
        )?);
        class.is_value = true;
        class.type_params = header.formals.clone();
        class.fields = vec![IrField::new(stored.name.clone(), stored.ty)
            .with_is_final(stored.final_)
            .with_is_private(stored.private)];
        class.properties = vec![crate::ir::IrProperty::stored_value(
            stored.name,
            stored.ty,
            0,
        )];
        class.ctor_param_count = 1;
        let class = ir.add_class(class);
        self.classes.insert(identity, class);
        Ok(super::ValueMember {
            class,
            field,
            contract,
        })
    }
}

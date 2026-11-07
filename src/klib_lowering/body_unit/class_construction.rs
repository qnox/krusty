//! Materialized dependency storage and its private initialization frame in this closed unit.

use super::*;
use crate::fx_hash::HashSet;
use crate::ir::{IrClass, IrCtorArg, IrCtorParameterProvenance, IrExpr, IrField};

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
        let Some(plan) = super::super::class_initialization::plan(selected, self.sources.bodies)?
        else {
            return Ok(());
        };
        let classifier = plan.storage.declaration.classifier;
        if ir.class_id_by_name(classifier).is_some() {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "different dependency identities claim the same materialized classifier".to_owned(),
            ));
        }
        let header = super::super::function_lowering::initializer_header(&plan)?;
        let self_type = super::super::klib_types::semantic_type(
            plan.header.arena,
            &plan.header.types,
            plan.header
                .class
                .this_receiver
                .as_ref()
                .expect("the plan authenticated its receiver")
                .ty,
        )?;
        let mut class = IrClass::synthetic(classifier);
        class.supertypes.push(plan.storage.superclass);
        class.fields = plan
            .storage
            .fields
            .iter()
            .map(|field| {
                IrField::new(field.name.clone(), field.ty)
                    .with_is_final(field.final_)
                    .with_is_private(field.private)
            })
            .collect();
        class.ctor_args = plan
            .header
            .function
            .regular_parameters
            .iter()
            .zip(header.params())
            .map(|(parameter, ty)| IrCtorArg {
                name: Some(parameter.name.clone()),
                context_kind: crate::types::ContextParameterKind::None,
                ty: *ty,
                declared_ty: Some(*ty),
                is_field: false,
                field_index: None,
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: None,
                anonymous_super_forward: None,
                capture: None,
                provenance: IrCtorParameterProvenance::Value,
                capture_identity: None,
            })
            .collect();
        // The initialization frame stores actual fields explicitly. Its plain value parameters
        // describe constructor arity, not leading constructor-backed storage.
        class.ctor_param_count = 0;
        class.explicit_param_stores = true;
        let class = ir.add_class(class);
        ir.record_declared_class_self_type(classifier, self_type);
        let function = declare_function(ir, &header);
        ir.functions[function as usize].is_static = false;
        ir.functions[function as usize].dispatch_receiver = Some(classifier);
        ir.set_method_visibility(function, crate::types::Visibility::Private);
        ir.classes[class as usize].methods.push(function);
        ir.note_class_method(class, function);
        let receiver = ir.add_expr(IrExpr::GetValue(0));
        ir.set_logical_type(receiver, Ty::obj_name(classifier));
        let args = header
            .params()
            .iter()
            .enumerate()
            .map(|(index, ty)| {
                let value = ir.add_expr(IrExpr::GetValue(
                    u32::try_from(index + 1).expect("a value index fits u32"),
                ));
                ir.set_logical_type(value, *ty);
                Some(value)
            })
            .collect();
        let initialize = ir.add_expr(IrExpr::MethodCall {
            class,
            index: 0,
            receiver,
            args,
        });
        ir.set_logical_type(initialize, Ty::Unit);
        ir.classes[class as usize].init_body = Some(initialize);
        self.classes.insert(identity.clone(), class);
        self.pending.push_back(PendingBody {
            identity: identity.clone(),
            arena: plan.header.arena,
            symbol: &plan.header.function.base.symbol,
            statements: &[],
            header,
            function,
            initialization: Some((class, plan)),
            value_member: None,
        });
        Ok(())
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

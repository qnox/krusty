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
        let Some(plan) = super::super::class_initialization::plan(
            selected,
            self.sources.bodies,
            self.sources.callees,
        )?
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
        let member = super::MemberClass {
            class,
            fields: plan.field_declarations.clone(),
            contract: plan.header.contract,
        };
        for function in &plan.overrides {
            self.declare_override(ir, member.clone(), plan.header.arena, function)?;
        }
        self.pending.push_back(PendingBody {
            identity: identity.clone(),
            arena: plan.header.arena,
            symbol: &plan.header.function.base.symbol,
            statements: &[],
            header,
            function,
            initialization: Some((class, plan)),
            value_member: None,
            local_body: None,
        });
        Ok(())
    }

    /// The class a member's body reads its receiver through. A final ordinary class is the one
    /// its primary constructor materializes. A value class is materialized here with its one
    /// backing field; its representation, the value itself wherever the target carries it
    /// unboxed, is the target's, and the class records only the declaration's storage.
    pub(super) fn materialize_member_class(
        &mut self,
        ir: &mut IrFile,
        selected: KlibBodyCallable<'s>,
    ) -> Result<super::MemberClass<'s>, KlibBodyDeclineReason> {
        let header = super::super::class_headers::authenticate(selected, self.sources.bodies)?;
        let contract = header.contract;
        if contract.is_builtin_type() {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "a member of a builtin type",
            ));
        }
        if !contract.is_value {
            return self.materialize_ordinary_member_class(ir, &header);
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
            return Ok(super::MemberClass {
                class,
                fields: vec![field],
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
        Ok(super::MemberClass {
            class,
            fields: vec![field],
            contract,
        })
    }

    /// The class a member of the final ordinary class `header` declares belongs to: the class
    /// its primary constructor materializes, which created every instance the member is called on.
    fn materialize_ordinary_member_class(
        &mut self,
        ir: &mut IrFile,
        header: &super::super::class_headers::ClassMemberHeader<'s, 's>,
    ) -> Result<super::MemberClass<'s>, KlibBodyDeclineReason> {
        let contract = header.contract;
        let primary = contract.primary_constructor.as_ref().ok_or(
            KlibBodyDeclineReason::UnsupportedDeclaration(
                "a class member without a primary constructor",
            ),
        )?;
        let identity = KlibDependencyCallable {
            module: contract.module.clone(),
            declaration: KlibDeclarationSignature::Public(primary.clone()),
        };
        let constructor = match self.sources.callees.get(&identity) {
            Some(KlibCalleeFact::Selected(callables)) => callables[0],
            _ => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "a class member's primary constructor has no unique provider declaration"
                        .to_owned(),
                ))
            }
        };
        self.materialize_class(ir, &identity, constructor)?;
        let class =
            *self
                .classes
                .get(&identity)
                .ok_or(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "a member of a class without ordinary initialization",
                ))?;
        let fields = header
            .class
            .members
            .iter()
            .filter_map(super::super::class_storage::member_field)
            .collect::<Vec<_>>();
        if fields.len() != ir.classes[class as usize].fields.len() {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a class member's declared fields disagree with its materialized class".to_owned(),
            ));
        }
        Ok(super::MemberClass {
            class,
            fields,
            contract,
        })
    }

    /// Declare `function`, an override of one of `Any`'s members which the materialized class
    /// `member` declares, as a method of the class, with the override edge a backend dispatches
    /// through. The class always carries it: no call site selects it, as dispatch reaches it.
    fn declare_override(
        &mut self,
        ir: &mut IrFile,
        member: super::MemberClass<'s>,
        arena: &'s KlibIrArena,
        function: &'s KlibIrFunction,
    ) -> Result<(), KlibBodyDeclineReason> {
        let module = &member.contract.module;
        let exact = |symbol: &crate::metadata::klib_ir::KlibIrSymbol| {
            let crate::metadata::klib_ir::KlibIrSignature::Public(signature) = &symbol.signature
            else {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "an override names a declaration without a public signature".to_owned(),
                ));
            };
            match self
                .sources
                .bodies
                .callee_origin(module, &KlibDeclarationSignature::Public(signature.clone()))
            {
                KlibCalleeOrigin::Exact(identity) => Ok(identity),
                KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => {
                    Err(KlibBodyDeclineReason::SignatureMismatch(
                        "an override names a declaration without one exact producer".to_owned(),
                    ))
                }
            }
        };
        let provided = |identity: &KlibDependencyCallable| {
            let selected = match self.sources.callees.get(identity) {
                Some(KlibCalleeFact::Selected(callables)) => callables[0],
                _ => {
                    return Err(KlibBodyDeclineReason::SignatureMismatch(
                        "an override edge has no unique provider declaration".to_owned(),
                    ))
                }
            };
            let target = self.sources.callees.reference_target(identity).ok_or(
                KlibBodyDeclineReason::SignatureMismatch(
                    "an override edge has no frozen declaration".to_owned(),
                ),
            )?;
            Ok((selected, target))
        };
        let identity = exact(&function.base.symbol)?;
        let (selected, implementation) = provided(&identity)?;
        let mut edges = Vec::with_capacity(function.overridden.len());
        for overridden in &function.overridden {
            let (declaration, target) = provided(&exact(overridden)?)?;
            let Some(crate::types::SemanticCallableOwner::Classifier(owner)) =
                declaration.declaration_owner()
            else {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "an overridden declaration is not a class member".to_owned(),
                ));
            };
            edges.push((declaration, target, owner));
        }
        let header = body_header(Some(selected), arena, function, self.sources.bodies)?;
        if header.value_receiver().is_none() {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a materialized class's override has no receiver".to_owned(),
            ));
        }
        let class = member.class;
        let declared = self.queue(
            ir,
            &identity,
            Some(selected),
            arena,
            function,
            super::BodyHeader {
                header,
                value_member: Some(member),
            },
        )?;
        ir.classes[class as usize].methods.push(declared);
        ir.note_class_method(class, declared);
        let implementation_owner = ir.classes[class as usize].fq_name;
        let (params, result) = (
            ir.functions[declared as usize].params.clone(),
            ir.functions[declared as usize].ret,
        );
        let records = ir
            .function_overrides
            .entry(implementation_owner)
            .or_default();
        for (declaration, target, owner) in edges {
            records.push(crate::ir::IrFunctionOverride {
                implementation: crate::fir::ResolvedFunctionOverrideTarget::External(
                    implementation,
                ),
                implementation_function: Some(declared),
                implementation_owner,
                overridden: crate::fir::ResolvedFunctionOverrideTarget::External(target),
                overridden_owner: owner,
                overridden_semantic_role: declaration.semantic_role(),
                collection_barrier: None,
                overridden_is_interface: false,
                name: function.name.clone(),
                declared_parameters: declaration.params().to_vec(),
                declared_result: declaration.ret(),
                applied_parameters: declaration.params().to_vec(),
                applied_result: declaration.ret(),
                implementation_parameters: params.clone(),
                implementation_parameter_identities: selected.parameter_identities().to_vec(),
                overridden_parameter_identities: declaration.parameter_identities().to_vec(),
                implementation_result: result,
                suspend: false,
                has_kotlin_superclass_override: false,
                depth: 0,
            });
        }
        Ok(())
    }
}

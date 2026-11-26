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
            return self.materialize_own_delegation(ir, identity, selected);
        };
        self.materialize_plan(ir, identity, plan)
    }

    /// Materialize `class`, an object or an enum class, with its instances, created by its private
    /// primary constructor, which `identity` names. The unit publishes the object's instance, or
    /// the enum's constants, as the class's own file would.
    pub(super) fn materialize_declared_instances(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
        class: &'s crate::libraries::KlibBodyClassContract,
    ) -> Result<(), KlibBodyDeclineReason> {
        if self.classes.contains_key(identity) {
            return Ok(());
        }
        let Some(plan) = super::super::class_initialization::declared_instances_plan(
            class,
            self.sources.bodies,
            self.sources.callees,
        )?
        else {
            return Ok(());
        };
        self.materialize_plan(ir, identity, plan)
    }

    fn materialize_plan(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
        plan: super::super::class_initialization::ClassInitialization<'s, 's>,
    ) -> Result<(), KlibBodyDeclineReason> {
        let classifier = plan.storage.declaration.classifier;
        // Each secondary constructor of a class without a primary one initializes the class this
        // unit already materialized for another of them.
        let existing = ir
            .class_id_by_name(classifier)
            .filter(|class| !plan.primary && self.classes.values().any(|known| known == class));
        if existing.is_none() && ir.class_id_by_name(classifier).is_some() {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "different dependency identities claim the same materialized classifier".to_owned(),
            ));
        }
        let header = super::super::function_lowering::initializer_header(&plan.header)?;
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
        let retained_initializer = plan
            .superclass
            .as_ref()
            .filter(|call| call.materialized)
            .and_then(|call| self.sources.externals.get(&call.constructor))
            .and_then(|external| external.constructor_initializer_target());
        // A materialized superclass is materialized first, from its own admitted initialization.
        let superclass = match &plan.superclass {
            Some(call) if call.materialized && retained_initializer.is_none() => {
                let selected = match self.sources.callees.get(&call.constructor) {
                    Some(KlibCalleeFact::Selected(callables)) => callables[0],
                    _ => {
                        return Err(KlibBodyDeclineReason::SignatureMismatch(
                            "a superclass constructor has no unique provider declaration"
                                .to_owned(),
                        ))
                    }
                };
                self.materialize_class(ir, &call.constructor, selected)?;
                let class = *self.classes.get(&call.constructor).ok_or(
                    KlibBodyDeclineReason::UnsupportedDeclaration(
                        "a superclass without ordinary initialization",
                    ),
                )?;
                Some(class)
            }
            _ => None,
        };
        let class = match existing {
            Some(class) => class,
            None => self.declare_materialized_class(ir, &plan, &header, self_type)?,
        };
        let function = declare_function(ir, &header);
        ir.functions[function as usize].is_static = false;
        ir.functions[function as usize].dispatch_receiver = Some(classifier);
        ir.set_method_visibility(function, crate::types::Visibility::Private);
        let method = u32::try_from(ir.classes[class as usize].methods.len())
            .expect("a method index fits u32");
        ir.classes[class as usize].methods.push(function);
        ir.note_class_method(class, function);
        let receiver = ir.add_expr(IrExpr::GetValue(0));
        ir.set_logical_type(receiver, self_type);
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
            index: method,
            receiver,
            args,
        });
        ir.set_logical_type(initialize, Ty::Unit);
        let secondary = if plan.primary {
            ir.classes[class as usize].init_body = Some(initialize);
            None
        } else {
            let delegate = secondary_delegation(ir, &plan, superclass)?;
            let secondaries = &mut ir.classes[class as usize].secondary_ctors;
            secondaries.push(materialized_secondary(
                &plan.header,
                &header,
                delegate,
                initialize,
            ));
            Some(secondaries.len() - 1)
        };
        if let Some(target) = retained_initializer {
            let target = crate::ir::IrExternalConstructorTarget::unresolved(target);
            match secondary {
                Some(ordinal) => {
                    ir.external_secondary_super_constructors.insert(
                        (
                            classifier,
                            u32::try_from(ordinal).expect("a constructor ordinal fits u32"),
                        ),
                        target,
                    );
                }
                None => {
                    ir.external_super_constructors.insert(classifier, target);
                }
            }
        }
        self.classes.insert(identity.clone(), class);
        self.constructors.insert(
            identity.clone(),
            super::MaterializedConstructor {
                initializer: function,
                secondary,
            },
        );
        let member = super::MemberClass {
            class,
            declaring: &plan.header.class.base.symbol,
            fields: plan.field_declarations.clone(),
            properties: super::super::class_storage::backed_properties(&plan.header.class.members),
            contract: plan.header.contract,
        };
        // An enum's constants are its instances: the arguments each passes to the constructor
        // are lowered in the enum's own initialization frame.
        if plan.header.contract.kind == crate::libraries::TypeKind::Enum {
            self.queue_enum_entry_arguments(class, identity, &plan, function);
            // Its synthetic `entries` body calls the checked `enumEntries` with its `values()`,
            // an edge the unit lowers as any body's call, so the enum's entries are built by the
            // unit's own factory.
            let factory =
                super::super::class_initialization::entries_factory(&plan, self.sources.callees);
            if let Some(KlibDeclarationSignature::Public(factory)) =
                factory.map(|factory| &factory.declaration)
            {
                self.link(ir, factory)?;
            }
        }
        if existing.is_none() {
            for function in &plan.dispatched {
                self.declare_dispatched(ir, member.clone(), plan.header.arena, function)?;
            }
            self.declare_inherited_implementations(
                ir,
                class,
                plan.header.contract,
                plan.header.arena,
                plan.header.class,
            )?;
        }
        let storage_accessors = if existing.is_none() {
            plan.storage_accessors.clone()
        } else {
            Vec::new()
        };
        let (contract, arena) = (plan.header.contract, plan.header.arena);
        // The initialization reads the class's private members as its members do.
        self.pending.push_back(PendingBody {
            identity: identity.clone(),
            arena: plan.header.arena,
            symbol: &plan.header.function.base.symbol,
            statements: &[],
            header,
            function,
            initialization: Some((class, super::PendingInitialization::Class(Box::new(plan)))),
            value_member: Some(member),
            local_body: None,
            static_initialization: None,
        });
        for function in storage_accessors {
            self.declare_storage_accessor(ir, &contract.module, arena, function)?;
        }
        Ok(())
    }

    /// Declare a default accessor of storage the class being materialized owns, as a call of it
    /// would: a later unit calls its export rather than materializing the class again.
    fn declare_storage_accessor(
        &mut self,
        ir: &mut IrFile,
        module: &str,
        arena: &'s KlibIrArena,
        function: &'s KlibIrFunction,
    ) -> Result<(), KlibBodyDeclineReason> {
        let signature = crate::klib_libraries::member_signature(&function.base.symbol.signature)
            .ok_or_else(|| {
                KlibBodyDeclineReason::SignatureMismatch(
                    "a storage accessor is not a member".to_owned(),
                )
            })?;
        let KlibCalleeOrigin::Exact(identity) =
            self.sources.bodies.linked_origin(module, &signature)
        else {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a storage accessor has no exact producer".to_owned(),
            ));
        };
        // An accessor the target realizes itself, or an earlier unit already exported, is linked
        // as that external edge; the class's storage does not re-emit it.
        if self.sources.externals.contains_key(&identity) {
            return Ok(());
        }
        // An accessor the program's closure never selected has no caller to export it to.
        let selected = match self.sources.callees.get(&identity) {
            Some(KlibCalleeFact::Selected(callables)) => callables[0],
            None => return Ok(()),
            Some(KlibCalleeFact::Ambiguous) => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "a storage accessor has no unique provider declaration".to_owned(),
                ))
            }
        };
        self.declare(ir, &identity, Some(selected), arena, function)?;
        Ok(())
    }

    /// A secondary constructor delegating to another constructor of its class: the delegation's
    /// target, materialized first, reaches the primary constructor that materializes the class,
    /// and the class gains a secondary constructor that delegates to the target with the
    /// delegation's arguments, then runs a private function holding the secondary constructor's
    /// own statements. A class whose primary initialization is not admitted keeps its target
    /// boundary.
    fn materialize_own_delegation(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
        selected: KlibBodyCallable<'s>,
    ) -> Result<(), KlibBodyDeclineReason> {
        let Some(delegation) = super::super::class_initialization::own_delegation(
            selected,
            self.sources.bodies,
            self.sources.callees,
        )?
        else {
            return Ok(());
        };
        self.materialize_class(ir, &delegation.target, delegation.target_selected)?;
        let class = *self.classes.get(&delegation.target).ok_or_else(|| {
            KlibBodyDeclineReason::SignatureMismatch(
                "an admitted delegation target materialized no class".to_owned(),
            )
        })?;
        let fields = delegation
            .header
            .class
            .members
            .iter()
            .filter_map(super::super::class_storage::member_field)
            .collect::<Vec<_>>();
        if fields.len() != ir.classes[class as usize].fields.len() {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a secondary constructor's class declares other fields than its materialized class"
                    .to_owned(),
            ));
        }
        let classifier = delegation.header.contract.classifier;
        let header = super::super::function_lowering::initializer_header(&delegation.header)?;
        let function = declare_function(ir, &header);
        ir.functions[function as usize].is_static = false;
        ir.functions[function as usize].dispatch_receiver = Some(classifier);
        ir.set_method_visibility(function, crate::types::Visibility::Private);
        let method = u32::try_from(ir.classes[class as usize].methods.len())
            .expect("a method index fits u32");
        ir.classes[class as usize].methods.push(function);
        ir.note_class_method(class, function);
        let self_type = ir.declared_class_self_type(classifier).ok_or_else(|| {
            KlibBodyDeclineReason::SignatureMismatch(
                "a materialized class has no declared self type".to_owned(),
            )
        })?;
        let receiver = ir.add_expr(IrExpr::GetValue(0));
        ir.set_logical_type(receiver, self_type);
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
        let run = ir.add_expr(IrExpr::MethodCall {
            class,
            index: method,
            receiver,
            args,
        });
        ir.set_logical_type(run, Ty::Unit);
        // The target is the primary constructor, or the materialized secondary one at its place.
        let delegate = match self
            .constructors
            .get(&delegation.target)
            .and_then(|constructor| constructor.secondary)
        {
            Some(secondary) => crate::ir::CtorDelegateTarget::This {
                target_params: ir.classes[class as usize].secondary_ctors[secondary]
                    .params
                    .clone(),
                target: crate::ir::IrConstructorTarget {
                    ordinal: u32::try_from(secondary + 1).expect("a constructor ordinal fits u32"),
                    access: crate::ir::IrConstructorAccess::Unrestricted,
                },
                default_masks: Vec::new(),
            },
            None => crate::ir::CtorDelegateTarget::This {
                target_params: ir.classes[class as usize]
                    .ctor_args
                    .iter()
                    .map(|parameter| parameter.ty)
                    .collect(),
                target: crate::ir::IrConstructorTarget::UNRESTRICTED_PRIMARY,
                default_masks: Vec::new(),
            },
        };
        let secondaries = &mut ir.classes[class as usize].secondary_ctors;
        secondaries.push(materialized_secondary(
            &delegation.header,
            &header,
            delegate,
            run,
        ));
        let secondary = secondaries.len() - 1;
        self.classes.insert(identity.clone(), class);
        self.constructors.insert(
            identity.clone(),
            super::MaterializedConstructor {
                initializer: function,
                secondary: Some(secondary),
            },
        );
        self.pending.push_back(PendingBody {
            identity: identity.clone(),
            arena: delegation.header.arena,
            symbol: &delegation.header.function.base.symbol,
            statements: delegation.statements,
            header,
            function,
            initialization: Some((
                class,
                super::PendingInitialization::OwnDelegation {
                    secondary,
                    arguments: delegation.arguments,
                },
            )),
            value_member: Some(super::MemberClass {
                class,
                declaring: &delegation.header.class.base.symbol,
                fields,
                properties: super::super::class_storage::backed_properties(
                    &delegation.header.class.members,
                ),
                contract: delegation.header.contract,
            }),
            local_body: None,
            static_initialization: None,
        });
        Ok(())
    }

    /// The class `plan` initializes, with its storage and its primary constructor's parameters
    /// when it has one.
    fn declare_materialized_class(
        &self,
        ir: &mut IrFile,
        plan: &super::super::class_initialization::ClassInitialization<'s, 's>,
        header: &super::super::function_lowering::FunctionHeader,
        self_type: Ty,
    ) -> Result<crate::ir::ClassId, KlibBodyDeclineReason> {
        let classifier = plan.storage.declaration.classifier;
        let mut class = IrClass::synthetic(classifier);
        class.is_source_declared = true;
        class.is_object = plan.header.contract.kind == crate::libraries::TypeKind::Object;
        if plan.header.contract.kind == crate::libraries::TypeKind::Enum {
            class.is_enum = true;
            class.declaration_name = Some(super::super::klib_types::classifier_declaration(
                &plan.header.contract.signature,
            )?);
            class.enum_entries = super::enum_materialization::declared_entries(plan);
        }
        if class.is_object {
            class.applied_annotations =
                super::super::object_instances::instance_annotations(plan.header.class);
        }
        class.supertypes.clone_from(&plan.supertypes);
        set_materialized_modality(&mut class, plan.header.contract.modality);
        if let Some(call) = &plan.superclass {
            class.superclass = call.owner;
            class.super_ctor_params.clone_from(&call.params);
        }
        let parameters = &plan.header.contract.parameters;
        for (formal, bounds) in parameters.formals.iter().zip(&parameters.bounds) {
            let crate::types::TyKind::TyParam(identity, _) = formal.kind() else {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "a materialized class parameter has no declared formal identity".to_owned(),
                ));
            };
            let identity = identity.as_str().to_owned();
            class.type_params.push(identity.clone());
            class
                .type_param_bounds
                .extend(bounds.iter().map(|bound| (identity.clone(), *bound)));
        }
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
        // A class without a primary constructor is built by its secondary constructors alone.
        class.has_primary_ctor = plan.primary;
        class.ctor_args = plan
            .header
            .function
            .regular_parameters
            .iter()
            .filter(|_| plan.primary)
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
        Ok(class)
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
                declaring: &header.class.base.symbol,
                fields: vec![field],
                properties: super::super::class_storage::backed_properties(&header.class.members),
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
        // A box's construction stores its value, which the value was validated as when it was
        // made: the class runs no initialization of its own.
        class.ctor_args = vec![IrCtorArg {
            name: Some(class.fields[0].name.clone()),
            context_kind: crate::types::ContextParameterKind::None,
            ty: stored.ty,
            declared_ty: Some(stored.ty),
            is_field: true,
            field_index: Some(0),
            has_default: false,
            is_vararg: false,
            type_param: None,
            check: None,
            anonymous_super_forward: None,
            capture: None,
            provenance: IrCtorParameterProvenance::Value,
            capture_identity: None,
        }];
        let self_type = super::super::klib_types::semantic_type(
            header.arena,
            &header.types,
            header
                .class
                .this_receiver
                .as_ref()
                .ok_or_else(|| {
                    KlibBodyDeclineReason::SignatureMismatch(
                        "a value class header has no receiver".to_owned(),
                    )
                })?
                .ty,
        )?;
        let class = ir.add_class(class);
        ir.record_declared_class_self_type(classifier, self_type);
        self.classes.insert(identity, class);
        let member = super::MemberClass {
            class,
            declaring: &header.class.base.symbol,
            fields: vec![field],
            properties: super::super::class_storage::backed_properties(&header.class.members),
            contract,
        };
        // A boxed instance dispatches to the class's overrides, as an ordinary class's does: each
        // one the program's closure selected, with every declaration it overrides. An override
        // left out keeps its slot unrealized, which a call reaching it declines on.
        let realized_earlier = self.realized_classes.contains(&classifier);
        for function in value_class_overrides(&header) {
            if self.selects_override(&contract.module, function) {
                self.declare_dispatched(ir, member.clone(), header.arena, function)?;
            }
        }
        // The unit materializing a value class owns its box construction, exported under the
        // primary constructor's identity: a later unit, or a body common preparation adds to this
        // one, constructs a box through that export instead of materializing the class again.
        if let Some(primary) = contract
            .primary_constructor
            .as_ref()
            .filter(|_| !realized_earlier)
        {
            let constructor = KlibDependencyCallable {
                module: contract.module.clone(),
                declaration: KlibDeclarationSignature::Public(primary.clone()),
            };
            if let std::collections::hash_map::Entry::Vacant(slot) =
                self.functions.entry(constructor)
            {
                let entry = ir
                    .add_construction_entry(class, None)
                    .map(|entry| entry.function)
                    .ok_or(KlibBodyDeclineReason::SignatureMismatch(
                        "a value class box without its checked self type".to_owned(),
                    ))?;
                slot.insert(entry);
            }
        }
        Ok(member)
    }

    /// The entry constructing a box of the value class whose primary constructor `selected` is.
    /// A box runs no initialization: the class this materializes stores the constructor's value,
    /// and the entry is exported under the constructor's identity as a function of the unit.
    pub(super) fn materialize_value_constructor(
        &mut self,
        ir: &mut IrFile,
        selected: KlibBodyCallable<'s>,
    ) -> Result<FunId, KlibBodyDeclineReason> {
        let member = self.materialize_member_class(ir, selected)?;
        let primary = member
            .contract
            .primary_constructor
            .as_ref()
            .map(|primary| KlibDeclarationSignature::Public(primary.clone()));
        if primary.as_ref() != Some(&selected.identity().declaration) {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a value class construction other than through its primary constructor".to_owned(),
            ));
        }
        if let Some(&entry) = self.functions.get(selected.identity()) {
            return Ok(entry);
        }
        let entry = ir
            .add_construction_entry(member.class, None)
            .map(|entry| entry.function)
            .ok_or(KlibBodyDeclineReason::SignatureMismatch(
                "a value class box without its checked self type".to_owned(),
            ))?;
        self.functions.insert(selected.identity().clone(), entry);
        Ok(entry)
    }

    /// The class a member of the final ordinary class `header` declares belongs to: the class
    /// the constructors that initialize it materialize, which created every instance the member
    /// is called on.
    pub(super) fn materialize_ordinary_member_class(
        &mut self,
        ir: &mut IrFile,
        header: &super::super::class_headers::ClassMemberHeader<'s, 's>,
    ) -> Result<super::MemberClass<'s>, KlibBodyDeclineReason> {
        let contract = header.contract;
        let class = if self.adds_members_to_realized_classes
            && contract.kind != crate::libraries::TypeKind::Enum
            && self.realized_classes.contains(&contract.classifier)
        {
            // The class's one owner is an earlier unit: this unit adds a final member of it, so
            // it declares only the facts that member reads.
            self.declare_earlier_unit_class(ir, header)?
        } else if contract.declares_instances() {
            // An object's instance, or an enum's constant, is one its own private constructor
            // created.
            let primary = contract.primary_constructor.as_ref().ok_or(
                KlibBodyDeclineReason::UnsupportedDeclaration(
                    "an object or enum member without a primary constructor",
                ),
            )?;
            let identity = KlibDependencyCallable {
                module: contract.module.clone(),
                declaration: KlibDeclarationSignature::Public(primary.clone()),
            };
            self.materialize_declared_instances(ir, &identity, contract)?;
            *self
                .classes
                .get(&identity)
                .ok_or(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "a member of an object or enum without admitted initialization",
                ))?
        } else {
            self.materialize_initializing_constructors(ir, contract)?
        };
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
            declaring: &header.class.base.symbol,
            fields,
            properties: super::super::class_storage::backed_properties(&header.class.members),
            contract,
        })
    }

    /// The class an earlier unit owns, declared here for the final members this unit adds to it:
    /// its fields, type parameters, supertypes and modality, with no constructor, initializer or
    /// dispatch table, which stay with the unit that owns it.
    fn declare_earlier_unit_class(
        &self,
        ir: &mut IrFile,
        header: &super::super::class_headers::ClassMemberHeader<'s, 's>,
    ) -> Result<crate::ir::ClassId, KlibBodyDeclineReason> {
        let contract = header.contract;
        let classifier = contract.classifier;
        if let Some(class) = ir.class_id_by_name(classifier) {
            return Ok(class);
        }
        let mut class = IrClass::synthetic(classifier);
        class.ownership = crate::ir::ClassOwnership::EarlierUnit;
        class.is_source_declared = true;
        class.is_object = contract.kind == crate::libraries::TypeKind::Object;
        class.supertypes = header
            .class
            .supertypes
            .iter()
            .map(|&supertype| {
                super::super::klib_types::semantic_type(header.arena, &header.types, supertype)
            })
            .collect::<Result<Vec<_>, _>>()?;
        set_materialized_modality(&mut class, contract.modality);
        class.declaration_name = Some(super::super::klib_types::classifier_declaration(
            &contract.signature,
        )?);
        let parameters = &contract.parameters;
        for (formal, bounds) in parameters.formals.iter().zip(&parameters.bounds) {
            let crate::types::TyKind::TyParam(identity, _) = formal.kind() else {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "a class parameter has no declared formal identity".to_owned(),
                ));
            };
            let identity = identity.as_str().to_owned();
            class.type_params.push(identity.clone());
            class
                .type_param_bounds
                .extend(bounds.iter().map(|bound| (identity.clone(), *bound)));
        }
        let mut identities = HashSet::default();
        class.fields = header
            .class
            .members
            .iter()
            .filter_map(super::super::class_storage::member_field)
            .map(|field| {
                super::super::class_storage::declared_field(header, field, &mut identities).map(
                    |stored| {
                        IrField::new(stored.name.clone(), stored.ty)
                            .with_is_final(stored.final_)
                            .with_is_private(stored.private)
                    },
                )
            })
            .collect::<Result<_, _>>()?;
        let self_type = contract.self_type();
        let class = ir.add_class(class);
        ir.record_declared_class_self_type(classifier, self_type);
        Ok(class)
    }

    /// The class every public initializing constructor of the ordinary class `contract`
    /// materializes.
    fn materialize_initializing_constructors(
        &mut self,
        ir: &mut IrFile,
        contract: &'s crate::libraries::KlibBodyClassContract,
    ) -> Result<crate::ir::ClassId, KlibBodyDeclineReason> {
        let constructors = super::super::class_initialization::initializing_constructors(
            contract,
            self.sources.bodies,
        )
        .filter(|constructors| !constructors.is_empty())
        .ok_or(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a class member without public initializing constructors",
        ))?;
        let mut class = None;
        for identity in &constructors {
            let constructor = match self.sources.callees.get(identity) {
                Some(KlibCalleeFact::Selected(callables)) => callables[0],
                _ => {
                    return Err(KlibBodyDeclineReason::SignatureMismatch(
                        "a class member's constructor has no unique provider declaration"
                            .to_owned(),
                    ))
                }
            };
            self.materialize_class(ir, identity, constructor)?;
            let materialized = *self.classes.get(identity).ok_or(
                KlibBodyDeclineReason::UnsupportedDeclaration(
                    "a member of a class without ordinary initialization",
                ),
            )?;
            if class.is_some_and(|class| class != materialized) {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "a class's constructors materialize different classes".to_owned(),
                ));
            }
            class = Some(materialized);
        }
        Ok(class.expect("a class has an initializing constructor"))
    }

    /// Whether this unit emits the override `function` of a class of `module`: the program's
    /// closure selected it and every declaration it overrides, each by one exact provider
    /// declaration, and no earlier unit already exported it. An override an earlier unit emitted
    /// is a retained export, which the closure does not walk again, so lowering its body here
    /// would reach declarations this unit never selected.
    fn selects_override(&self, module: &str, function: &KlibIrFunction) -> bool {
        let selected = |identity: &KlibDependencyCallable| {
            matches!(
                self.sources.callees.get(identity),
                Some(KlibCalleeFact::Selected(_))
            ) && self.sources.callees.reference_target(identity).is_some()
        };
        let Some(signature) =
            crate::klib_libraries::member_signature(&function.base.symbol.signature)
        else {
            return false;
        };
        let KlibCalleeOrigin::Exact(identity) =
            self.sources.bodies.linked_origin(module, &signature)
        else {
            return false;
        };
        !self.sources.externals.contains_key(&identity)
            && selected(&identity)
            && function.overridden.iter().all(|symbol| {
                crate::klib_libraries::member_signature(&symbol.signature)
                    .and_then(|signature| {
                        self.sources
                            .bodies
                            .overridden_closure(module, &signature)
                            .ok()
                    })
                    .is_some_and(|declarations| declarations.iter().all(selected))
            })
    }

    /// Record, for each member the materialized class `class` inherits without overriding it,
    /// the interface declarations it implements with the inherited body: `AbstractList`
    /// implements `List.contains` with `AbstractCollection.contains`. The edge names the
    /// inherited method this unit already declared, so dispatch through the interface reaches
    /// the receiver's most derived override of it.
    fn declare_inherited_implementations(
        &mut self,
        ir: &mut IrFile,
        class: crate::ir::ClassId,
        contract: &crate::libraries::KlibBodyClassContract,
        arena: &KlibIrArena,
        declaration: &crate::metadata::klib_ir::tree::KlibIrClass,
    ) -> Result<(), KlibBodyDeclineReason> {
        let module = &contract.module;
        let owner = ir.classes[class as usize].fq_name;
        let inherited = self
            .sources
            .bodies
            .inherited_implementations(module, arena, declaration)
            .map_err(|reason| KlibBodyDeclineReason::SignatureMismatch(reason.to_owned()))?;
        for (body, implemented) in &inherited {
            // The inherited body belongs to a superclass this unit materialized; its member may
            // not be declared yet when no body calls it (`Itr.remove` under `KeysItr`).
            let from_class = matches!(
                self.sources.bodies.member_owner(body),
                crate::klib_libraries::LocatedMember::One { class, .. }
                    if crate::metadata::semantic::class_kind(class.base.flags)
                        == crate::libraries::TypeKind::Class
            );
            // An interface default that implements only itself is carried by the class too.
            let implements_itself = contract.modality
                == crate::metadata::semantic::KotlinModality::Final
                && implemented.as_slice() == std::slice::from_ref(body);
            if (from_class || implements_itself) && self.declared_member(body).is_none() {
                if let Some(KlibCalleeFact::Selected(callables)) = self.sources.callees.get(body) {
                    let selected = callables[0];
                    // A superclass an earlier unit realized keeps its members there: this unit's
                    // class inherits them from that class, and the edge names the earlier unit's
                    // member by its frozen declaration.
                    if self.class_realized_earlier(selected) {
                        self.record_inherited_implementation(
                            ir,
                            owner,
                            body,
                            Some(selected),
                            implemented,
                        )?;
                        continue;
                    }
                    let (arena, function) =
                        self.sources.bodies.function(body).ok_or_else(|| {
                            KlibBodyDeclineReason::SignatureMismatch(
                                "an inherited body has no serialized declaration".to_owned(),
                            )
                        })?;
                    self.declare(ir, body, Some(selected), arena, function)?;
                }
            }
            self.record_inherited_implementation(ir, owner, body, None, implemented)?;
        }
        Ok(())
    }

    /// Record that the class `owner`, private to its library, implements `implemented` with the
    /// inherited member `body`, declaring `body` first when this unit has not: no other unit
    /// exports the class, so an interface default only dispatch reaches is declared here.
    pub(super) fn declare_inherited_implementation(
        &mut self,
        ir: &mut IrFile,
        owner: crate::types::TypeName,
        body: &KlibDependencyCallable,
        implemented: &[KlibDependencyCallable],
    ) -> Result<(), KlibBodyDeclineReason> {
        if self.declared_member(body).is_none() {
            // The class carries the body however dispatch reaches it, so a body the closure did
            // not select uniquely declines rather than leaving the slot without an implementation.
            let Some(KlibCalleeFact::Selected(callables)) = self.sources.callees.get(body) else {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "an inherited implementation has no unique provider declaration".to_owned(),
                ));
            };
            let selected = callables[0];
            let (arena, function) = self.sources.bodies.function(body).ok_or_else(|| {
                KlibBodyDeclineReason::SignatureMismatch(
                    "an inherited body has no serialized declaration".to_owned(),
                )
            })?;
            self.declare(ir, body, Some(selected), arena, function)?;
        }
        self.record_inherited_implementation(ir, owner, body, None, implemented)
    }

    /// The method this unit declared for the member `body`: a member of a class it materialized,
    /// or of a same-library class it installed.
    fn declared_member(&self, body: &KlibDependencyCallable) -> Option<FunId> {
        if let Some(&function) = self.functions.get(body) {
            return Some(function);
        }
        let signature = match &body.declaration {
            KlibDeclarationSignature::Public(signature) => {
                KlibIrSignature::Public(signature.clone())
            }
            KlibDeclarationSignature::Accessor(signature) => {
                KlibIrSignature::Accessor(signature.clone())
            }
            KlibDeclarationSignature::FileLocal { file, slot } => KlibIrSignature::FileLocal {
                file: *file,
                slot: *slot,
            },
        };
        self.local_functions
            .get(&super::super::same_library_class::LocalIdentity {
                module: body.module.clone(),
                signature,
            })
            .map(|local| local.function)
    }

    /// Record that `owner` implements each interface declaration of `implemented` with the
    /// inherited member `body`, unless an edge already names that declaration. A body this unit
    /// did not declare, or a declaration with no registered provider handle, records nothing.
    pub(super) fn record_inherited_implementation(
        &self,
        ir: &mut IrFile,
        owner: crate::types::TypeName,
        body: &KlibDependencyCallable,
        realized: Option<KlibBodyCallable<'s>>,
        implemented: &[KlibDependencyCallable],
    ) -> Result<(), KlibBodyDeclineReason> {
        // The implementation's own method when this unit declared it, and otherwise the
        // declaration the earlier unit exported (`realized`), which the edge names by callable.
        let (method, name, params, result) = match realized {
            None => {
                let Some(method) = self.declared_member(body) else {
                    return Ok(());
                };
                let function = &ir.functions[method as usize];
                (
                    Some(method),
                    function.name.clone(),
                    function.params.clone(),
                    function.ret,
                )
            }
            Some(selected) => (
                None,
                selected.name().to_owned(),
                selected.params().to_vec(),
                selected.ret(),
            ),
        };
        for declared in implemented {
            let Some(KlibCalleeFact::Selected(callables)) = self.sources.callees.get(declared)
            else {
                continue;
            };
            let overridden_declaration = callables[0];
            if !overridden_declaration
                .class_contract()
                .is_some_and(|class| class.kind == crate::libraries::TypeKind::Interface)
            {
                continue;
            }
            let Some(crate::types::SemanticCallableOwner::Classifier(interface)) =
                overridden_declaration.declaration_owner()
            else {
                continue;
            };
            // The declaration's provider handle, which a source call of it selects even when no
            // body of the closure calls it (`ListIterator.next` on a list iterator inheriting
            // `next` from its superclass), as an own override's edge names it.
            let target = match super::local_class::registered_override_handle(
                declared,
                self.sources.externals,
                self.sources.declarations,
            ) {
                Ok(handle) => handle.target(),
                Err(KlibBodyDeclineReason::UnsupportedDeclaration(_)) => continue,
                Err(reason) => return Err(reason),
            };
            // A member of a class private to its library has no provider handle of its own; the
            // edge then names the interface declaration it implements, as an installed class's
            // own override does.
            let implementation = self
                .sources
                .callees
                .reference_target(body)
                .unwrap_or(target);
            let overridden = crate::fir::ResolvedFunctionOverrideTarget::External(target);
            let records = ir.function_overrides.entry(owner).or_default();
            if records.iter().any(|edge| edge.overridden == overridden) {
                continue;
            }
            records.push(crate::ir::IrFunctionOverride {
                implementation: crate::fir::ResolvedFunctionOverrideTarget::External(
                    implementation,
                ),
                implementation_function: method,
                implementation_owner: owner,
                overridden,
                overridden_owner: interface,
                overridden_semantic_role: overridden_declaration.semantic_role(),
                collection_barrier: None,
                overridden_is_interface: true,
                name: name.clone(),
                declared_parameters: overridden_declaration.params().to_vec(),
                declared_result: overridden_declaration.ret(),
                applied_parameters: overridden_declaration.params().to_vec(),
                applied_result: overridden_declaration.ret(),
                implementation_parameters: params.clone(),
                implementation_parameter_identities: Vec::new(),
                overridden_parameter_identities: overridden_declaration
                    .parameter_identities()
                    .to_vec(),
                implementation_result: result,
                suspend: false,
                has_kotlin_superclass_override: false,
                depth: 0,
            });
        }
        Ok(())
    }

    /// Declare `function`, a member dispatch on an instance of the materialized class `member`
    /// reaches, as a method of the class, with the override edges a backend dispatches through.
    /// The class always carries it: a call site may select it only through what it overrides.
    fn declare_dispatched(
        &mut self,
        ir: &mut IrFile,
        member: super::MemberClass<'s>,
        arena: &'s KlibIrArena,
        function: &'s KlibIrFunction,
    ) -> Result<(), KlibBodyDeclineReason> {
        let module = &member.contract.module;
        let member_signature = |symbol: &crate::metadata::klib_ir::KlibIrSymbol| {
            crate::klib_libraries::member_signature(&symbol.signature).ok_or_else(|| {
                KlibBodyDeclineReason::SignatureMismatch(
                    "an override names a declaration that is not a member".to_owned(),
                )
            })
        };
        let exact = |symbol: &crate::metadata::klib_ir::KlibIrSymbol| match self
            .sources
            .bodies
            .linked_origin(module, &member_signature(symbol)?)
        {
            KlibCalleeOrigin::Exact(identity) => Ok(identity),
            KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => {
                Err(KlibBodyDeclineReason::SignatureMismatch(
                    "an override names a declaration without one exact producer".to_owned(),
                ))
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
        let overridden = function
            .overridden
            .iter()
            .map(|symbol| {
                self.sources
                    .bodies
                    .overridden_closure(module, &member_signature(symbol)?)
                    .map_err(|reason| KlibBodyDeclineReason::SignatureMismatch(reason.to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for overridden in overridden.iter().flatten() {
            let (declaration, target) = provided(overridden)?;
            let Some(crate::types::SemanticCallableOwner::Classifier(owner)) =
                declaration.declaration_owner()
            else {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "an overridden declaration is not a class member".to_owned(),
                ));
            };
            let interface = declaration
                .class_contract()
                .is_some_and(|class| class.kind == crate::libraries::TypeKind::Interface);
            edges.push((declaration, target, owner, interface));
        }
        let header = body_header(Some(selected), arena, function, self.sources.bodies)?;
        if header.value_receiver().is_none() {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a materialized class's dispatched member has no receiver".to_owned(),
            ));
        }
        let class = member.class;
        let realized_earlier = self.realized_classes.contains(&member.contract.classifier);
        let declared = if function.body.is_some() {
            self.queue(
                ir,
                &identity,
                Some(selected),
                arena,
                function,
                super::BodyHeader {
                    header,
                    value_member: Some(member),
                },
            )?
        } else {
            // An abstract member is a slot without an implementation.
            let declared = declare_function(ir, &header);
            self.functions.insert(identity.clone(), declared);
            if let Some(receiver) = header.value_receiver() {
                self.member_receivers.insert(identity.clone(), receiver);
            }
            declared
        };
        ir.dependency_methods.insert(implementation, declared);
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
        for (declaration, target, owner, interface) in edges {
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
                overridden_is_interface: interface,
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
        if realized_earlier {
            // An earlier unit published this member; this unit's copy only completes the class's
            // table and is called through that export.
            self.functions.remove(&identity);
            self.member_receivers.remove(&identity);
        }
        Ok(())
    }
}

/// The members of the value class `header` with bodies that override another declaration: the
/// ones dispatch reaches on a boxed instance of the class.
fn value_class_overrides<'s>(
    header: &super::super::class_headers::ClassMemberHeader<'s, 's>,
) -> Vec<&'s KlibIrFunction> {
    let arena = header.arena;
    header
        .class
        .members
        .iter()
        .flat_map(|member| match member {
            crate::metadata::klib_ir::tree::KlibIrMember::Function(function) => vec![*function],
            crate::metadata::klib_ir::tree::KlibIrMember::Property(property) => {
                [property.getter, property.setter]
                    .into_iter()
                    .flatten()
                    .collect()
            }
            _ => Vec::new(),
        })
        .map(|function| arena.function(function))
        .filter(|function| {
            !function.constructor && function.body.is_some() && !function.overridden.is_empty()
        })
        .collect()
}

/// The constructor a secondary constructor of a class without a primary one delegates to: its
/// superclass's selected constructor, `Any`'s when it names none.
fn secondary_delegation(
    ir: &IrFile,
    plan: &super::super::class_initialization::ClassInitialization<'_, '_>,
    superclass: Option<crate::ir::ClassId>,
) -> Result<crate::ir::CtorDelegateTarget, KlibBodyDeclineReason> {
    let Some(call) = &plan.superclass else {
        return Ok(crate::ir::CtorDelegateTarget::Super {
            owner: crate::types::wk::any(),
            target_params: Vec::new(),
            target: crate::ir::IrConstructorTarget::UNRESTRICTED_PRIMARY,
            default_masks: Vec::new(),
        });
    };
    let ordinal = superclass_constructor_ordinal(
        superclass.map(|class| &ir.classes[class as usize]),
        &call.params,
    )?;
    Ok(crate::ir::CtorDelegateTarget::Super {
        owner: call.owner,
        target_params: call.params.clone(),
        target: crate::ir::IrConstructorTarget {
            ordinal,
            access: crate::ir::IrConstructorAccess::Unrestricted,
        },
        default_masks: Vec::new(),
    })
}

/// The secondary constructor `plan` initializes its class through: it delegates to `delegate`,
/// whose arguments its initialization lowers, then runs `initialize`, the class's own field and
/// initializer effects. It is a realization of a library declaration, which publishes its own
/// Kotlin metadata, so it records none here.
fn materialized_secondary(
    declaration: &super::super::class_headers::ClassMemberHeader<'_, '_>,
    header: &super::super::function_lowering::FunctionHeader,
    delegate: crate::ir::CtorDelegateTarget,
    initialize: crate::ir::ExprId,
) -> crate::ir::IrSecondaryCtor {
    let params = header.params().to_vec();
    crate::ir::IrSecondaryCtor {
        annotations: crate::ir::DeclarationAnnotations::default(),
        source_order: u32::MAX,
        lines: crate::ir::IrSecondaryCtorLines::default(),
        prefix_params: Vec::new(),
        declared_spellings: Default::default(),
        named_params: declaration
            .function
            .regular_parameters
            .iter()
            .zip(&params)
            .map(|(parameter, ty)| (parameter.name.clone(), *ty))
            .collect(),
        metadata_visibility: None,
        generated_debug: crate::ir::IrGeneratedDeclarationDebug::None,
        vararg_index: None,
        defaults: vec![None; params.len()],
        delegate_prelude: Vec::new(),
        delegate_args: Vec::new(),
        default_parameters: Vec::new(),
        body: Some(initialize),
        delegate,
        synthetic: false,
        platform_visible: false,
        vc_params: false,
        param_checks: vec![None; params.len()],
        params,
    }
}

/// The ordinal of the constructor a delegation with `params` reaches in a materialized superclass.
/// A superclass without a primary constructor is reached through the secondary one declaring the
/// selected parameters; Kotlin declares no two constructors of a class with the same ones.
pub(crate) fn superclass_constructor_ordinal(
    superclass: Option<&IrClass>,
    params: &[Ty],
) -> Result<u32, KlibBodyDeclineReason> {
    match superclass {
        Some(parent) if !parent.has_primary_ctor => parent
            .secondary_ctors
            .iter()
            .position(|secondary| secondary.params == params)
            .map(|index| u32::try_from(index + 1).expect("a constructor ordinal fits u32"))
            .ok_or_else(|| {
                KlibBodyDeclineReason::SignatureMismatch(
                    "a superclass has no constructor with the delegation's parameters".to_owned(),
                )
            }),
        _ => Ok(0),
    }
}

fn set_materialized_modality(
    class: &mut IrClass,
    modality: crate::metadata::semantic::KotlinModality,
) {
    class.is_open = modality != crate::metadata::semantic::KotlinModality::Final;
    class.is_abstract = modality.is_abstract();
    class.is_sealed = modality == crate::metadata::semantic::KotlinModality::Sealed;
}

#[cfg(test)]
mod modality_tests {
    use super::*;

    #[test]
    fn sealed_materialization_retains_initializer_inventory_without_allocation() {
        let identity = KlibDependencyCallable {
            module: "producer".into(),
            declaration: KlibDeclarationSignature::Public(KlibPublicIdSignature::classifier(
                crate::types::type_name("sample/Sealed"),
            )),
        };
        let mut unit = DependencyBodyUnit::default();
        let mut class = IrClass::synthetic(crate::types::type_name("sample/Sealed"));
        set_materialized_modality(
            &mut class,
            crate::metadata::semantic::KotlinModality::Sealed,
        );
        assert!(class.is_sealed);
        unit.ir.classes.push(class);
        unit.classes.insert(identity.clone(), 0);
        unit.constructors.insert(
            identity.clone(),
            MaterializedConstructor {
                initializer: 0,
                secondary: None,
            },
        );
        assert_eq!(
            unit.initializing_constructors()
                .map(|(identity, _)| identity)
                .collect::<Vec<_>>(),
            vec![&identity]
        );
        assert!(unit.allocating_constructors().next().is_none());
        unit.ir.classes[0].is_abstract = false;
        assert!(unit.allocating_constructors().next().is_none());
        set_materialized_modality(
            &mut unit.ir.classes[0],
            crate::metadata::semantic::KotlinModality::Open,
        );
        assert_eq!(unit.allocating_constructors().count(), 1);
    }
}

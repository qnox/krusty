//! Materialized dependency storage and its private initialization frame in this closed unit.

use super::*;
use crate::fx_hash::HashSet;
use crate::ir::{IrClass, IrCtorArg, IrCtorParameterProvenance, IrExpr, IrField};

impl<'u, 's> Linker<'u, 's> {
    fn dependency_identity(
        &self,
        callable: crate::fir::ExternalCallableId,
    ) -> Result<KlibDependencyCallable, KlibBodyDeclineReason> {
        self.sources
            .declarations
            .external_callable(callable)
            .and_then(|realization| realization.dependency_callable)
            .ok_or_else(|| {
                KlibBodyDeclineReason::SignatureMismatch(
                    "a materialized class member has no exact KLIB dependency identity".to_owned(),
                )
            })
    }

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
        // A materialized superclass is materialized first, from its own admitted initialization.
        let superclass = match &plan.superclass {
            Some(call) if call.materialized => {
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
            secondaries.push(materialized_secondary(&plan, &header, delegate, initialize));
            Some(secondaries.len() - 1)
        };
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
            fields: plan.field_declarations.clone(),
            contract: plan.header.contract,
        };
        if existing.is_none() {
            for function in &plan.dispatched {
                self.declare_dispatched(ir, member.clone(), plan.header.arena, function)?;
            }
        }
        // The initialization reads the class's private members as its members do.
        self.pending.push_back(PendingBody {
            identity: identity.clone(),
            arena: plan.header.arena,
            symbol: &plan.header.function.base.symbol,
            statements: &[],
            header,
            function,
            initialization: Some((class, plan)),
            value_member: Some(member),
            local_body: None,
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
        class.supertypes.clone_from(&plan.supertypes);
        class.is_open =
            plan.header.contract.modality != crate::metadata::semantic::KotlinModality::Final;
        class.is_abstract =
            plan.header.contract.modality == crate::metadata::semantic::KotlinModality::Abstract;
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
                    .overridden_declarations(module, &member_signature(symbol)?)
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
        let class_owner = member.contract.classifier;
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
        let accessor = selected.accessor().cloned();
        let property_getter = if let Some(accessor) = &accessor {
            let property = self
                .sources
                .declarations
                .external_property(accessor.property)
                .ok_or_else(|| {
                    KlibBodyDeclineReason::SignatureMismatch(
                        "a dispatched property accessor has no exact property realization"
                            .to_owned(),
                    )
                })?;
            let role_matches = match accessor.role {
                crate::libraries::KlibAccessorRole::Getter => property.getter == implementation,
                crate::libraries::KlibAccessorRole::Setter => {
                    property.setter == Some(implementation)
                }
            };
            if !role_matches {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "a dispatched property accessor changed its exact getter or setter identity"
                        .to_owned(),
                ));
            }
            Some(self.dependency_identity(property.getter)?)
        } else {
            None
        };
        let property_setter = if let Some(accessor) = &accessor {
            let property = self
                .sources
                .declarations
                .external_property(accessor.property)
                .expect("the exact property was validated above");
            property
                .setter
                .map(|setter| self.dependency_identity(setter))
                .transpose()?
        } else {
            None
        };
        let property_overrides = if let Some(implementation_accessor) = &accessor {
            let implementation_role = implementation_accessor.role;
            edges
                .iter()
                .filter_map(|(declaration, target, owner, interface)| {
                    let accessor = declaration.accessor()?;
                    (accessor.role == implementation_role).then_some((
                        declaration,
                        *target,
                        *owner,
                        *interface,
                        accessor.property,
                    ))
                })
                .map(|(_declaration, target, owner, interface, property)| {
                    let realization = self
                        .sources
                        .declarations
                        .external_property(property)
                        .ok_or_else(|| {
                            KlibBodyDeclineReason::SignatureMismatch(
                                "an overridden property getter has no exact property realization"
                                    .to_owned(),
                            )
                        })?;
                    let role_matches = match implementation_role {
                        crate::libraries::KlibAccessorRole::Getter => realization.getter == target,
                        crate::libraries::KlibAccessorRole::Setter => {
                            realization.setter == Some(target)
                        }
                    };
                    if !role_matches {
                        return Err(KlibBodyDeclineReason::SignatureMismatch(
                            "an overridden property accessor changed its exact identity".to_owned(),
                        ));
                    }
                    let overridden = self.dependency_identity(realization.getter)?;
                    Ok(super::MaterializedClassPropertyOverride {
                        overridden,
                        overridden_owner: owner,
                        overridden_is_interface: interface,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let function_overrides = edges
            .iter()
            .filter(|(declaration, _, _, _)| declaration.accessor().is_none())
            .map(
                |(declaration, _, owner, interface)| super::MaterializedClassFunctionOverride {
                    implementation: identity.clone(),
                    overridden: declaration.identity().clone(),
                    overridden_owner: *owner,
                    overridden_is_interface: *interface,
                    overridden_role: declaration.semantic_role(),
                },
            )
            .collect();
        self.materialized_class_members
            .push(super::MaterializedClassMember {
                owner: class_owner,
                target: identity.clone(),
                accessor: accessor.map(|accessor| accessor.role),
                property_getter,
                property_setter,
                function_overrides,
                property_overrides,
            });
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
                suspend: selected
                    .declaration_modifiers()
                    .is_some_and(|modifiers| modifiers.is_suspend),
                has_kotlin_superclass_override: false,
                depth: 0,
            });
        }
        Ok(())
    }
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
    plan: &super::super::class_initialization::ClassInitialization<'_, '_>,
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
        named_params: plan
            .header
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
pub(in crate::klib_lowering) fn superclass_constructor_ordinal(
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

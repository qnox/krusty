//! Installing a same-library class into the unit that lowers its bodies, and linking calls of
//! the members it installs.

use super::*;
use crate::ir::{IrExpr, IrFile};
use crate::metadata::klib_ir::KlibIrSignature;

impl<'s> Linker<'_, 's> {
    pub(super) fn install_local_class(
        &mut self,
        ir: &mut IrFile,
        prepared: Box<super::super::same_library_class::PreparedLocalClass<'s>>,
    ) -> Result<super::super::same_library_class::LocalConstructor, KlibBodyDeclineReason> {
        self.install_class(ir, prepared, None)
    }

    /// Install `prepared` in this unit. A public class's `constructor` publishes its allocation,
    /// and each of its public members is published under its own identity, so a caller outside
    /// the unit names them as it names any other emitted dependency declaration.
    pub(super) fn install_class(
        &mut self,
        ir: &mut IrFile,
        prepared: Box<super::super::same_library_class::PreparedLocalClass<'s>>,
        constructor: Option<&KlibDependencyCallable>,
    ) -> Result<super::super::same_library_class::LocalConstructor, KlibBodyDeclineReason> {
        if ir.class_id_by_name(prepared.classifier).is_some() {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "different dependency identities claim the same materialized classifier".to_owned(),
            ));
        }
        // A same-library superclass is installed first, and the class's constructor passes it the
        // constructor parameters its delegation names.
        let superclass = match &prepared.superclass {
            Some(superclass) => {
                let declaration = match superclass.constructor {
                    super::super::same_library_class::SuperclassConstructor::Local(signature) => {
                        self.link_local_constructor(ir, signature)?.declaration
                    }
                    super::super::same_library_class::SuperclassConstructor::Library {
                        signature,
                        classifier,
                    } => {
                        // The library superclass is extended only where this unit materializes it.
                        let linked = self.link_constructor(
                            ir,
                            signature,
                            super::ConstructorRole::Delegation,
                        )?;
                        if linked.declaration.classifier != classifier
                            || !self
                                .classes
                                .values()
                                .any(|class| ir.classes[*class as usize].fq_name_id() == classifier)
                        {
                            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                                "a superclass this unit does not materialize",
                            ));
                        }
                        linked.declaration
                    }
                };
                let params = declaration.declared_parameters().to_vec();
                if params.len() != superclass.arguments.len() {
                    return Err(KlibBodyDeclineReason::SignatureMismatch(
                        "a superclass delegation passes another number of arguments".to_owned(),
                    ));
                }
                Some((declaration.classifier, params))
            }
            None => None,
        };
        // The class's own library's non-public interfaces are installed before it, so its members
        // name the members they override.
        for interface in &prepared.local_interfaces {
            self.install_local_interface(ir, interface)?;
        }
        let mut class = super::super::same_library_class::installed_class(&prepared);
        // The initializer lowers the arguments the constructor passes, as its body reads them.
        if let Some((name, params)) = superclass {
            class.superclass = name;
            class.super_ctor_params = params;
        }
        let class_id = ir.add_class(class);
        if let Some(naming) = prepared.naming.clone() {
            ir.local_class_name_provenance.insert(class_id, naming);
        }
        ir.record_declared_class_self_type(prepared.classifier, prepared.self_type);
        let init = declare_function(ir, &prepared.init_header);
        ir.set_method_visibility(init, crate::types::Visibility::Private);
        ir.classes[class_id as usize].methods.push(init);
        ir.note_class_method(class_id, init);
        let receiver = ir.add_expr(IrExpr::GetValue(0));
        ir.set_logical_type(receiver, prepared.self_type);
        // The initialization takes the constructor's parameters, an outer instance among them,
        // at the slots the constructor numbers them.
        let args = prepared
            .init_header
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
            class: class_id,
            index: 0,
            receiver,
            args,
        });
        ir.set_logical_type(initialize, Ty::Unit);
        ir.classes[class_id as usize].init_body = Some(initialize);
        let constructor_signature = prepared.constructor_symbol.signature.clone();
        let identity = self.active_identity.clone();
        self.pending.push_back(PendingBody {
            identity: identity.clone(),
            arena: prepared.arena,
            symbol: prepared.constructor_symbol,
            statements: &[],
            header: prepared.init_header,
            function: init,
            initialization: None,
            value_member: None,
            local_body: Some(super::super::same_library_class::LocalBody::Initialize(
                super::super::same_library_class::LocalInitialize {
                    class: class_id,
                    declaring: prepared.declaring,
                    receiver_symbol: prepared.receiver_symbol,
                    receiver_ty: prepared.receiver_ty,
                    fields: prepared.stored.clone(),
                    steps: prepared.steps,
                    outer: prepared.outer,
                    superclass_arguments: prepared
                        .superclass
                        .map(|superclass| superclass.arguments)
                        .unwrap_or_default(),
                },
            )),
            static_initialization: None,
        });
        if let Some(constructor) = constructor {
            self.classes.insert(constructor.clone(), class_id);
        }
        for member in prepared.members {
            let function = declare_function(ir, &member.header);
            let declaration = match &member.signature {
                KlibIrSignature::Public(signature) => {
                    Some(KlibDeclarationSignature::Public(signature.clone()))
                }
                KlibIrSignature::Accessor(signature) => {
                    Some(KlibDeclarationSignature::Accessor(signature.clone()))
                }
                _ => None,
            };
            // A private member is called only by its class's members in this unit; no provider
            // publishes it to a caller outside the class.
            let published = match (constructor, declaration) {
                (Some(_), Some(declaration))
                    if member.visibility != crate::types::Visibility::Private =>
                {
                    match self
                        .sources
                        .bodies
                        .linked_origin(&prepared.key.module, &declaration)
                    {
                        KlibCalleeOrigin::Exact(identity) => Some(identity),
                        KlibCalleeOrigin::Missing => None,
                        KlibCalleeOrigin::Ambiguous => {
                            return Err(KlibBodyDeclineReason::SignatureMismatch(
                                "two libraries serialize one class member".to_owned(),
                            ))
                        }
                    }
                }
                _ => None,
            };
            ir.set_method_visibility(
                function,
                if published.is_some() {
                    member.visibility
                } else {
                    crate::types::Visibility::Private
                },
            );
            if let Some(identity) = published {
                if ir.functions[function as usize].dispatch_receiver.is_some() {
                    self.member_receivers
                        .insert(identity.clone(), prepared.self_type);
                }
                self.functions.insert(identity, function);
            }
            ir.classes[class_id as usize].methods.push(function);
            ir.note_class_method(class_id, function);
            self.local_functions.insert(
                super::super::same_library_class::LocalIdentity {
                    module: prepared.key.module.clone(),
                    signature: member.signature.clone(),
                },
                LocalFunction {
                    function,
                    class: prepared.key.clone(),
                    formals: member.formals.clone(),
                },
            );
            if let Some(binding) = &member.runtime_binding {
                ir.runtime_bindings.insert(function, binding.clone());
            }
            let bodyless = matches!(
                member.runtime_binding,
                Some(crate::ir::RuntimeBinding::Entry(_))
            );
            if !bodyless {
                self.pending.push_back(PendingBody {
                    identity: identity.clone(),
                    arena: prepared.arena,
                    symbol: member.symbol,
                    statements: member.statements,
                    header: member.header,
                    function,
                    initialization: None,
                    value_member: None,
                    local_body: Some(super::super::same_library_class::LocalBody::Member(
                        super::super::same_library_class::LocalMemberFields {
                            class: class_id,
                            declaring: prepared.declaring,
                            receiver: prepared.self_type,
                            fields: prepared.stored.clone(),
                            outer: prepared.outer,
                        },
                    )),
                    static_initialization: None,
                });
            }
            publish_overrides(
                ir,
                prepared.classifier,
                function,
                &member.overrides,
                self.sources.externals,
                self.sources.declarations,
            )?;
            self.record_installed_overrides(
                ir,
                class_id,
                function,
                &prepared.key.module,
                &member.installed_overrides,
            )?;
        }
        // A member the class inherits from a superclass this unit installed or materialized
        // implements the interface declarations the class reaches only through its supertypes.
        let inherited = self
            .sources
            .bodies
            .inherited_implementations_of(&prepared.key.module, &prepared.key.signature)
            .map_err(|reason| KlibBodyDeclineReason::SignatureMismatch(reason.to_owned()))?;
        for (body, implemented) in &inherited {
            self.record_inherited_implementation(ir, prepared.classifier, body, None, implemented)?;
        }
        let constructor = super::super::same_library_class::LocalConstructor {
            declaration: prepared.declaration.clone(),
            access: prepared.access,
            object: prepared.object,
        };
        self.local_classes.insert(
            prepared.key,
            super::super::same_library_class::MaterializedLocalClass {
                class: class_id,
                classifier: prepared.classifier,
                construction: Some(super::super::same_library_class::LocalConstruction {
                    constructor: constructor_signature,
                    declaration: prepared.declaration,
                    access: prepared.access,
                    object: prepared.object,
                }),
                formals: prepared.formals,
                supertypes: prepared.supertypes,
                self_type: prepared.self_type,
                class_type_parameters: prepared.class_type_parameters,
            },
        );
        Ok(constructor)
    }

    /// Record that `function`, a method of the installed class `class`, overrides each member of
    /// an installed interface of `module` that `overridden` names. The interface member and the
    /// override share one signature: a member whose signature differs would need a bridge, which
    /// an installed interface does not have.
    fn record_installed_overrides(
        &self,
        ir: &mut IrFile,
        class: crate::ir::ClassId,
        function: crate::ir::FunId,
        module: &str,
        overridden: &[KlibIrSignature],
    ) -> Result<(), KlibBodyDeclineReason> {
        for signature in overridden {
            let key = super::super::same_library_class::LocalIdentity {
                module: module.to_owned(),
                signature: signature.clone(),
            };
            let Some(member) = self.local_functions.get(&key) else {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "an overridden non-public interface member was not installed".to_owned(),
                ));
            };
            let declared = &ir.functions[member.function as usize];
            let implementation = &ir.functions[function as usize];
            if declared.params != implementation.params || declared.ret != implementation.ret {
                return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "an override of a non-public interface member with another signature",
                ));
            }
            let edge = crate::ir::IrInstalledOverride {
                implementation: function,
                overridden: member.function,
            };
            ir.classes[class as usize]
                .installed_interface_overrides
                .push(edge);
        }
        Ok(())
    }

    /// The constructor a file-local construction invokes: one of a class already installed for
    /// this library, or the constructor of a class prepared and installed now.
    pub(super) fn link_constructor_of_local_class(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibIrSignature,
    ) -> Result<super::super::same_library_class::LocalConstructor, KlibBodyDeclineReason> {
        let module = self.active_module.clone();
        let bodies = self.sources.bodies;
        let callees = self.sources.callees;
        match super::super::same_library_class::link(
            super::super::same_library_class::LocalClassSource {
                bodies,
                declarations: self.sources.declarations,
                classes: callees.class_type_oracle(),
                module: &module,
                validate_root: &|root| callees.validate_root_constructor(&module, root, bodies),
            },
            callee,
            self.local_classes,
        )? {
            super::super::same_library_class::LocalLink::Ready(constructor) => Ok(constructor),
            super::super::same_library_class::LocalLink::Prepared(prepared) => {
                self.install_local_class(ir, prepared)
            }
        }
    }

    /// Whether `signature` names a non-public interface of this library.
    pub(super) fn names_local_interface(&self, signature: &KlibIrSignature) -> bool {
        self.sources
            .bodies
            .module_local_class(&self.active_module, signature)
            .is_some_and(|(_, class)| {
                crate::metadata::semantic::class_kind(class.base.flags)
                    == crate::libraries::TypeKind::Interface
            })
    }

    /// Install the non-public interface `signature` of this library in the unit, with its abstract
    /// members declared, so a type test or a member call of it has a class to name.
    pub(super) fn install_local_interface(
        &mut self,
        ir: &mut IrFile,
        signature: &KlibIrSignature,
    ) -> Result<(), KlibBodyDeclineReason> {
        let module = self.active_module.clone();
        let bodies = self.sources.bodies;
        let callees = self.sources.callees;
        match super::super::same_library_class::link_interface(
            super::super::same_library_class::LocalClassSource {
                bodies,
                declarations: self.sources.declarations,
                classes: callees.class_type_oracle(),
                module: &module,
                validate_root: &|root| callees.validate_root_constructor(&module, root, bodies),
            },
            signature,
            self.local_classes,
        )? {
            super::super::same_library_class::InterfaceLink::Ready => Ok(()),
            super::super::same_library_class::InterfaceLink::Prepared(prepared) => {
                self.install_interface(ir, prepared)
            }
        }
    }

    fn install_interface(
        &mut self,
        ir: &mut IrFile,
        prepared: Box<super::super::same_library_class::PreparedLocalInterface<'s>>,
    ) -> Result<(), KlibBodyDeclineReason> {
        if ir.class_id_by_name(prepared.classifier).is_some() {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "different dependency identities claim the same materialized classifier".to_owned(),
            ));
        }
        let class = super::super::same_library_class::installed_interface(&prepared);
        let class_id = ir.add_class(class);
        ir.record_declared_class_self_type(prepared.classifier, prepared.self_type);
        for member in prepared.members {
            let function = declare_function(ir, &member.header);
            ir.classes[class_id as usize].methods.push(function);
            ir.note_class_method(class_id, function);
            self.local_functions.insert(
                super::super::same_library_class::LocalIdentity {
                    module: prepared.key.module.clone(),
                    signature: member.signature.clone(),
                },
                LocalFunction {
                    function,
                    class: prepared.key.clone(),
                    formals: member.formals.clone(),
                },
            );
            publish_overrides(
                ir,
                prepared.classifier,
                function,
                &member.overrides,
                self.sources.externals,
                self.sources.declarations,
            )?;
        }
        self.local_classes.insert(
            prepared.key,
            super::super::same_library_class::MaterializedLocalClass {
                class: class_id,
                classifier: prepared.classifier,
                construction: None,
                formals: prepared.formals,
                supertypes: prepared.supertypes,
                self_type: prepared.self_type,
                class_type_parameters: prepared.class_type_parameters,
            },
        );
        Ok(())
    }

    pub(super) fn link_local_member(
        &mut self,
        ir: &mut IrFile,
        signature: &KlibIrSignature,
    ) -> Result<LinkedCallee, KlibBodyDeclineReason> {
        let module = self.active_module.clone();
        let key = super::super::same_library_class::LocalIdentity {
            module: module.clone(),
            signature: signature.clone(),
        };
        if self.local_functions.contains_key(&key) {
            return self.linked_local_member(ir, &key);
        }
        let Some((arena, function)) = self
            .sources
            .bodies
            .module_local_function(&module, signature)
        else {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a call of a file-private declaration",
            ));
        };
        if function.constructor {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a call of a file-private constructor",
            ));
        }
        let Some((class_arena, class)) = self
            .sources
            .bodies
            .module_class_declaring(&module, signature)
        else {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a call of a file-private declaration",
            ));
        };
        if !std::ptr::eq(arena, class_arena) {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a member is not in its class".to_owned(),
            ));
        }
        if crate::metadata::semantic::class_kind(class.base.flags)
            == crate::libraries::TypeKind::Interface
        {
            self.install_local_interface(ir, &class.base.symbol.signature)?;
            return self.linked_local_member(ir, &key);
        }
        let constructor = class_constructor(class_arena, class)?;
        self.link_local_constructor(ir, &constructor)?;
        self.linked_local_member(ir, &key)
    }

    /// The object a body of this library reads under the non-public `signature`, installed in
    /// this unit by its private constructor, which creates its one instance.
    pub(super) fn install_local_object(
        &mut self,
        ir: &mut IrFile,
        signature: &KlibIrSignature,
    ) -> Result<crate::types::TypeName, KlibBodyDeclineReason> {
        let module = self.active_module.clone();
        let Some(classifier) =
            super::super::same_library_class::classifier(self.sources.bodies, &module, signature)?
        else {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a file-private object value",
            ));
        };
        let Some((arena, class)) = self.sources.bodies.module_local_class(&module, signature)
        else {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a file-private object value",
            ));
        };
        let constructor = class_constructor(arena, class)?;
        if !self.link_local_constructor(ir, &constructor)?.object {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "an object value names a class other than an object".to_owned(),
            ));
        }
        Ok(classifier)
    }

    pub(super) fn linked_installed_member(
        &self,
        ir: &IrFile,
        key: &super::super::same_library_class::LocalIdentity,
    ) -> Result<LinkedCallee, KlibBodyDeclineReason> {
        self.linked_local_member(ir, key)
    }

    fn linked_local_member(
        &self,
        ir: &IrFile,
        key: &super::super::same_library_class::LocalIdentity,
    ) -> Result<LinkedCallee, KlibBodyDeclineReason> {
        let Some(registered) = self.local_functions.get(key) else {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a call of a file-private declaration",
            ));
        };
        let Some(class) = self.local_classes.get(&registered.class) else {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a member's class was not materialized".to_owned(),
            ));
        };
        let serialized = match &key.signature {
            KlibIrSignature::Public(signature) => {
                self.sources.bodies.function(&KlibDependencyCallable {
                    module: key.module.clone(),
                    declaration: KlibDeclarationSignature::Public(signature.clone()),
                })
            }
            KlibIrSignature::Accessor(signature) => {
                self.sources.bodies.function(&KlibDependencyCallable {
                    module: key.module.clone(),
                    declaration: KlibDeclarationSignature::Accessor(signature.clone()),
                })
            }
            local => self
                .sources
                .bodies
                .module_local_function(&key.module, local),
        };
        let Some((_, declaration)) = serialized else {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a materialized member has no serialized declaration".to_owned(),
            ));
        };
        let defaults = declaration
            .regular_parameters
            .iter()
            .map(|parameter| parameter.default_value.is_some())
            .collect();
        Ok(super::super::same_library_class::linked_member(
            class,
            (registered.function, &registered.formals),
            ir,
            defaults,
        ))
    }
}

fn publish_overrides(
    ir: &mut IrFile,
    owner: crate::types::TypeName,
    function: crate::ir::FunId,
    overrides: &[super::super::same_library_class::MemberOverride],
    externals: &HashMap<KlibDependencyCallable, EmittedDependencyCallable>,
    declarations: &dyn crate::symbol_source::SymbolSource,
) -> Result<(), KlibBodyDeclineReason> {
    if overrides.is_empty() {
        return Ok(());
    }
    let implementation = &ir.functions[function as usize];
    let implementation_parameters = implementation.params.clone();
    let implementation_result = implementation.ret;
    let name = implementation.name.clone();
    for edge in overrides {
        let external = registered_override_handle(&edge.identity, externals, declarations)?;
        if edge.declared_parameters.len() != implementation_parameters.len()
            || edge.applied_parameters.len() != implementation_parameters.len()
        {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "an override's parameters differ from its interface member".to_owned(),
            ));
        }
        ir.function_overrides
            .entry(owner)
            .or_default()
            .push(crate::ir::IrFunctionOverride {
                implementation: crate::fir::ResolvedFunctionOverrideTarget::External(
                    external.target(),
                ),
                implementation_function: Some(function),
                implementation_owner: owner,
                overridden: crate::fir::ResolvedFunctionOverrideTarget::External(external.target()),
                overridden_owner: edge.owner,
                overridden_semantic_role: edge.semantic_role,
                collection_barrier: None,
                overridden_is_interface: edge.is_interface,
                name: name.clone(),
                declared_parameters: edge.declared_parameters.clone(),
                declared_result: edge.declared_result,
                applied_parameters: edge.applied_parameters.clone(),
                applied_result: edge.applied_result,
                implementation_parameters: implementation_parameters.clone(),
                implementation_parameter_identities: implementation_parameters
                    .iter()
                    .enumerate()
                    .map(
                        |(ordinal, _)| crate::fir::ResolvedParameterIdentity::Unnamed {
                            ordinal: u32::try_from(ordinal).expect("a parameter ordinal fits u32"),
                        },
                    )
                    .collect(),
                overridden_parameter_identities: edge
                    .declared_parameters
                    .iter()
                    .enumerate()
                    .map(
                        |(ordinal, _)| crate::fir::ResolvedParameterIdentity::Unnamed {
                            ordinal: u32::try_from(ordinal).expect("a parameter ordinal fits u32"),
                        },
                    )
                    .collect(),
                implementation_result,
                suspend: false,
                has_kotlin_superclass_override: false,
                depth: 1,
            });
    }
    Ok(())
}

/// The provider handle of an authenticated override. The call closure may already carry the same
/// handle when a body calls the member; a body that only constructs the class still publishes it.
pub(super) fn registered_override_handle(
    identity: &KlibDependencyCallable,
    externals: &HashMap<KlibDependencyCallable, EmittedDependencyCallable>,
    declarations: &dyn crate::symbol_source::SymbolSource,
) -> Result<EmittedDependencyCallable, KlibBodyDeclineReason> {
    if let Some((target, realization)) = declarations.external_callable_by_dependency(identity) {
        if realization.dependency_callable.as_ref() != Some(identity) {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "an override realization changed its exact identity".to_owned(),
            ));
        }
        if let Some(emitted) = externals.get(identity) {
            if emitted.target() != target {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "an override declaration handle disagrees with its call".to_owned(),
                ));
            }
        }
        return Ok(EmittedDependencyCallable::provider_leaf(target));
    }
    externals
        .get(identity)
        .copied()
        .ok_or(KlibBodyDeclineReason::UnsupportedDeclaration(
            "an override whose declaration handle was not registered",
        ))
}

fn class_constructor(
    arena: &crate::metadata::klib_ir::tree::KlibIrArena,
    class: &crate::metadata::klib_ir::tree::KlibIrClass,
) -> Result<KlibIrSignature, KlibBodyDeclineReason> {
    let mut found = None;
    for member in &class.members {
        let crate::metadata::klib_ir::tree::KlibIrMember::Function(id) = member else {
            continue;
        };
        let function = arena.function(*id);
        if !function.constructor {
            continue;
        }
        if found.is_some() {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a non-public class with another constructor",
            ));
        }
        found = Some(function.base.symbol.signature.clone());
    }
    found.ok_or(KlibBodyDeclineReason::UnsupportedOperation(
        "a call of a file-private declaration",
    ))
}

impl<'s> Linker<'_, 's> {
    /// Whether an earlier unit of the program materialized the class of the member `candidate`:
    /// it published the initialization of each of the class's constructors.
    pub(super) fn class_realized_earlier(&self, candidate: KlibBodyCallable<'s>) -> bool {
        let Some(constructors) = candidate.class_contract().and_then(|class| {
            super::super::class_initialization::initializing_constructors(
                class,
                self.sources.bodies,
            )
        }) else {
            return false;
        };
        !constructors.is_empty()
            && constructors.iter().all(|identity| {
                self.sources
                    .externals
                    .get(identity)
                    .is_some_and(|external| {
                        matches!(
                            external.realization,
                            DependencyExternalRealization::AllocatingConstructor
                                | DependencyExternalRealization::ConstructorInitializer
                        )
                    })
            })
    }

    /// Whether `declaration` is a member with a dispatch receiver of a non-public class of the
    /// same library, which no provider records. An earlier unit installed the class with all its
    /// members and published each under its identity, so a call of one retains the dispatch
    /// operand of its serialized declaration, as a call of any member emitted as part of its
    /// class does.
    pub(super) fn is_published_member(
        &self,
        identity: &KlibDependencyCallable,
        arena: &KlibIrArena,
        declaration: &KlibIrFunction,
        unselected: bool,
    ) -> bool {
        let KlibDeclarationSignature::Public(signature) = &identity.declaration else {
            return false;
        };
        unselected
            && declaration.dispatch_receiver.is_some()
            && self
                .sources
                .bodies
                .module_class_declaring(
                    &identity.module,
                    &crate::metadata::klib_ir::KlibIrSignature::Public(signature.clone()),
                )
                .is_some_and(|(class_arena, class)| {
                    std::ptr::eq(arena, class_arena)
                        && crate::metadata::semantic::declaration_visibility(class.base.flags)
                            != crate::types::Visibility::Public
                })
    }

    /// The member `identity` of a public class this unit installed, which every call in the
    /// unit reaches directly: the class is final, so no override can replace it.
    pub(super) fn installed_member(
        &self,
        ir: &IrFile,
        identity: &KlibDependencyCallable,
    ) -> Result<Option<LinkedCallee>, KlibBodyDeclineReason> {
        let key = installed_identity(identity);
        if !self.local_functions.contains_key(&key) {
            return Ok(None);
        }
        self.linked_installed_member(ir, &key).map(Some)
    }
}

/// The key under which a class installed with all of its members records the member `identity`.
pub(super) fn installed_identity(
    identity: &KlibDependencyCallable,
) -> super::super::same_library_class::LocalIdentity {
    let signature = match &identity.declaration {
        KlibDeclarationSignature::Public(signature) => KlibIrSignature::Public(signature.clone()),
        KlibDeclarationSignature::Accessor(signature) => {
            KlibIrSignature::Accessor(signature.clone())
        }
        KlibDeclarationSignature::FileLocal { file, slot } => KlibIrSignature::FileLocal {
            file: *file,
            slot: *slot,
        },
    };
    super::super::same_library_class::LocalIdentity {
        module: identity.module.clone(),
        signature,
    }
}

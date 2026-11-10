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
        let class = super::super::same_library_class::installed_class(&prepared);
        let class_id = ir.add_class(class);
        ir.record_declared_class_self_type(prepared.classifier, prepared.self_type);
        let init = declare_function(ir, &prepared.init_header);
        ir.set_method_visibility(init, crate::types::Visibility::Private);
        ir.classes[class_id as usize].methods.push(init);
        ir.note_class_method(class_id, init);
        let receiver = ir.add_expr(IrExpr::GetValue(0));
        ir.set_logical_type(receiver, prepared.self_type);
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
            value_member: None,
            local_body: Some(super::super::same_library_class::LocalBody::Initialize(
                super::super::same_library_class::LocalInitialize {
                    class: class_id,
                    receiver_symbol: prepared.receiver_symbol,
                    receiver_ty: prepared.receiver_ty,
                    fields: prepared.stored.clone(),
                    steps: prepared.steps,
                },
            )),
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
                        .callee_origin(&prepared.key.module, &declaration)
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
                    setter: member.setter,
                },
            );
            self.pending.push_back(PendingBody {
                identity: identity.clone(),
                arena: prepared.arena,
                symbol: member.symbol,
                statements: member.statements,
                header: member.header,
                function,
                value_member: None,
                local_body: Some(super::super::same_library_class::LocalBody::Member(
                    super::super::same_library_class::LocalMemberFields {
                        class: class_id,
                        receiver: prepared.self_type,
                        fields: prepared.stored.clone(),
                    },
                )),
            });
            publish_overrides(
                ir,
                prepared.classifier,
                function,
                &member.overrides,
                self.sources.externals,
                self.sources.declarations,
            )?;
        }
        let constructor = super::super::same_library_class::LocalConstructor {
            declaration: prepared.declaration.clone(),
            access: prepared.access,
        };
        self.local_classes.insert(
            prepared.key,
            super::super::same_library_class::MaterializedLocalClass {
                class: class_id,
                classifier: prepared.classifier,
                constructor: constructor_signature,
                declaration: prepared.declaration,
                access: prepared.access,
                formals: prepared.formals,
                supertypes: prepared.supertypes,
                self_type: prepared.self_type,
                class_type_parameters: prepared.class_type_parameters,
            },
        );
        Ok(constructor)
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
        let constructor = class_constructor(class_arena, class)?;
        self.link_local_constructor(ir, &constructor)?;
        self.linked_local_member(ir, &key)
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
            registered.function,
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
                overridden_semantic_role: None,
                collection_barrier: None,
                overridden_is_interface: true,
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

impl Linker<'_, '_> {
    /// The member `identity` of a public class this unit installed, which every call in the
    /// unit reaches directly: the class is final, so no override can replace it.
    pub(super) fn installed_member(
        &self,
        ir: &IrFile,
        identity: &KlibDependencyCallable,
    ) -> Result<Option<(LinkedCallee, bool)>, KlibBodyDeclineReason> {
        let key = installed_identity(identity);
        let Some(setter) = self
            .local_functions
            .get(&key)
            .map(|function| function.setter)
        else {
            return Ok(None);
        };
        self.linked_installed_member(ir, &key)
            .map(|linked| Some((linked, setter)))
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
    };
    super::super::same_library_class::LocalIdentity {
        module: identity.module.clone(),
        signature,
    }
}

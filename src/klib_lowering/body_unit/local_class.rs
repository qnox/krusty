//! Installing a same-library file-local class into the unit that lowers its bodies.

use super::*;
use crate::ir::{IrExpr, IrFile};
use crate::metadata::klib_ir::KlibIrSignature;

impl<'u, 's> Linker<'u, 's> {
    pub(super) fn install_local_class(
        &mut self,
        ir: &mut IrFile,
        prepared: super::super::same_library_class::PreparedLocalClass<'s>,
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
            initialization: None,
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
        for member in prepared.members {
            let function = declare_function(ir, &member.header);
            ir.set_method_visibility(function, crate::types::Visibility::Private);
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
                },
            );
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
                        receiver: prepared.self_type,
                        fields: prepared.stored.clone(),
                    },
                )),
            });
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
        let Some((_, declaration)) = self
            .sources
            .bodies
            .module_local_function(&key.module, &key.signature)
        else {
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

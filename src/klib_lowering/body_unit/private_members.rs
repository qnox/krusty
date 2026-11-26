//! Private members of a class whose member body this unit lowers: the value class or enclosing
//! class a body calls them through, their headers, and their emission with the calling member.

use super::*;

impl<'u, 's> Linker<'u, 's> {
    /// A call of a private member of the value class whose member is being lowered. No provider
    /// records a private member, and only its class calls it, so it is emitted as the calling
    /// member is, reading the value through the same field.
    pub(super) fn private_class_member(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
    ) -> Result<Option<LinkedCallee>, KlibBodyDeclineReason> {
        // A private member is called only from its own class or a class nested in it, whose
        // member body this lowering made active before it.
        if self.sources.callees.get(identity).is_some()
            || self.sources.externals.contains_key(identity)
        {
            return Ok(None);
        }
        let Some(active) = self.private_member_class(ir, identity)? else {
            return Ok(None);
        };
        let Some((_, declaration)) = self.sources.bodies.function(identity) else {
            return Ok(None);
        };
        if declaration.dispatch_receiver.is_none()
            || crate::metadata::semantic::declaration_visibility(declaration.base.flags)
                != crate::types::Visibility::Private
        {
            return Ok(None);
        }
        let (arena, declaration, header, class_formals) =
            private_class_member_header(active.contract, identity, self.sources.bodies)?;
        self.private_members.entry(identity.clone()).or_default();
        let function = match self.functions.get(identity) {
            Some(&function) => function,
            None => {
                let class = active.class;
                let function = self.queue(
                    ir,
                    identity,
                    None,
                    arena,
                    declaration,
                    BodyHeader {
                        header,
                        value_member: Some(active),
                    },
                )?;
                // It is a member of its class like any other, one no subclass overrides. An
                // inline one is expanded into every call and never dispatched, so the class
                // carries no method for it.
                if !ir.inline_only_fns.contains(&function) {
                    ir.set_method_visibility(function, crate::types::Visibility::Private);
                    register_class_method(ir, class, function);
                }
                function
            }
        };
        let declared = &ir.functions[function as usize];
        let formals = &self.private_members[identity];
        let (Some(receiver), params) = (
            self.member_receivers.get(identity).copied(),
            &declared.params,
        ) else {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a value class member is emitted without its receiver".to_owned(),
            ));
        };
        Ok(Some(LinkedCallee {
            callee: Callee::Local(function),
            receiver: Some(receiver),
            params: params.to_vec(),
            ret: declared.ret,
            context_count: 0,
            singleton_dispatch: None,
            type_parameters: formals.iter().map(|(name, _)| *name).collect(),
            type_parameter_bounds: Some(formals.iter().map(|(_, bounds)| bounds.clone()).collect()),
            class_type_parameters: class_formals,
            parameter_defaults: parameter_defaults(declaration),
            edge: super::super::CallEdge::default(),
        }))
    }

    pub(super) fn record_member_class(&mut self, member: &MemberClass<'s>) {
        if !self
            .member_classes
            .iter()
            .any(|known| known.class == member.class)
        {
            self.member_classes.push(member.clone());
        }
    }

    /// The class that directly declares the private member `identity`: one this lowering made
    /// active, an object the bodies read, or a class enclosing the calling one, which Kotlin lets
    /// call its private members. Kotlin makes a companion's private member visible in its
    /// containing class, whose body reads the companion and calls the member on it.
    fn private_member_class(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
    ) -> Result<Option<MemberClass<'s>>, KlibBodyDeclineReason> {
        let member = match &identity.declaration {
            KlibDeclarationSignature::Public(member) => member,
            KlibDeclarationSignature::Accessor(accessor) => accessor.property(),
            KlibDeclarationSignature::FileLocal { .. } => return Ok(None),
        };
        let Some((_, owner)) = member.declaration().segments().split_last() else {
            return Ok(None);
        };
        let declares = |class: &MemberClass<'s>| {
            class.contract.module == identity.module
                && class.contract.signature.package() == member.package()
                && class.contract.signature.declaration().segments() == owner
        };
        if let Some(active) = self
            .active_value_class
            .iter()
            .chain(&self.member_classes)
            .find(|class| declares(class))
        {
            return Ok(Some(active.clone()));
        }
        let crate::klib_libraries::LocatedMember::One { class, .. } =
            self.sources.bodies.member_owner(identity)
        else {
            return Ok(None);
        };
        let crate::metadata::klib_ir::KlibIrSignature::Public(signature) =
            &class.base.symbol.signature
        else {
            return Ok(None);
        };
        let Some(classifier) = super::super::object_classifier(signature)
            .filter(|classifier| !self.realized_classes.contains(classifier))
        else {
            return Ok(None);
        };
        let callees: &'s super::super::KlibCalleeFacts<'s> = self.sources.callees;
        let Some(object) = callees
            .read_class(classifier)
            .or_else(|| callees.private_member_class(classifier))
        else {
            return Ok(None);
        };
        let header = super::super::class_headers::authenticate_private_member(
            object,
            identity,
            self.sources.bodies,
        )?;
        let object = self.materialize_ordinary_member_class(ir, &header)?;
        self.record_member_class(&object);
        Ok(Some(object))
    }
}

/// Whether `identity` is a private member of the materialized class `class` with a body, which no
/// provider records and which a member of the class emitted from KLIB emits with it.
pub(crate) fn is_private_class_member(
    bodies: &KlibDeclarationBodies,
    identity: &KlibDependencyCallable,
    class: &crate::libraries::KlibBodyClassContract,
) -> bool {
    private_class_member_header(class, identity, bodies).is_ok()
}

/// A private class member's serialized declaration, its header, and its class's formals.
type PrivateMemberHeader<'b> = (
    &'b KlibIrArena,
    &'b KlibIrFunction,
    FunctionHeader,
    Vec<String>,
);

/// The header of `identity`, a private member of the value class `class` with a body, proved
/// against the class's raw header and read at the class's declared formals.
fn private_class_member_header<'b>(
    class: &crate::libraries::KlibBodyClassContract,
    identity: &KlibDependencyCallable,
    bodies: &'b KlibDeclarationBodies,
) -> Result<PrivateMemberHeader<'b>, KlibBodyDeclineReason> {
    let member = super::super::class_headers::authenticate_private_member(class, identity, bodies)?;
    block_statements(member.function)?;
    let header = super::super::function_lowering::raw_function_header_with_class(
        None,
        HeaderPurpose::PrivateClassMember(class.classifier),
        member.arena,
        member.function,
        member.types,
    )?;
    Ok((member.arena, member.function, header, member.formals))
}

/// A reference to the private class member `identity`, which the unit emits as `call` invokes it.
pub(super) fn private_member_reference(
    bodies: &KlibDeclarationBodies,
    identity: &KlibDependencyCallable,
    call: LinkedCallee,
) -> Result<LinkedCallableReference, KlibBodyDeclineReason> {
    let (Callee::Local(function), Some(receiver)) = (&call.callee, call.receiver) else {
        return Err(KlibBodyDeclineReason::SignatureMismatch(
            "a private member reference reaches no emitted member".to_owned(),
        ));
    };
    let (crate::types::TyKind::Obj(owner, _), Some((_, declaration))) =
        (receiver.kind(), bodies.function(identity))
    else {
        return Err(KlibBodyDeclineReason::SignatureMismatch(
            "a private member reference has no class receiver".to_owned(),
        ));
    };
    Ok(LinkedCallableReference {
        target: super::super::body_lowering::LinkedReferenceTarget::PrivateMember {
            owner,
            name: declaration.name.as_str().into(),
            function: *function,
        },
        kind: crate::libraries::ExternalCallableKind::Member,
        extension_receiver_parameter: None,
        call,
    })
}

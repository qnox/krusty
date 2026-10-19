//! Provider identities of published declarations, interned by their exact `IdSignature`.

use crate::fx_hash::HashMap;
use std::cell::RefCell;

use super::declaration_signatures::{SignedFunction, SignedProperty};
use crate::fir::{ExternalCallableId, ExternalPropertyId, ResolvedParameterIdentity};
use crate::libraries::{
    ExternalCallableKind, ExternalCallableRealization, ExternalPropertyRealization, FnKind,
    FunctionInfo, KlibBodyCompanionReceiver, KlibDeclarationSignature, KlibDependencyCallable,
    LibraryCallable, LibraryMember, PropertyInfo,
};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::types::{SemanticCallableOwner, TypeName};

/// One identity per exact declaration signature. The signature, not a spelling or a parameter
/// tuple, decides whether two candidates are the same declaration.
#[derive(Default)]
pub(super) struct ExternalIdentities {
    realizations: RefCell<Vec<ExternalCallableRealization>>,
    /// Parallel to `realizations`: the declaring KLIB is a friend of this compilation.
    internal_friends: RefCell<Vec<bool>>,
    by_signature: RefCell<HashMap<(Option<String>, KlibDeclarationSignature), ExternalCallableId>>,
    properties: RefCell<Vec<ExternalPropertyRealization>>,
    properties_by_signature:
        RefCell<HashMap<(Option<String>, KlibPublicIdSignature), ExternalPropertyId>>,
}

impl ExternalIdentities {
    /// Give a normalized function the identity of its declaration, publishing the declaration's
    /// realization, of shape `kind`, the first time its signature is seen.
    pub(super) fn assign_function(
        &self,
        signed: &SignedFunction,
        kind: ExternalCallableKind,
        function: &mut FunctionInfo,
    ) {
        let companion_receiver = function
            .associated_classifier
            .filter(|_| signed.declaration.receiver.is_some())
            .map(KlibBodyCompanionReceiver::new);
        self.assign_callable(
            KlibDeclarationSignature::Public(signed.signature.clone()),
            kind,
            &signed.parameters.physical,
            signed.internal_friend,
            signed.module.as_deref(),
            companion_receiver,
            signed.direct_c_function.clone(),
            &mut function.callable,
        );
    }

    /// Give a normalized property and each of its accessors the identity of its declaration. Each
    /// accessor realizes its own accessor signature, as a function of shape `kind`; the property
    /// realization joins the two accessor identities.
    pub(super) fn assign_property(
        &self,
        signed: &SignedProperty,
        kind: ExternalCallableKind,
        property: &mut PropertyInfo,
    ) {
        let companion_receiver = property
            .associated_classifier
            .filter(|_| signed.declaration.receiver.is_some())
            .map(KlibBodyCompanionReceiver::new);
        self.assign_callable(
            KlibDeclarationSignature::Accessor(signed.getter.clone()),
            kind,
            &signed.parameters.getter,
            signed.internal_friend,
            signed.module.as_deref(),
            companion_receiver.clone(),
            None,
            &mut property.getter,
        );
        if let Some(setter) = &mut property.setter {
            let signature = signed
                .setter
                .clone()
                .expect("a normalized setter is a signed `var` setter");
            let parameters = signed
                .parameters
                .setter
                .as_deref()
                .expect("a signed `var` has setter parameter identities");
            self.assign_callable(
                KlibDeclarationSignature::Accessor(signature),
                kind,
                parameters,
                signed.internal_friend,
                signed.module.as_deref(),
                companion_receiver,
                None,
                setter,
            );
        }
        let identity = self.intern_property(signed, property);
        // Each accessor's realization names its property, as the published accessor does.
        let mut realizations = self.realizations.borrow_mut();
        for accessor in std::iter::once(&mut property.getter).chain(property.setter.as_mut()) {
            accessor.external_property_identity = Some(identity);
            if let Some(realization) = accessor
                .external_identity
                .and_then(|accessor| realizations.get_mut(accessor.raw() as usize))
            {
                realization.callable.external_property_identity = Some(identity);
            }
        }
    }

    /// Give a normalized constructor of `owner` the identity of its declaration.
    pub(super) fn assign_constructor(
        &self,
        signature: &KlibPublicIdSignature,
        parameters: &[ResolvedParameterIdentity],
        owner: TypeName,
        internal_friend: bool,
        module: Option<&str>,
        constructor: &mut LibraryMember,
    ) {
        let mut callable = LibraryCallable::constructor(owner, constructor);
        callable.declaration_owner = Some(SemanticCallableOwner::Classifier(owner));
        self.assign_callable(
            KlibDeclarationSignature::Public(signature.clone()),
            ExternalCallableKind::Constructor,
            parameters,
            internal_friend,
            module,
            None,
            None,
            &mut callable,
        );
        constructor.external_identity = callable.external_identity;
    }

    /// Give a member `owner` declares implicitly, and that is named through `owner` with no value
    /// operand (an enum class's `values`, `valueOf`, or `entries` getter), the identity of its
    /// declaration.
    pub(super) fn assign_classifier_member(
        &self,
        signature: KlibDeclarationSignature,
        parameters: &[ResolvedParameterIdentity],
        owner: TypeName,
        internal_friend: bool,
        module: Option<&str>,
        member: &mut LibraryMember,
    ) {
        let mut callable =
            FunctionInfo::classifier_member(FnKind::TopLevel, owner, member.clone()).callable;
        callable.declaration_owner = Some(SemanticCallableOwner::Classifier(owner));
        self.assign_callable(
            signature,
            ExternalCallableKind::TopLevel,
            parameters,
            internal_friend,
            module,
            None,
            None,
            &mut callable,
        );
        member.external_identity = callable.external_identity;
    }

    fn intern_property(
        &self,
        signed: &SignedProperty,
        property: &PropertyInfo,
    ) -> ExternalPropertyId {
        if let Some(identity) = self
            .properties_by_signature
            .borrow()
            .get(&(signed.module.clone(), signed.signature.clone()))
            .copied()
        {
            return identity;
        }
        let mut properties = self.properties.borrow_mut();
        let identity = ExternalPropertyId::from_raw(
            u32::try_from(properties.len())
                .expect("too many external property declarations for packed FIR identity"),
        );
        properties.push(ExternalPropertyRealization {
            name: property.name.clone(),
            getter: property
                .getter
                .external_identity
                .expect("a published property's getter has its identity"),
            setter: property
                .setter
                .as_ref()
                .and_then(|setter| setter.external_identity),
            declares_value_class_storage: false,
            compile_time_constant: property.compile_time_constant.clone(),
        });
        self.properties_by_signature
            .borrow_mut()
            .insert((signed.module.clone(), signed.signature.clone()), identity);
        identity
    }

    fn assign_callable(
        &self,
        signature: KlibDeclarationSignature,
        kind: ExternalCallableKind,
        parameters: &[ResolvedParameterIdentity],
        internal_friend: bool,
        module: Option<&str>,
        klib_body_companion_receiver: Option<KlibBodyCompanionReceiver>,
        direct_c_function: Option<crate::libraries::DirectCFunction>,
        callable: &mut LibraryCallable,
    ) {
        let key = (module.map(str::to_owned), signature.clone());
        if let Some(identity) = self.by_signature.borrow().get(&key).copied() {
            callable.external_identity = Some(identity);
            if internal_friend {
                if let Some(friend) = self
                    .internal_friends
                    .borrow_mut()
                    .get_mut(identity.raw() as usize)
                {
                    *friend = true;
                }
            }
            return;
        }
        let mut realizations = self.realizations.borrow_mut();
        let identity = ExternalCallableId::from_raw(
            u32::try_from(realizations.len())
                .expect("too many external callable declarations for packed FIR identity"),
        );
        callable.external_identity = Some(identity);
        self.internal_friends.borrow_mut().push(internal_friend);
        realizations.push(ExternalCallableRealization {
            callable: callable.clone(),
            kind,
            declaration_owner: callable.declaration_owner,
            parameter_identities: parameters.into(),
            declaration_signature: Some(signature.clone()),
            dependency_callable: module.map(|module| KlibDependencyCallable {
                module: module.to_owned(),
                declaration: signature,
            }),
            direct_c_function,
            klib_body_companion_receiver,
        });
        self.by_signature.borrow_mut().insert(key, identity);
    }

    pub(super) fn realization(
        &self,
        identity: ExternalCallableId,
    ) -> Option<ExternalCallableRealization> {
        self.realizations
            .borrow()
            .get(identity.raw() as usize)
            .cloned()
    }

    pub(super) fn property(
        &self,
        identity: ExternalPropertyId,
    ) -> Option<ExternalPropertyRealization> {
        self.properties
            .borrow()
            .get(identity.raw() as usize)
            .cloned()
    }

    pub(super) fn internal_friend(&self, identity: ExternalCallableId) -> bool {
        self.internal_friends
            .borrow()
            .get(identity.raw() as usize)
            .copied()
            .unwrap_or(false)
    }
}

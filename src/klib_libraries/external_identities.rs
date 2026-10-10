//! Provider identities of published declarations, interned by their exact `IdSignature`.

use std::cell::RefCell;
use std::collections::HashMap;

use super::declaration_signatures::{SignedFunction, SignedProperty};
use crate::fir::{ExternalCallableId, ExternalPropertyId, ResolvedParameterIdentity};
use crate::libraries::{
    ExternalCallableKind, ExternalCallableRealization, ExternalPropertyRealization, FnKind,
    FunctionInfo, KlibDeclarationSignature, LibraryCallable, LibraryMember, PropertyInfo,
};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::types::{SemanticCallableOwner, TypeName};

/// One identity per exact declaration signature. The signature, not a spelling or a parameter
/// tuple, decides whether two candidates are the same declaration.
#[derive(Default)]
pub(super) struct ExternalIdentities {
    realizations: RefCell<Vec<ExternalCallableRealization>>,
    by_signature: RefCell<HashMap<KlibDeclarationSignature, ExternalCallableId>>,
    properties: RefCell<Vec<ExternalPropertyRealization>>,
    properties_by_signature: RefCell<HashMap<KlibPublicIdSignature, ExternalPropertyId>>,
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
        self.assign_callable(
            KlibDeclarationSignature::Public(signed.signature.clone()),
            kind,
            &signed.parameters.physical,
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
        self.assign_callable(
            KlibDeclarationSignature::Accessor(signed.getter.clone()),
            kind,
            &signed.parameters.getter,
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
                setter,
            );
        }
        let identity = self.intern_property(signed, property);
        property.getter.external_property_identity = Some(identity);
        if let Some(setter) = &mut property.setter {
            setter.external_property_identity = Some(identity);
        }
    }

    /// Give a normalized constructor of `owner` the identity of its declaration.
    pub(super) fn assign_constructor(
        &self,
        signature: &KlibPublicIdSignature,
        parameters: &[ResolvedParameterIdentity],
        owner: TypeName,
        constructor: &mut LibraryMember,
    ) {
        let mut callable = LibraryCallable::constructor(owner, constructor);
        callable.declaration_owner = Some(SemanticCallableOwner::Classifier(owner));
        self.assign_callable(
            KlibDeclarationSignature::Public(signature.clone()),
            ExternalCallableKind::Constructor,
            parameters,
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
        member: &mut LibraryMember,
    ) {
        let mut callable =
            FunctionInfo::classifier_member(FnKind::TopLevel, owner, member.clone()).callable;
        callable.declaration_owner = Some(SemanticCallableOwner::Classifier(owner));
        self.assign_callable(
            signature,
            ExternalCallableKind::TopLevel,
            parameters,
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
            .get(&signed.signature)
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
            .insert(signed.signature.clone(), identity);
        identity
    }

    fn assign_callable(
        &self,
        signature: KlibDeclarationSignature,
        kind: ExternalCallableKind,
        parameters: &[ResolvedParameterIdentity],
        callable: &mut LibraryCallable,
    ) {
        if let Some(identity) = self.by_signature.borrow().get(&signature).copied() {
            callable.external_identity = Some(identity);
            return;
        }
        let mut realizations = self.realizations.borrow_mut();
        let identity = ExternalCallableId::from_raw(
            u32::try_from(realizations.len())
                .expect("too many external callable declarations for packed FIR identity"),
        );
        callable.external_identity = Some(identity);
        realizations.push(ExternalCallableRealization {
            callable: callable.clone(),
            kind,
            declaration_owner: callable.declaration_owner,
            parameter_identities: parameters.into(),
            declaration_signature: Some(signature.clone()),
        });
        self.by_signature.borrow_mut().insert(signature, identity);
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
}

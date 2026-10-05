//! The interface default bodies a classifier inherits without overriding, as override resolution
//! selected them.
//!
//! Pass 1 (or the checked publication of a body-local classifier) walks the classifier's interface
//! closure while the declaration providers are live, keeps the nearest declaration of every
//! override slot, and records each selected non-abstract member here with the direct
//! superinterface it is inherited through. A target backend maps one record to its own
//! realization (a compatibility forwarder, a holder republication, a value-class static); it never
//! walks the interface hierarchy or compares members again.

use super::{DeclarationId, ExternalCallableId, ResolvedModuleIndex, ResolvedParameterIdentity};
use crate::types::{Ty, TypeName};

/// The semantic name of an inherited member: a function's declared name, or the property an
/// accessor belongs to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InheritedMemberName {
    Function(Box<str>),
    PropertyGetter(Box<str>),
    PropertySetter(Box<str>),
}

/// Where the selected declaration's body lives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InheritedDefaultBody {
    /// A body this module compiles.
    Module,
    /// A Kotlin dependency interface that publishes the body as its own interface method.
    DependencyInterfaceMethod(ExternalCallableId),
    /// A Kotlin dependency compiled with a receiver-first compatibility holder as the only body.
    DependencyHolder(ExternalCallableId),
    /// A Java interface's default method.
    JavaDefaultMethod,
}

/// One interface member a classifier inherits with a body and does not override.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedInheritedDefault {
    pub name: InheritedMemberName,
    /// The selected function declaration, which a `super` call through the classifier names; an
    /// accessor leaves it unset.
    pub function: Option<super::ResolvedFunctionOverrideTarget>,
    /// The interface that declares the selected member.
    pub declaring_interface: TypeName,
    /// The first declared direct superinterface of the classifier through which the member is
    /// inherited (the declaring interface itself when the classifier lists it).
    pub dispatch_interface: TypeName,
    /// The declaration's own parameters (context parameters, then an extension receiver, then the
    /// value parameters or a setter's value), before any substitution.
    pub parameters: Box<[Ty]>,
    pub parameter_identities: Box<[ResolvedParameterIdentity]>,
    pub result: Ty,
    /// The same parameters and result as the classifier sees them through its applied supertype
    /// (`f(value: String): String` for `I<String>`). A realization that specializes the inherited
    /// member declares these; a call to the inherited declaration still uses the declared shape.
    pub applied_parameters: Box<[Ty]>,
    pub applied_result: Ty,
    pub suspend: bool,
    /// The declaration's last physical parameter is its declared `vararg` (kotlinc's
    /// `ACC_VARARGS` condition for the compatibility surface that republishes it).
    pub vararg: bool,
    pub body: InheritedDefaultBody,
}

impl ResolvedModuleIndex {
    /// The interface defaults `classifier` inherits without overriding, in the order its interface
    /// closure reaches them. Absence means the classifier's override plan has not been finalized.
    pub fn inherited_defaults(
        &self,
        classifier: DeclarationId,
    ) -> Option<&[ResolvedInheritedDefault]> {
        self.inherited_defaults.get(&classifier).map(Box::as_ref)
    }

    pub(crate) fn publish_inherited_defaults(
        &mut self,
        classifier: DeclarationId,
        defaults: Vec<ResolvedInheritedDefault>,
    ) {
        assert!(
            self.classifier_header(classifier).is_some(),
            "inherited defaults require a published classifier"
        );
        for default in &defaults {
            for ty in default
                .parameters
                .iter()
                .chain(default.applied_parameters.iter())
                .chain([&default.result, &default.applied_result])
            {
                assert!(
                    !ty.mentions_pending() && !ty.mentions_error(),
                    "an inherited default publishes only finalized types"
                );
            }
            assert_eq!(
                default.parameters.len(),
                default.parameter_identities.len(),
                "an inherited default publishes every parameter identity"
            );
            assert_eq!(
                default.parameters.len(),
                default.applied_parameters.len(),
                "an inherited default applies every declared parameter"
            );
        }
        assert!(
            self.inherited_defaults
                .insert(classifier, defaults.into_boxed_slice())
                .is_none(),
            "a source classifier may publish inherited defaults only once"
        );
    }
}

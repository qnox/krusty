//! Kotlin functions and properties normalized into common library candidates.
//!
//! One declaration model covers every place a function or property can sit: at package level, as
//! a member of a class, or named through a classifier with no value operand. The placement decides
//! the candidate's kind and owners; every type stays semantic either way.

use super::parameter_identities::{FunctionParameterIdentities, PropertyParameterIdentities};
use super::type_signatures::{
    declared_function_generic_sig, declared_property_generic_sig,
    only_input_type_formals_with_identities, reified_type_parameter_ordinals, EnclosingBounds,
};
use super::TypeParameterIdentities;
use crate::libraries::{
    CallSig, FnFlags, FnKind, FunctionInfo, InlineKind, LibraryCallable, LibraryConst, PropKind,
    PropertyInfo, PropertyProducer, PropertyReadStability,
};
use crate::metadata::semantic::{KotlinFunction, KotlinModality, KotlinProperty};
use crate::types::{stored_value_ty, SemanticCallableOwner, Ty, TypeName};

/// Where a normalized function or property is declared and how a call reaches it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CallablePlacement {
    /// A top-level declaration of a package, or an extension declared there.
    Package(TypeName),
    /// A member of a class, or a member extension, dispatched on an instance of `owner`.
    Member {
        owner: TypeName,
        owner_is_interface: bool,
    },
    /// Named through `classifier` with no value operand: a `companion { … }` block member, which
    /// `classifier` declares and whose lexical body owns its private access, or a companion
    /// extension (`companion fun C.name()`), which a package declares. A written receiver names
    /// the classifier and is not a parameter.
    Associated {
        classifier: TypeName,
        declaration_owner: SemanticCallableOwner,
        access_owner: Option<TypeName>,
    },
}

impl CallablePlacement {
    fn declaration_owner(self) -> SemanticCallableOwner {
        match self {
            Self::Package(package) => SemanticCallableOwner::Package(package),
            Self::Member { owner, .. } => SemanticCallableOwner::Classifier(owner),
            Self::Associated {
                declaration_owner, ..
            } => declaration_owner,
        }
    }

    fn owner_is_interface(self) -> bool {
        matches!(
            self,
            Self::Member {
                owner_is_interface: true,
                ..
            }
        )
    }

    fn associated(self) -> (Option<TypeName>, Option<TypeName>) {
        match self {
            Self::Associated {
                classifier,
                access_owner,
                ..
            } => (Some(classifier), access_owner),
            Self::Package(_) | Self::Member { .. } => (None, None),
        }
    }

    /// Whether the declaration's written receiver is an extension receiver parameter.
    fn receiver_is_parameter(self) -> bool {
        !matches!(self, Self::Associated { .. })
    }
}

/// Normalize one function at `placement` into a selection candidate. `enclosing` bounds the type
/// parameters of the classes around the declaration.
///
/// Every type stays semantic: the callable's parameters are the declared ones in physical source
/// order (leading context parameters, then the extension receiver, then value parameters), and its
/// result is the declared result. The candidate names no physical owner or descriptor and carries
/// no provider identity; the provider that publishes it assigns one.
///
/// `parameters` are the function's validated parameter identities, which also prove that its
/// context parameters are a prefix of its parameters.
///
/// Normalization attaches no target realization: a compiler intrinsic or a semantic role is a
/// fact a provider decides from what its artifact says about the declaration's implementation.
///
/// A member is a [`FnKind::Member`], or a [`FnKind::Extension`] when it declares an extension
/// receiver; an associated function is receiver-less and names its classifier.
pub(crate) fn declared_function(
    placement: CallablePlacement,
    function: &KotlinFunction,
    parameters: &FunctionParameterIdentities,
    type_parameters: &TypeParameterIdentities,
    enclosing: &EnclosingBounds,
) -> FunctionInfo {
    let mut generic_sig = declared_function_generic_sig(function, enclosing, type_parameters);
    if !placement.receiver_is_parameter() {
        generic_sig.receiver = None;
    }
    let (contexts, values) = generic_sig
        .params
        .split_at_checked(function.context_count)
        .expect("validated declarations list their context parameters first");
    let params = contexts
        .iter()
        .chain(&generic_sig.receiver)
        .chain(values)
        .copied()
        .collect();
    let kind = match (generic_sig.receiver, placement) {
        (Some(_), _) => FnKind::Extension,
        (None, CallablePlacement::Member { .. }) => FnKind::Member,
        (None, CallablePlacement::Package(_) | CallablePlacement::Associated { .. }) => {
            FnKind::TopLevel
        }
    };
    let inline = InlineKind::from_flags(function.is_inline, function.is_inline);
    let reified = reified_type_parameter_ordinals(&function.formals);
    let declaration_owner = placement.declaration_owner();
    let is_abstract = function.modality == KotlinModality::Abstract;

    let mut callable = LibraryCallable::library(
        declaration_owner.name(),
        function.name.clone(),
        params,
        generic_sig.ret,
        generic_sig.ret,
        String::new(),
    );
    callable.declaration_owner = Some(declaration_owner);
    callable.owner_is_interface = placement.owner_is_interface();
    callable.is_abstract = is_abstract;
    callable.visibility = function.visibility;
    callable.inline = inline;
    callable.suspend = function.is_suspend;
    callable.source_receiver = generic_sig.receiver;
    callable.generic_sig = Some(Box::new(generic_sig.clone()));
    callable.reified_type_parameter_ordinals = reified.clone().into_boxed_slice();
    callable.context_count = function.context_count;
    callable.annotations = function.annotations.clone();
    callable.contract = function
        .contract
        .as_ref()
        .map(|contract| type_parameters.normalize_contract(contract));

    let mut normalized = FunctionInfo::plain(kind, generic_sig.receiver, callable);
    (
        normalized.associated_classifier,
        normalized.associated_access_owner,
    ) = placement.associated();
    normalized.call_sig = CallSig::metadata_member(
        generic_sig.params.len(),
        function.param_names.clone(),
        function.param_defaults.clone(),
        function.vararg,
    );
    normalized.call_sig.parameter_identities = parameters.arguments.clone();
    normalized.call_sig.only_input_type_formals =
        only_input_type_formals_with_identities(&function.formals, type_parameters);
    normalized.call_sig.reified_type_parameter_ordinals = reified;
    normalized.generic_sig = Some(generic_sig);
    normalized.context_count = function.context_count;
    normalized.visibility = function.visibility;
    normalized.flags = FnFlags {
        inline,
        reified: function.has_reified_type_params,
        suspend: function.is_suspend,
        operator: function.is_operator,
        infix: function.is_infix,
        is_abstract,
        is_final: function.modality == KotlinModality::Final,
        inherited_by_delegation: false,
        return_value_status: Some(function.return_value_status),
    };
    normalized
}

/// The declared names of a property's accessors, as the publishing artifact names them.
pub(crate) struct PropertyAccessorNames<'a> {
    pub(crate) getter: &'a str,
    /// Present exactly for a `var`.
    pub(crate) setter: Option<&'a str>,
}

/// Normalize one property at `placement` into a selection candidate.
///
/// The property type and every accessor parameter stay semantic. Each accessor lists its
/// parameters in physical source order (leading context parameters, then the extension receiver,
/// then a setter's value); its result is the property type, or `Unit` for a setter. The candidate
/// names no physical owner, descriptor, or storage and carries no provider identity; the provider
/// that publishes it assigns one to each accessor and to the property. As for functions,
/// normalization attaches no compiler intrinsic and no semantic role to either accessor.
///
/// A member is a [`PropKind::Member`], or a [`PropKind::MemberExtension`] when it declares an
/// extension receiver; an associated property is receiver-less and names its classifier.
pub(crate) fn declared_property(
    placement: CallablePlacement,
    property: &KotlinProperty,
    parameters: &PropertyParameterIdentities,
    type_parameters: &TypeParameterIdentities,
    accessors: PropertyAccessorNames<'_>,
    enclosing: &EnclosingBounds,
) -> PropertyInfo {
    let mut generic_sig = declared_property_generic_sig(property, enclosing, type_parameters);
    if !placement.receiver_is_parameter() {
        generic_sig.receiver = None;
    }
    let receiver = generic_sig.receiver;
    let ty = generic_sig.ret;
    let declaration_owner = placement.declaration_owner();
    let is_abstract = property.modality == KotlinModality::Abstract;
    let getter_params: Vec<Ty> = generic_sig
        .params
        .iter()
        .chain(&receiver)
        .copied()
        .collect();
    let accessor = |name: &str, params: Vec<Ty>, ret: Ty| {
        let mut callable = LibraryCallable::library(
            declaration_owner.name(),
            name,
            params,
            ret,
            ret,
            String::new(),
        );
        callable.declaration_owner = Some(declaration_owner);
        callable.owner_is_interface = placement.owner_is_interface();
        callable.is_abstract = is_abstract;
        callable.source_receiver = receiver;
        callable.context_count = property.context_count;
        callable
    };

    let mut getter = accessor(accessors.getter, getter_params.clone(), ty);
    getter.visibility = property.visibility;
    getter.annotations = property.annotations.clone();
    let mut setter = accessors.setter.map(|name| {
        let params = getter_params
            .iter()
            .copied()
            .chain([stored_value_ty(ty)])
            .collect();
        let mut setter = accessor(name, params, Ty::Unit);
        setter.visibility = property.setter_visibility;
        setter
    });
    if !generic_sig.formals.is_empty() {
        if let Some(setter) = &mut setter {
            let mut setter_sig = generic_sig.clone();
            setter_sig.params.push(ty);
            setter_sig.ret = Ty::Unit;
            setter.generic_sig = Some(Box::new(setter_sig));
        }
        getter.generic_sig = Some(Box::new(generic_sig.clone()));
    }

    let compile_time_constant = property
        .constant
        .clone()
        .filter(|_| property.is_const)
        .map(|value| LibraryConst { ty, value });
    let kind = match (receiver, placement) {
        (Some(_), CallablePlacement::Member { .. }) => PropKind::MemberExtension,
        (Some(_), _) => PropKind::Extension,
        (None, CallablePlacement::Member { .. }) => PropKind::Member,
        (None, CallablePlacement::Package(_) | CallablePlacement::Associated { .. }) => {
            PropKind::TopLevel
        }
    };
    let (associated_classifier, associated_access_owner) = placement.associated();
    PropertyInfo {
        name: property.name.clone(),
        kind,
        receiver,
        associated_classifier,
        associated_access_owner,
        formals: generic_sig.formals,
        ty,
        context_count: property.context_count,
        context_param_names: property.context_param_names.clone(),
        context_parameter_identities: parameters.contexts.clone(),
        getter,
        setter,
        setter_visibility: property.setter_visibility,
        setter_parameter_name: property.setter_parameter_name.clone(),
        is_const: property.is_const,
        implicit_integer_coercion: property
            .annotations
            .iter()
            .any(|annotation| *annotation == crate::types::wk::implicit_integer_coercion()),
        compile_time_constant,
        metadata_constant_read: false,
        visibility: property.visibility,
        owner: declaration_owner.name(),
        receiver_rank: 0,
        source_key: None,
        stable_declaration: None,
        getter_declaration: None,
        setter_declaration: None,
        source_member: None,
        producer: PropertyProducer::KotlinAccessor,
        // A dependency property's reads are never stable for smart casts.
        read_stability: PropertyReadStability::Unstable,
        return_value_status: Some(property.return_value_status),
    }
}

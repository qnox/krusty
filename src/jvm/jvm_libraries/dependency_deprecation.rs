//! Deprecation of dependency declarations, published at the provider boundary.
//!
//! A class file states a declaration's deprecation ([`DeclaredDeprecation`]): Kotlin's
//! `@Deprecated` with the `@DeprecatedSinceKotlin` that replaces its level, or a Java declaration's
//! `Deprecated` attribute. The level in effect depends on this compilation's API version, which
//! only the provider knows, so the provider resolves it here and publishes it on the declaration
//! together with kotlinc's SYMBOL rendering of that declaration (`FirDiagnosticRenderers.SYMBOL`).
//! A use of the declaration reports what was published; the checker never reopens the class file.
//!
//! The rendering is the declaration's short header in source terms: `fun <T : B> R.name(p: P =
//! ...): Ret`, `var name: Type`, `constructor<T>(p: P): Owner<T>`, `class Name<out T> : Super`, and
//! for a Java declaration `static fun`, `field` and `static field`.

use std::sync::Arc;

use super::JvmLibraries;
use crate::jvm::classreader::ClassInfo;
use crate::libraries::{
    CallSig, FunctionInfo, GenericSig, LibraryMember, PropKind, PropertyInfo, TypeKind,
};
use crate::types::{
    DeclaredDeprecation, DeprecatedDeclaration, Deprecation, InlineParameterModifier, Ty, TypeName,
    TypeVariance,
};

/// The deprecations one dependency property states: its own (on its annotation method) and each
/// accessor's.
#[derive(Clone, Copy, Default)]
pub(super) struct PropertyDeprecations<'a> {
    pub property: Option<&'a DeclaredDeprecation>,
    pub getter: Option<&'a DeclaredDeprecation>,
    pub setter: Option<&'a DeclaredDeprecation>,
}

impl<'a> PropertyDeprecations<'a> {
    /// What a Kotlin dependency property's metadata record was stamped with.
    pub(super) fn of(property: &'a crate::jvm::metadata::MetaProp) -> Self {
        Self {
            property: property.deprecation.as_deref(),
            getter: property.getter_deprecation.as_deref(),
            setter: property.setter_deprecation.as_deref(),
        }
    }
}

/// How a dependency property is declared, which its rendering names.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PropertyDeclaration {
    /// A Kotlin property: `val` or `var`.
    Kotlin,
    /// A Java field, static or not.
    JavaField { is_static: bool },
}

/// The classifier facts a dependency classifier's rendering names.
pub(super) struct ClassifierShape<'a> {
    pub name: TypeName,
    pub kind: TypeKind,
    pub is_fun_interface: bool,
    pub is_companion: bool,
    pub type_parameters: &'a [String],
    pub type_parameter_bounds: &'a [Vec<Ty>],
    pub type_parameter_variances: &'a [TypeVariance],
    pub supertypes: &'a [Ty],
}

impl JvmLibraries {
    fn effective_deprecation(&self, declared: Option<&DeclaredDeprecation>) -> Option<Deprecation> {
        declared?.effective(self.api_version)
    }

    /// Publish a dependency function's deprecation on its callable.
    pub(super) fn publish_function_deprecation(
        &self,
        function: Option<&mut FunctionInfo>,
        declared: Option<&DeclaredDeprecation>,
    ) {
        let (Some(function), Some(deprecation)) = (function, self.effective_deprecation(declared))
        else {
            return;
        };
        let symbol = FunctionSymbol {
            name: &function.callable.name,
            generic_sig: function.generic_sig.as_ref(),
            receiver: function.receiver,
            params: &function.callable.params,
            ret: function.callable.ret,
            call_sig: &function.call_sig,
            context_count: function.context_count,
            suspend: function.flags.suspend,
            java_static: false,
        }
        .render();
        function.callable.deprecation = Some(published(deprecation, symbol));
    }

    /// A dependency member function's deprecation. A Java static method renders as `static fun`.
    pub(super) fn member_deprecation(
        &self,
        member: &LibraryMember,
        declared: Option<&DeclaredDeprecation>,
        java_static: bool,
    ) -> Option<Arc<DeprecatedDeclaration>> {
        let deprecation = self.effective_deprecation(declared)?;
        let symbol = FunctionSymbol {
            name: &member.name,
            generic_sig: member.generic_sig.as_ref(),
            receiver: None,
            params: &member.params,
            ret: member.ret,
            call_sig: &member.call_sig,
            context_count: member.context_count,
            suspend: member.suspend(),
            java_static,
        }
        .render();
        Some(published(deprecation, symbol))
    }

    /// Publish the deprecation of the dependency classifier `ci` declares, as `classifier`, and of
    /// each of its `constructors`; returns the classifier's.
    pub(super) fn publish_classifier_deprecation(
        &self,
        ci: &ClassInfo,
        classifier: &ClassifierShape<'_>,
        constructors: &mut [LibraryMember],
        outer: Option<TypeName>,
    ) -> Option<Arc<DeprecatedDeclaration>> {
        let meta_constructors = crate::jvm::metadata::class_constructors(ci);
        for constructor in constructors {
            let declared = if ci.meta.is_present() {
                let realization_name = constructor
                    .physical_name
                    .as_deref()
                    .unwrap_or(&constructor.name);
                let exact_realization = (!constructor.descriptor.is_empty()).then(|| {
                    meta_constructors.iter().find(|declaration| {
                        declaration.jvm_name == realization_name
                            && declaration.jvm_desc == Some(constructor.descriptor.as_str())
                    })
                });
                exact_realization
                    .flatten()
                    .or_else(|| {
                        constructor
                            .descriptor
                            .is_empty()
                            .then(|| {
                                meta_constructors.iter().find(|declaration| {
                                    declaration.params.names == constructor.call_sig.param_names
                                        && declaration.params.types == constructor.params
                                })
                            })
                            .flatten()
                    })
                    .and_then(|declaration| declaration.deprecation.as_deref())
            } else {
                ci.methods
                    .iter()
                    .find(|method| {
                        method.name == "<init>" && method.descriptor == constructor.descriptor
                    })
                    .and_then(|method| method.deprecation.as_deref())
            };
            constructor.deprecation = self.constructor_deprecation(
                constructor,
                declared,
                classifier,
                ci.deprecation.as_deref(),
                outer,
            );
        }
        self.classifier_deprecation(ci.deprecation.as_deref(), classifier)
    }

    /// A dependency constructor's deprecation: the more severe of its own and its classifier's,
    /// rendered as the constructor, as kotlinc reports a constructor call.
    fn constructor_deprecation(
        &self,
        constructor: &LibraryMember,
        declared: Option<&DeclaredDeprecation>,
        classifier: &ClassifierShape<'_>,
        classifier_declared: Option<&DeclaredDeprecation>,
        outer: Option<TypeName>,
    ) -> Option<Arc<DeprecatedDeclaration>> {
        let own = self.effective_deprecation(declared);
        let deprecation = match (own, self.effective_deprecation(classifier_declared)) {
            (Some(own), Some(class)) if class.level > own.level => class,
            (Some(own), _) => own,
            (None, class) => class?,
        };
        let outer = outer
            .map(|outer| format!("{}.", Ty::obj_name(outer).source_name()))
            .unwrap_or_default();
        let type_parameters = type_parameters_display(
            classifier.type_parameters,
            classifier.type_parameter_bounds,
            Some(classifier.type_parameter_variances),
            &[],
        );
        let owner = Ty::obj_args_name(
            classifier.name,
            &classifier
                .type_parameters
                .iter()
                .map(|formal| Ty::ty_param(formal, Ty::obj("kotlin/Any")))
                .collect::<Vec<_>>(),
        );
        let symbol = format!(
            "{outer}constructor{type_parameters}({}): {}",
            value_parameters(&constructor.params, &constructor.call_sig, 0).join(", "),
            owner.source_name()
        );
        Some(published(deprecation, symbol))
    }

    /// Publish a Kotlin dependency property's deprecation, as its metadata record `declared` was
    /// stamped, on `property`.
    pub(super) fn publish_kotlin_property_deprecation(
        &self,
        property: Option<&mut PropertyInfo>,
        declared: &crate::jvm::metadata::MetaProp,
    ) {
        if let Some(property) = property {
            self.publish_property_deprecation(
                property,
                PropertyDeprecations::of(declared),
                PropertyDeclaration::Kotlin,
            );
        }
    }

    /// Publish a dependency property's deprecation on its accessors. A property's own deprecation
    /// covers both; an accessor's covers its own uses. Both render the property.
    pub(super) fn publish_property_deprecation(
        &self,
        property: &mut PropertyInfo,
        declared: PropertyDeprecations<'_>,
        declaration: PropertyDeclaration,
    ) {
        let own = self.effective_deprecation(declared.property);
        let getter = self
            .effective_deprecation(declared.getter)
            .or_else(|| own.clone());
        let setter = self.effective_deprecation(declared.setter).or(own);
        if getter.is_none() && setter.is_none() {
            return;
        }
        let symbol = property_symbol(property, declaration);
        property.getter.deprecation =
            getter.map(|deprecation| published(deprecation, symbol.clone()));
        if let Some(accessor) = property.setter.as_mut() {
            accessor.deprecation = setter.map(|deprecation| published(deprecation, symbol));
        }
    }

    /// A dependency classifier's deprecation.
    fn classifier_deprecation(
        &self,
        declared: Option<&DeclaredDeprecation>,
        classifier: &ClassifierShape<'_>,
    ) -> Option<Arc<DeprecatedDeclaration>> {
        let deprecation = self.effective_deprecation(declared)?;
        Some(published(deprecation, classifier_symbol(classifier)))
    }
}

fn published(deprecation: Deprecation, symbol: String) -> Arc<DeprecatedDeclaration> {
    Arc::new(DeprecatedDeclaration {
        deprecation,
        symbol: symbol.into_boxed_str(),
    })
}

/// The facts of one dependency function its rendering names. The declared signature, when the
/// provider has one, is preferred over the call-site shape.
struct FunctionSymbol<'a> {
    name: &'a str,
    generic_sig: Option<&'a GenericSig>,
    receiver: Option<Ty>,
    params: &'a [Ty],
    ret: Ty,
    call_sig: &'a CallSig,
    context_count: usize,
    suspend: bool,
    java_static: bool,
}

impl FunctionSymbol<'_> {
    fn render(&self) -> String {
        let (receiver, params, ret) = match self.generic_sig {
            Some(signature) => (
                signature.receiver,
                signature.params.as_slice(),
                signature.ret,
            ),
            None => (self.receiver, self.params, self.ret),
        };
        let parameters = value_parameters(params, self.call_sig, 0);
        let context_count = self.context_count.min(parameters.len());
        let context = if context_count == 0 {
            String::new()
        } else {
            format!("context({}) ", parameters[..context_count].join(", "))
        };
        let type_parameters = self.generic_sig.map_or_else(String::new, |signature| {
            type_parameters_display(
                &signature.formals,
                &signature.formal_bounds,
                None,
                &self.call_sig.reified_type_parameter_ordinals,
            )
        });
        let type_parameters = if type_parameters.is_empty() {
            type_parameters
        } else {
            format!("{type_parameters} ")
        };
        format!(
            "{}{context}{}fun {type_parameters}{}{}({}): {}",
            if self.java_static { "static " } else { "" },
            if self.suspend { "suspend " } else { "" },
            receiver
                .map(|receiver| format!("{}.", receiver.source_name()))
                .unwrap_or_default(),
            self.name,
            parameters[context_count..].join(", "),
            ret.source_name()
        )
    }
}

/// Each value parameter as `[noinline |crossinline ][vararg ][name: ]Type[ = ...]`, numbering from
/// `first`. A `vararg` parameter shows its element type. A declaration without a recorded source
/// parameter identity renders only the type; a diagnostic must not invent a reflection name.
fn value_parameters(params: &[Ty], call_sig: &CallSig, first: usize) -> Vec<String> {
    params
        .iter()
        .enumerate()
        .map(|(offset, &ty)| {
            let index = first + offset;
            let vararg = call_sig.vararg_index == Some(index);
            let modifier = match call_sig.inline_modifiers.get(index) {
                Some(InlineParameterModifier::Noinline) => "noinline ",
                Some(InlineParameterModifier::Crossinline) => "crossinline ",
                _ => "",
            };
            let name = match call_sig.parameter_identities.get(index) {
                Some(crate::fir::ResolvedParameterIdentity::Source(name))
                | Some(crate::fir::ResolvedParameterIdentity::ContextValue {
                    source_name: name,
                    ..
                }) => format!("{name}: "),
                _ => String::new(),
            };
            let ty = if vararg {
                ty.non_null().array_read_elem().unwrap_or(ty)
            } else {
                ty
            };
            format!(
                "{modifier}{}{name}{}{}",
                if vararg { "vararg " } else { "" },
                ty.source_name(),
                if call_sig.param_defaults.get(index).copied().unwrap_or(false) {
                    " = ..."
                } else {
                    ""
                }
            )
        })
        .collect()
}

/// `<reified T : Bound, in U>`, or empty. A bound is shown unless it is the implicit `Any?`; a
/// variance only when `variances` is given (a classifier's own parameters).
fn type_parameters_display(
    formals: &[String],
    bounds: &[Vec<Ty>],
    variances: Option<&[TypeVariance]>,
    reified: &[u32],
) -> String {
    if formals.is_empty() {
        return String::new();
    }
    let parameters = formals
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let variance = match variances.and_then(|variances| variances.get(index)) {
                Some(TypeVariance::In) => "in ",
                Some(TypeVariance::Out) => "out ",
                _ => "",
            };
            let reified = if reified
                .iter()
                .any(|&ordinal| usize::try_from(ordinal).is_ok_and(|ordinal| ordinal == index))
            {
                "reified "
            } else {
                ""
            };
            let bound = bounds
                .get(index)
                .and_then(|bounds| bounds.first())
                .filter(|bound| **bound != Ty::nullable(Ty::obj("kotlin/Any")))
                .map(|bound| format!(" : {}", bound.source_name()))
                .unwrap_or_default();
            format!("{variance}{reified}{name}{bound}")
        })
        .collect::<Vec<_>>();
    format!("<{}>", parameters.join(", "))
}

/// The supertypes `ci` declares, in source terms: its Kotlin metadata's, or a Java class's
/// superclass (`Object` as `Any`) and interfaces.
pub(super) fn declared_supertypes(ci: &ClassInfo) -> Vec<Ty> {
    if ci.meta.is_present() {
        return ci.meta.class_supertypes.clone();
    }
    ci.super_class
        .iter()
        .map(|superclass| {
            if superclass.matches("java/lang/Object") {
                Ty::obj("kotlin/Any")
            } else {
                Ty::obj_name(*superclass)
            }
        })
        .chain(ci.interfaces.iter_ids().map(Ty::obj_name))
        .collect()
}

fn property_symbol(property: &PropertyInfo, declaration: PropertyDeclaration) -> String {
    let keyword = match declaration {
        PropertyDeclaration::JavaField { is_static: true } => "static field",
        PropertyDeclaration::JavaField { is_static: false } => "field",
        PropertyDeclaration::Kotlin if property.setter.is_some() => "var",
        PropertyDeclaration::Kotlin => "val",
    };
    let type_parameters = if property.formals.is_empty() {
        String::new()
    } else {
        format!("<{}> ", property.formals.join(", "))
    };
    format!(
        "{keyword} {type_parameters}{}{}: {}",
        // kotlinc names only an extension receiver; a member's dispatch receiver is implicit.
        property
            .receiver
            .filter(|_| matches!(
                property.kind,
                PropKind::Extension | PropKind::MemberExtension
            ))
            .map(|receiver| format!("{}.", receiver.source_name()))
            .unwrap_or_default(),
        property.name,
        property.ty.source_name()
    )
}

fn classifier_symbol(classifier: &ClassifierShape<'_>) -> String {
    let kind = match classifier.kind {
        TypeKind::Annotation => "annotation class",
        TypeKind::Interface if classifier.is_fun_interface => "fun interface",
        TypeKind::Interface => "interface",
        TypeKind::Enum => "enum class",
        TypeKind::Object if classifier.is_companion => "companion object",
        TypeKind::Object => "object",
        TypeKind::Class => "class",
    };
    let supertypes = if classifier.supertypes.is_empty() {
        "Any".to_string()
    } else {
        classifier
            .supertypes
            .iter()
            .map(|supertype| supertype.source_name())
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "{kind} {}{} : {supertypes}",
        classifier.name.nested_segment_ref(),
        type_parameters_display(
            classifier.type_parameters,
            classifier.type_parameter_bounds,
            Some(classifier.type_parameter_variances),
            &[],
        )
    )
}

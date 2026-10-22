//! A property's getter and setter as KSP shows them. Every property has a getter and a `var` a
//! setter, written in the source or made by the compiler; an annotation targeted at one from the
//! property (`@get:A`, `@set:A`, `@setparam:A`) is shown on it.

use krusty_plugin_ir::{KspAccessor, KspAnnotation, KspModifier};

use super::annotations::Placement;
use super::{Source, Subject};
use crate::ast::{AnnotationRef, PropDecl, PropParam};
use crate::frontend::FrontendDeclarationId;
use crate::types::{PropertyAnnotationSite, ResolvedAnnotation};

/// A getter the compiler makes for a property whose modifiers say nothing of it. KSP2 reports a
/// public final property's accessors this way; another's are not described yet.
pub(super) fn compiler_getter() -> KspAccessor {
    compiler_accessor(Some(&[]), Vec::new(), None)
}

fn compiler_accessor(
    property_modifiers: Option<&[KspModifier]>,
    annotations: Vec<KspAnnotation>,
    parameter: Option<(String, Vec<KspAnnotation>)>,
) -> KspAccessor {
    let described = property_modifiers.is_some_and(|modifiers| {
        modifiers
            .iter()
            .all(|modifier| *modifier == KspModifier::Const)
    });
    KspAccessor {
        written: false,
        modifiers: described.then(|| vec![KspModifier::Final, KspModifier::Public]),
        annotations,
        parameter,
    }
}

/// The written annotations of `annotations` targeted at `site` from the property.
fn targeted(
    annotations: &[AnnotationRef],
    site: PropertyAnnotationSite,
) -> impl Iterator<Item = &AnnotationRef> {
    annotations
        .iter()
        .filter(move |annotation| annotation.property_site == Some(site))
}

impl Source<'_> {
    /// The accessors of the property `property`, the published declaration `declaration`, whose
    /// written modifiers are `modifiers`.
    pub(super) fn written_accessors(
        &self,
        subject: Subject<'_>,
        property: &PropDecl,
        declaration: Option<FrontendDeclarationId>,
        modifiers: Option<&[KspModifier]>,
    ) -> Result<(KspAccessor, Option<KspAccessor>), String> {
        let accessor_annotations = |setter: bool| {
            declaration.map(|declaration| self.analysis.accessor_annotations(declaration, setter))
        };
        let getter_written = targeted(&property.annotations, PropertyAnnotationSite::Getter)
            .chain(property.getter_annotations.iter().map(|entry| &entry.annotation));
        let getter_annotations = self.published_annotations(
            subject,
            getter_written,
            accessor_annotations(false),
            Placement::All,
        )?;
        let getter = if property.getter_declared {
            KspAccessor {
                written: true,
                // A written accessor's own modifiers are recorded only as far as `inline`.
                modifiers: (!property.getter_inline).then(Vec::new),
                annotations: getter_annotations,
                parameter: None,
            }
        } else {
            compiler_accessor(modifiers, getter_annotations, None)
        };
        if !property.is_var {
            return Ok((getter, None));
        }
        let written_setter = property.setter.as_ref();
        if written_setter.is_some_and(|setter| !setter.param_annotations.is_empty()) {
            return Err(self.refusal(
                subject,
                "has annotations on its setter's parameter, which are not published",
            ));
        }
        let setter_written = targeted(&property.annotations, PropertyAnnotationSite::Setter).chain(
            written_setter
                .into_iter()
                .flat_map(|setter| setter.annotations.iter().map(|entry| &entry.annotation)),
        );
        let setter_annotations = self.published_annotations(
            subject,
            setter_written,
            accessor_annotations(true),
            Placement::All,
        )?;
        let parameter_annotations = self.published_annotations(
            subject,
            &property.annotations,
            declaration.map(|declaration| self.analysis.declaration_annotations(declaration)),
            Placement::SetterParameter,
        )?;
        let parameter_name = written_setter
            .and_then(|setter| setter.param.clone())
            .unwrap_or_else(|| "value".to_string());
        let setter = match written_setter {
            Some(setter) => KspAccessor {
                written: true,
                modifiers: (!setter.is_inline && setter.visibility.is_none()).then(Vec::new),
                annotations: setter_annotations,
                parameter: Some((parameter_name, parameter_annotations)),
            },
            None => compiler_accessor(
                modifiers,
                setter_annotations,
                Some((parameter_name, parameter_annotations)),
            ),
        };
        Ok((getter, Some(setter)))
    }

    /// The accessors of the property a primary-constructor parameter declares: the published
    /// declaration `declaration`, whose applications were written on the parameter and published
    /// as `written`.
    pub(super) fn constructor_accessors(
        &self,
        subject: Subject<'_>,
        parameter: &PropParam,
        declaration: Option<FrontendDeclarationId>,
        written: Option<&[ResolvedAnnotation]>,
        modifiers: Option<&[KspModifier]>,
    ) -> Result<(KspAccessor, Option<KspAccessor>), String> {
        let accessor_annotations = |setter: bool| {
            declaration.map(|declaration| self.analysis.accessor_annotations(declaration, setter))
        };
        let getter = compiler_accessor(
            modifiers,
            self.published_annotations(
                subject,
                targeted(&parameter.annotations, PropertyAnnotationSite::Getter),
                accessor_annotations(false),
                Placement::All,
            )?,
            None,
        );
        if !parameter.is_var {
            return Ok((getter, None));
        }
        let setter = compiler_accessor(
            modifiers,
            self.published_annotations(
                subject,
                targeted(&parameter.annotations, PropertyAnnotationSite::Setter),
                accessor_annotations(true),
                Placement::All,
            )?,
            Some((
                "value".to_string(),
                self.published_annotations(
                    subject,
                    &parameter.annotations,
                    written,
                    Placement::SetterParameter,
                )?,
            )),
        );
        Ok((getter, Some(setter)))
    }
}

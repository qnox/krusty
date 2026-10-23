//! What KSP2 reports as a declaration's JVM signature (`Resolver.mapToJvmSignature`) and JVM name
//! (`Resolver.getJvmName`), from the names the platform's binary interface gives.
//!
//! KSP2 spells a signature from the declaration's value parameters and result alone: an extension
//! receiver, a continuation and an outer instance are left out, `Unit` is `V` wherever it is, a
//! `vararg` parameter is its element, and a non-null value class is its underlying type. It mangles
//! a function's name when a value parameter's type is a value class, `kotlin.Result` included, or
//! when a source class member's result is one, a constructor counting as returning its class; the
//! extension receiver is left out here too. It adds the module's suffix to an `internal` class
//! member's name only, after any mangle suffix. An accessor of
//! an annotation class's or a `@JvmRecord` class's property is the property's bare name.

use super::type_model::qualified_name;
use crate::frontend::SourceSetAnalysis;
use crate::fx_hash::HashSet;
use crate::libraries::PlatformAbi;
use crate::types::{wk, AnnotationValue, ResolvedAnnotation, Ty, TypeName};

pub(super) struct PlatformNames<'a> {
    abi: &'a dyn PlatformAbi,
    analysis: &'a SourceSetAnalysis,
    module_name: &'a str,
}

/// What names a function on the platform besides its parameters and result.
pub(super) struct FunctionNaming<'a> {
    pub(super) name: &'a str,
    pub(super) annotations: &'a [ResolvedAnnotation],
    pub(super) site: NamingSite,
}

/// Where a declaration is, as far as its platform name depends on it.
#[derive(Clone, Copy)]
pub(super) struct NamingSite {
    /// It is `internal`.
    pub(super) internal: bool,
    /// It is a class member, a constructor included, rather than a top-level declaration.
    pub(super) member: bool,
    /// It is a member of an annotation class or a `@JvmRecord` class, whose property accessors
    /// are named as the property.
    pub(super) bare_accessors: bool,
    /// The language generates it (a data class's `componentN` or `copy`, an enum class's
    /// `values`, `valueOf` or `entries`), so that its result never mangles its name.
    pub(super) generated: bool,
}

impl<'a> PlatformNames<'a> {
    /// The names `analysis`'s platform gives, or `None` when it has no binary interface.
    pub(super) fn new(analysis: &'a SourceSetAnalysis, module_name: &'a str) -> Option<Self> {
        Some(PlatformNames {
            abi: analysis.symbols.libraries.abi()?,
            analysis,
            module_name,
        })
    }

    /// The signature of a classifier or property of type `ty`.
    pub(super) fn type_signature(&self, ty: Ty) -> Option<String> {
        type_signature(self.analysis, ty)
    }

    /// The signature of a function with `parameters` and `result`; `None` when one of them is not
    /// known or not spelled.
    pub(super) fn function_signature(
        &self,
        parameters: &[Option<Ty>],
        result: Option<Ty>,
    ) -> Option<String> {
        let mut signature = String::from("(");
        for parameter in parameters {
            signature.push_str(&self.type_signature((*parameter)?)?);
        }
        signature.push(')');
        signature.push_str(&self.type_signature(result?)?);
        Some(signature)
    }

    /// The name of a function with `parameters` and `result`; `None` when a type is not known.
    pub(super) fn function_name(
        &self,
        naming: &FunctionNaming<'_>,
        parameters: &[Option<Ty>],
        result: Option<Ty>,
    ) -> Option<String> {
        if let Some(name) = jvm_name(naming.annotations) {
            return Some(name);
        }
        let result_mangles = naming.site.member && !naming.site.generated;
        let mut name = self.mangled(naming.name, parameters, result, result_mangles)?;
        if naming.site.internal && naming.site.member {
            name.push_str(&self.abi.internal_suffix(self.module_name));
        }
        Some(name)
    }

    /// The name of the getter (`setter` false) or setter of a property `name` of type `ty`.
    pub(super) fn accessor_name(
        &self,
        name: &str,
        setter: bool,
        ty: Option<Ty>,
        annotations: &[ResolvedAnnotation],
        site: NamingSite,
    ) -> Option<String> {
        if site.bare_accessors {
            return Some(jvm_name(annotations).unwrap_or_else(|| name.to_string()));
        }
        let base = if setter {
            self.abi.setter_name(name)
        } else {
            self.abi.getter_name(name)
        };
        let naming = FunctionNaming {
            name: &base,
            annotations,
            site,
        };
        match setter {
            true => self.function_name(&naming, &[ty], Some(Ty::Unit)),
            false => self.function_name(&naming, &[], ty),
        }
    }

    /// `name` with the mangle suffix when a parameter's type is a value class or, when
    /// `result_mangles`, the result's.
    fn mangled(
        &self,
        name: &str,
        parameters: &[Option<Ty>],
        result: Option<Ty>,
        result_mangles: bool,
    ) -> Option<String> {
        let mut elements = String::new();
        let mut mangles = false;
        for parameter in parameters {
            let parameter = (*parameter)?;
            match self.value_class(parameter) {
                Some(class) => {
                    mangles = true;
                    push_element(&mut elements, class, parameter.is_nullable());
                }
                None => elements.push('_'),
            }
        }
        let result = result?;
        if let Some(class) = self.value_class(result).filter(|_| result_mangles) {
            mangles = true;
            elements.push(':');
            push_element(&mut elements, class, result.is_nullable());
        }
        Some(match mangles {
            true => format!("{name}{}", self.abi.mangle_suffix(&elements)),
            false => name.to_string(),
        })
    }

    /// The value class `ty` is an occurrence of, nullable or not.
    fn value_class(&self, ty: Ty) -> Option<TypeName> {
        let class = ty.non_null().obj_internal()?;
        underlying(self.analysis, class).map(|_| class)
    }
}

/// The signature of a classifier or property of type `ty` on `analysis`'s platform; `None` when
/// the platform has no binary interface or cannot spell the type.
pub(super) fn type_signature(analysis: &SourceSetAnalysis, ty: Ty) -> Option<String> {
    let abi = analysis.symbols.libraries.abi()?;
    abi.type_descriptor(represented(analysis, ty))
}

/// The type `ty` is represented as: a non-null value class as its underlying type, all the way
/// down.
fn represented(analysis: &SourceSetAnalysis, ty: Ty) -> Ty {
    let mut ty = ty;
    let mut seen = HashSet::default();
    while let Some(class) = (!ty.is_nullable()).then(|| ty.obj_internal()).flatten() {
        match underlying(analysis, class) {
            Some(underlying) if seen.insert(class) => ty = underlying,
            _ => break,
        }
    }
    ty
}

/// A value class's declared underlying type: a source class's, or a library class's.
fn underlying(analysis: &SourceSetAnalysis, class: TypeName) -> Option<Ty> {
    let symbols = &analysis.symbols;
    match symbols.classes.get(&class) {
        Some(source) => source.value_field.as_ref().map(|(_, ty)| *ty),
        None => symbols.libraries.value_underlying(Ty::obj_name(class)),
    }
}

/// The name a `@JvmName` among `annotations` gives.
fn jvm_name(annotations: &[ResolvedAnnotation]) -> Option<String> {
    let annotation = annotations
        .iter()
        .find(|annotation| annotation.annotation == wk::jvm_name())?;
    annotation
        .arguments
        .iter()
        .find_map(|(name, value)| match value {
            AnnotationValue::String(text) if name == "name" => text.as_str().map(str::to_string),
            _ => None,
        })
}

/// A value class's element of a mangling signature.
fn push_element(elements: &mut String, class: TypeName, nullable: bool) {
    elements.push('L');
    elements.push_str(&qualified_name(class));
    if nullable {
        elements.push('?');
    }
    elements.push(';');
}

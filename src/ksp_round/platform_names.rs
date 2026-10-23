//! What KSP2 reports as a declaration's JVM signature (`Resolver.mapToJvmSignature`) and JVM name
//! (`Resolver.getJvmName`), from the names the platform's binary interface gives.
//!
//! KSP2 spells a signature from the declaration's value parameters and result alone: an extension
//! receiver, a continuation and an outer instance are left out, `Unit` is `V` wherever it is, a
//! `vararg` parameter is its element, and a non-null value class is its underlying type. It mangles
//! a function's name when a parameter's type is a value class or its result is one, a constructor
//! counting as returning its class, and adds `$` and the module's name to an `internal` one's.

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
    pub(super) internal: bool,
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
        self.abi.type_descriptor(self.represented(ty))
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

    /// The name of a function with `parameters` and `result`; `None` when a type is not known, or
    /// when it is both mangled and `internal`, which no oracle has shown yet.
    pub(super) fn function_name(
        &self,
        naming: &FunctionNaming<'_>,
        parameters: &[Option<Ty>],
        result: Option<Ty>,
    ) -> Option<String> {
        if let Some(name) = jvm_name(naming.annotations) {
            return Some(name);
        }
        let mangled = self.mangled(naming.name, parameters, result)?;
        match (naming.internal, mangled == naming.name) {
            (false, _) => Some(mangled),
            (true, true) => Some(format!("{}${}", naming.name, self.module_name)),
            (true, false) => None,
        }
    }

    /// The name of the getter (`setter` false) or setter of a property `name` of type `ty`.
    pub(super) fn accessor_name(
        &self,
        name: &str,
        setter: bool,
        ty: Option<Ty>,
        annotations: &[ResolvedAnnotation],
        internal: bool,
    ) -> Option<String> {
        let base = if setter {
            self.abi.setter_name(name)
        } else {
            self.abi.getter_name(name)
        };
        let naming = FunctionNaming {
            name: &base,
            annotations,
            internal,
        };
        match setter {
            true => self.function_name(&naming, &[ty], Some(Ty::Unit)),
            false => self.function_name(&naming, &[], ty),
        }
    }

    fn mangled(&self, name: &str, parameters: &[Option<Ty>], result: Option<Ty>) -> Option<String> {
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
        if let Some(class) = self.value_class(result?) {
            mangles = true;
            elements.push(':');
            push_element(&mut elements, class, result?.is_nullable());
        }
        Some(match mangles {
            true => format!("{name}{}", self.abi.mangle_suffix(&elements)),
            false => name.to_string(),
        })
    }

    /// The value class `ty` is an occurrence of, nullable or not.
    fn value_class(&self, ty: Ty) -> Option<TypeName> {
        let class = ty.non_null().obj_internal()?;
        self.underlying(class).map(|_| class)
    }

    /// The type `ty` is represented as: a non-null value class as its underlying type, all the way
    /// down.
    fn represented(&self, ty: Ty) -> Ty {
        let mut ty = ty;
        let mut seen = HashSet::default();
        while let Some(class) = (!ty.is_nullable()).then(|| ty.obj_internal()).flatten() {
            match self.underlying(class) {
                Some(underlying) if seen.insert(class) => ty = underlying,
                _ => break,
            }
        }
        ty
    }

    /// A value class's declared underlying type: a source class's, or a library class's.
    fn underlying(&self, class: TypeName) -> Option<Ty> {
        let symbols = &self.analysis.symbols;
        match symbols.classes.get(&class) {
            Some(source) => source.value_field.as_ref().map(|(_, ty)| *ty),
            None => symbols.libraries.value_underlying(Ty::obj_name(class)),
        }
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

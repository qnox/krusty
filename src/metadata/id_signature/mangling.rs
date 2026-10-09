//! The public `IdSignature` a KLIB's serialized IR gives a declaration, computed from the
//! declaration's shape.
//!
//! Kotlin derives a public declaration's member id by mangling its signature (Kotlin's
//! `IrMangleComputer` in `MangleMode.SIGNATURE`) and hashing the mangled text with CityHash64.
//! Two kinds of declaration meet on that one exact identity: one a KLIB's metadata describes and
//! one krusty writes into a KLIB of its own. Neither is ever joined to IR by name, arity or
//! parameter spelling.
//!
//! The mangling works on a target-free shape. A declaration model provides it by implementing
//! [`SignatureType`] for its types and building a [`CallableShape`] or [`PropertyShape`].
//!
//! The mangled form of a function is
//! `name` `(context types)`? `@receiver`? `(parameters)` `{type parameters}`, where a type is its
//! fully qualified classifier with `<arguments>` and `?`, a type parameter is
//! `container:index` (container 0 is the declaration itself, 1 its class, and so on outward), a
//! type parameter declaration is `index§<bound&bound>`, and a vararg parameter carries `...`. A
//! property is `(context types)`? `@receiver`? `{type parameters}` `name`. A result type is never
//! written for the declarations a KLIB links by public signature.

use std::borrow::Cow;

use super::{city_hash, KlibAccessorIdSignature, KlibNamePath, KlibPublicIdSignature};

/// A type as the signature mangler sees it.
pub trait SignatureType: Sized {
    fn view(&self) -> Result<TypeView<'_, Self>, ManglingError>;
}

/// One level of a type.
pub enum TypeView<'a, T> {
    Class {
        /// Package and class names joined with `.` (`kotlin.collections.Map.Entry`).
        fq_name: Cow<'a, str>,
        arguments: Vec<&'a T>,
        nullable: bool,
    },
    /// A reference to a type parameter in scope, by its declared name.
    Parameter {
        name: &'a str,
        nullable: bool,
    },
    In(&'a T),
    Out(&'a T),
    Star,
}

/// A declared type parameter.
pub struct TypeParameterShape<'a, T> {
    pub name: &'a str,
    /// Declared upper bounds; empty means the default `Any?`.
    pub bounds: Vec<&'a T>,
}

/// A class around a declaration.
pub struct ClassScope<'a, T> {
    pub name: &'a str,
    pub type_parameters: Vec<TypeParameterShape<'a, T>>,
}

/// Where a declaration sits: its package and the classes around it, outermost first.
pub struct DeclarationContainer<'a, T> {
    pub package: &'a [String],
    pub classes: &'a [ClassScope<'a, T>],
}

impl<T> Clone for DeclarationContainer<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for DeclarationContainer<'_, T> {}

impl<'a, T> DeclarationContainer<'a, T> {
    fn signature(&self, name: &str, mangled: Option<&str>) -> KlibPublicIdSignature {
        KlibPublicIdSignature {
            package: KlibNamePath {
                segments: self.package.to_vec().into_boxed_slice(),
            },
            declaration: KlibNamePath {
                segments: self
                    .classes
                    .iter()
                    .map(|class| class.name.to_owned())
                    .chain(std::iter::once(name.to_owned()))
                    .collect(),
            },
            member_id: mangled.map(|mangled| city_hash::city_hash64(mangled.as_bytes())),
            mask: 0,
        }
    }

    /// The type-parameter scopes a declaration's types see, innermost (the declaration) first.
    fn scopes<'s>(
        &'s self,
        own: &'s [TypeParameterShape<'a, T>],
    ) -> Vec<&'s [TypeParameterShape<'a, T>]> {
        std::iter::once(own)
            .chain(
                self.classes
                    .iter()
                    .rev()
                    .map(|class| class.type_parameters.as_slice()),
            )
            .collect()
    }
}

/// A function or constructor.
pub struct CallableShape<'a, T> {
    /// `<init>` for a constructor.
    pub name: &'a str,
    pub contexts: Vec<&'a T>,
    pub receiver: Option<&'a T>,
    pub params: Vec<&'a T>,
    /// The index in `params` of the vararg parameter, whose type is its array type.
    pub vararg: Option<usize>,
    pub type_parameters: Vec<TypeParameterShape<'a, T>>,
}

/// A property.
pub struct PropertyShape<'a, T> {
    pub name: &'a str,
    pub contexts: Vec<&'a T>,
    pub receiver: Option<&'a T>,
    pub type_parameters: Vec<TypeParameterShape<'a, T>>,
}

/// Which accessor of a property.
pub enum Accessor<'a, T> {
    Getter,
    /// A setter takes the property's type.
    Setter {
        value: &'a T,
    },
}

/// A declaration's mangled signature could not be formed from its shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManglingError {
    detail: String,
}

impl ManglingError {
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for ManglingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for ManglingError {}

/// The identity of a class, interface, object, enum or annotation class: a path, no member id.
pub fn class_signature<T>(
    container: DeclarationContainer<'_, T>,
    name: &str,
) -> KlibPublicIdSignature {
    container.signature(name, None)
}

/// The identity of a function or constructor.
pub fn callable_signature<T: SignatureType>(
    container: DeclarationContainer<'_, T>,
    callable: &CallableShape<'_, T>,
) -> Result<KlibPublicIdSignature, ManglingError> {
    let mangled = mangle_callable(&container.scopes(&callable.type_parameters), callable)?;
    Ok(container.signature(callable.name, Some(&mangled)))
}

/// The identity of a property.
pub fn property_signature<T: SignatureType>(
    container: DeclarationContainer<'_, T>,
    property: &PropertyShape<'_, T>,
) -> Result<KlibPublicIdSignature, ManglingError> {
    let mangled = mangle_property(&container.scopes(&property.type_parameters), property)?;
    Ok(container.signature(property.name, Some(&mangled)))
}

/// The identity of a property's getter or setter. An accessor is mangled as a function named
/// `<get-name>` or `<set-name>` with the property's contexts, receiver and type parameters.
pub fn accessor_signature<T: SignatureType>(
    container: DeclarationContainer<'_, T>,
    property: &PropertyShape<'_, T>,
    accessor: Accessor<'_, T>,
) -> Result<KlibAccessorIdSignature, ManglingError> {
    let (name, params) = match accessor {
        Accessor::Getter => (format!("<get-{}>", property.name), Vec::new()),
        Accessor::Setter { value } => (format!("<set-{}>", property.name), vec![value]),
    };
    let type_parameters = property
        .type_parameters
        .iter()
        .map(|parameter| TypeParameterShape {
            name: parameter.name,
            bounds: parameter.bounds.clone(),
        })
        .collect();
    let function = CallableShape {
        name: &name,
        contexts: property.contexts.clone(),
        receiver: property.receiver,
        params,
        vararg: None,
        type_parameters,
    };
    let scopes = container.scopes(&function.type_parameters);
    let mangled = mangle_callable(&scopes, &function)?;
    Ok(KlibAccessorIdSignature::new(
        property_signature(container, property)?,
        name,
        city_hash::city_hash64(mangled.as_bytes()),
        0,
    ))
}

type Scopes<'s, 'a, T> = [&'s [TypeParameterShape<'a, T>]];

fn mangle_callable<T: SignatureType>(
    scopes: &Scopes<'_, '_, T>,
    callable: &CallableShape<'_, T>,
) -> Result<String, ManglingError> {
    let mut out = String::from(callable.name);
    contexts(&callable.contexts, scopes, &mut out)?;
    if let Some(receiver) = callable.receiver {
        out.push('@');
        ty(receiver, scopes, &mut out)?;
    }
    out.push('(');
    for (index, parameter) in callable.params.iter().enumerate() {
        if index > 0 {
            out.push(';');
        }
        ty(*parameter, scopes, &mut out)?;
        if callable.vararg == Some(index) {
            out.push_str("...");
        }
    }
    out.push(')');
    type_parameters(&callable.type_parameters, scopes, &mut out)?;
    Ok(out)
}

fn mangle_property<T: SignatureType>(
    scopes: &Scopes<'_, '_, T>,
    property: &PropertyShape<'_, T>,
) -> Result<String, ManglingError> {
    let mut out = String::new();
    contexts(&property.contexts, scopes, &mut out)?;
    if let Some(receiver) = property.receiver {
        out.push('@');
        ty(receiver, scopes, &mut out)?;
    }
    type_parameters(&property.type_parameters, scopes, &mut out)?;
    out.push_str(property.name);
    Ok(out)
}

/// Context parameters are written only when there are any.
fn contexts<T: SignatureType>(
    contexts: &[&T],
    scopes: &Scopes<'_, '_, T>,
    out: &mut String,
) -> Result<(), ManglingError> {
    if contexts.is_empty() {
        return Ok(());
    }
    out.push('(');
    for (index, context) in contexts.iter().enumerate() {
        if index > 0 {
            out.push(';');
        }
        ty(*context, scopes, out)?;
    }
    out.push(')');
    Ok(())
}

/// The type-parameter list is always written, even when empty.
fn type_parameters<T: SignatureType>(
    parameters: &[TypeParameterShape<'_, T>],
    scopes: &Scopes<'_, '_, T>,
    out: &mut String,
) -> Result<(), ManglingError> {
    out.push('{');
    for (index, parameter) in parameters.iter().enumerate() {
        if index > 0 {
            out.push(';');
        }
        out.push_str(&index.to_string());
        out.push_str("§<");
        if parameter.bounds.is_empty() {
            out.push_str("kotlin.Any?");
        }
        for (bound_index, bound) in parameter.bounds.iter().enumerate() {
            if bound_index > 0 {
                out.push('&');
            }
            ty(*bound, scopes, out)?;
        }
        out.push('>');
    }
    out.push('}');
    Ok(())
}

fn ty<T: SignatureType>(
    ty_ref: &T,
    scopes: &Scopes<'_, '_, T>,
    out: &mut String,
) -> Result<(), ManglingError> {
    match ty_ref.view()? {
        TypeView::Class {
            fq_name,
            arguments,
            nullable,
        } => {
            out.push_str(&fq_name);
            if !arguments.is_empty() {
                out.push('<');
                for (index, argument) in arguments.into_iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    ty(argument, scopes, out)?;
                }
                out.push('>');
            }
            if nullable {
                out.push('?');
            }
        }
        TypeView::Parameter { name, nullable } => {
            // Source cannot name a shadowed type parameter, so the innermost one wins.
            let (container, index) = scopes
                .iter()
                .enumerate()
                .find_map(|(container, scope)| {
                    scope
                        .iter()
                        .position(|parameter| parameter.name == name)
                        .map(|index| (container, index))
                })
                .ok_or_else(|| {
                    ManglingError::new(format!("type parameter {name} is not in scope"))
                })?;
            out.push_str(&format!("{container}:{index}"));
            if nullable {
                out.push('?');
            }
        }
        TypeView::In(inner) => {
            out.push_str("in|");
            ty(inner, scopes, out)?;
        }
        TypeView::Out(inner) => {
            out.push_str("out|");
            ty(inner, scopes, out)?;
        }
        TypeView::Star => out.push('*'),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A declaration model of its own, unrelated to decoded metadata.
    enum Model {
        Class(&'static str, Vec<Model>),
        Parameter(&'static str),
    }

    impl SignatureType for Model {
        fn view(&self) -> Result<TypeView<'_, Self>, ManglingError> {
            Ok(match self {
                Model::Class(fq_name, arguments) => TypeView::Class {
                    fq_name: Cow::Borrowed(fq_name),
                    arguments: arguments.iter().collect(),
                    nullable: false,
                },
                Model::Parameter(name) => TypeView::Parameter {
                    name,
                    nullable: false,
                },
            })
        }
    }

    #[test]
    fn any_declaration_model_mangles_through_its_type_view() {
        // `kotlin.comparisons.maxOf<T : Comparable<T>>(T, T)` in the 2.4.20 stdlib KLIB.
        let bound = Model::Class("kotlin.Comparable", vec![Model::Parameter("T")]);
        let t = Model::Parameter("T");
        let package = ["kotlin".to_owned(), "comparisons".to_owned()];
        let max_of = CallableShape {
            name: "maxOf",
            contexts: Vec::new(),
            receiver: None,
            params: vec![&t, &t],
            vararg: None,
            type_parameters: vec![TypeParameterShape {
                name: "T",
                bounds: vec![&bound],
            }],
        };
        let signature = callable_signature(
            DeclarationContainer {
                package: &package,
                classes: &[],
            },
            &max_of,
        )
        .unwrap();
        assert_eq!(signature.member_id(), Some(0xba31_5fe0_bd1b_9796));
        assert_eq!(
            signature.member_id().map(|id| id as i64),
            Some(-5_030_133_889_946_118_250)
        );
    }
}

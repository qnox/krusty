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
//! property is `(context types)`? `@receiver`? `{type parameters}` `name`. A class member without a
//! dispatch receiver carries `#static`, after a function's name or before a property's other parts. A result type is never
//! written for the declarations a KLIB links by public signature.
//!
//! A signature's mask carries Kotlin's `IdSignature.Flags`. The two a public KLIB declaration can
//! have are recursive: `IS_EXPECT` when the declaration or a class around it is `expect`, and
//! `IS_NATIVE_INTEROP_LIBRARY` for every declaration of a C-interop library. The declaration model
//! supplies both facts; the mangler never assumes them.

use std::borrow::Cow;

use super::{city_hash, KlibAccessorIdSignature, KlibNamePath, KlibPublicIdSignature};

/// `IdSignature.Flags.IS_EXPECT`.
const IS_EXPECT: u64 = 1 << 0;
/// `IdSignature.Flags.IS_NATIVE_INTEROP_LIBRARY`.
const IS_NATIVE_INTEROP_LIBRARY: u64 = 1 << 2;

/// `MangleConstant.STATIC_MEMBER_MARK`.
const STATIC_MEMBER: &str = "#static";

/// `MangleConstant.COMPANION_EXTENSION_MARK`, followed by `@` and the extended class's id.
const COMPANION_EXTENSION: &str = "#companion@";

/// How a declaration is reached, which the mangler marks before its contexts and receiver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Placement<'a> {
    /// A top-level declaration, or a class member with a dispatch receiver.
    Ordinary,
    /// A class member without a dispatch receiver: a `companion { … }` block member, or an enum
    /// class's `values`, `valueOf` or `entries`.
    Static,
    /// A companion extension (`companion fun C.name`) of the class whose `ClassId` string
    /// (`package/path/Outer.Inner`) this is. It takes the place of an extension receiver.
    CompanionExtension { class_id: &'a str },
}

/// A type as the signature mangler sees it.
pub trait SignatureType: Sized {
    /// The identity of a type parameter's declaration. Mangling locates a referenced parameter
    /// by this identity, never by its spelling.
    type ParameterId: Copy + Eq + std::fmt::Debug;

    fn view(&self) -> Result<TypeView<'_, Self>, ManglingError>;
}

/// One level of a type.
pub enum TypeView<'a, T: SignatureType> {
    Class {
        /// Package and class names joined with `.` (`kotlin.collections.Map.Entry`).
        fq_name: Cow<'a, str>,
        arguments: Vec<&'a T>,
        nullable: bool,
    },
    /// A reference to a type parameter in scope.
    Parameter {
        id: T::ParameterId,
        nullable: bool,
    },
    In(&'a T),
    Out(&'a T),
    Star,
}

/// A declared type parameter.
pub struct TypeParameterShape<'a, T: SignatureType> {
    pub id: T::ParameterId,
    /// Declared upper bounds; empty means the default `Any?`.
    pub bounds: Vec<&'a T>,
}

impl<T: SignatureType> Clone for TypeParameterShape<'_, T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            bounds: self.bounds.clone(),
        }
    }
}

/// A class: the declaration a class signature names, and a scope around nested declarations.
pub struct ClassScope<'a, T: SignatureType> {
    pub name: &'a str,
    pub type_parameters: Vec<TypeParameterShape<'a, T>>,
    pub expect: bool,
}

/// Where a declaration sits: its package and the classes around it, outermost first.
pub struct DeclarationContainer<'a, T: SignatureType> {
    pub package: &'a [String],
    pub classes: &'a [ClassScope<'a, T>],
    /// The declaration belongs to a C-interop library.
    pub native_interop_library: bool,
}

impl<T: SignatureType> Clone for DeclarationContainer<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: SignatureType> Copy for DeclarationContainer<'_, T> {}

impl<'a, T: SignatureType> DeclarationContainer<'a, T> {
    fn signature(&self, name: &str, mangled: Option<&str>, expect: bool) -> KlibPublicIdSignature {
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
            mask: self.mask(expect),
        }
    }

    /// The recursive flags: a declaration inside an `expect` class is itself expected.
    fn mask(&self, expect: bool) -> u64 {
        let mut mask = 0;
        if expect || self.classes.iter().any(|class| class.expect) {
            mask |= IS_EXPECT;
        }
        if self.native_interop_library {
            mask |= IS_NATIVE_INTEROP_LIBRARY;
        }
        mask
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
pub struct CallableShape<'a, T: SignatureType> {
    /// `<init>` for a constructor.
    pub name: &'a str,
    pub contexts: Vec<&'a T>,
    pub receiver: Option<&'a T>,
    pub params: Vec<&'a T>,
    /// The index in `params` of the vararg parameter, whose type is its array type.
    pub vararg: Option<usize>,
    pub type_parameters: Vec<TypeParameterShape<'a, T>>,
    pub expect: bool,
    pub placement: Placement<'a>,
}

/// A property.
pub struct PropertyShape<'a, T: SignatureType> {
    pub name: &'a str,
    pub contexts: Vec<&'a T>,
    pub receiver: Option<&'a T>,
    pub type_parameters: Vec<TypeParameterShape<'a, T>>,
    pub expect: bool,
    pub placement: Placement<'a>,
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
pub fn class_signature<T: SignatureType>(
    container: DeclarationContainer<'_, T>,
    class: &ClassScope<'_, T>,
) -> KlibPublicIdSignature {
    container.signature(class.name, None, class.expect)
}

/// The identity of an enum entry; `container` ends with its enum class. An entry carries no
/// flags of its own, only those of the classes around it.
pub fn enum_entry_signature<T: SignatureType>(
    container: DeclarationContainer<'_, T>,
    name: &str,
) -> KlibPublicIdSignature {
    container.signature(name, None, false)
}

/// The identity of a function or constructor.
pub fn callable_signature<T: SignatureType>(
    container: DeclarationContainer<'_, T>,
    callable: &CallableShape<'_, T>,
) -> Result<KlibPublicIdSignature, ManglingError> {
    let mangled = mangle_callable(&container.scopes(&callable.type_parameters), callable)?;
    Ok(container.signature(callable.name, Some(&mangled), callable.expect))
}

/// The identity of a property.
pub fn property_signature<T: SignatureType>(
    container: DeclarationContainer<'_, T>,
    property: &PropertyShape<'_, T>,
) -> Result<KlibPublicIdSignature, ManglingError> {
    let mangled = mangle_property(&container.scopes(&property.type_parameters), property)?;
    Ok(container.signature(property.name, Some(&mangled), property.expect))
}

/// The identity of a property's getter or setter. An accessor is mangled as a function named
/// `<get-name>` or `<set-name>` with the property's contexts, receiver and type parameters, and
/// shares the property's flags.
pub fn accessor_signature<T: SignatureType>(
    container: DeclarationContainer<'_, T>,
    property: &PropertyShape<'_, T>,
    accessor: Accessor<'_, T>,
) -> Result<KlibAccessorIdSignature, ManglingError> {
    let (name, params) = match accessor {
        Accessor::Getter => (format!("<get-{}>", property.name), Vec::new()),
        Accessor::Setter { value } => (format!("<set-{}>", property.name), vec![value]),
    };
    let function = CallableShape {
        name: &name,
        contexts: property.contexts.clone(),
        receiver: property.receiver,
        params,
        vararg: None,
        type_parameters: property.type_parameters.clone(),
        expect: property.expect,
        placement: property.placement,
    };
    let mangled = mangle_callable(&container.scopes(&function.type_parameters), &function)?;
    let property_signature = property_signature(container, property)?;
    let mask = property_signature.mask;
    Ok(KlibAccessorIdSignature::new(
        property_signature,
        name,
        city_hash::city_hash64(mangled.as_bytes()),
        mask,
    ))
}

type Scopes<'s, 'a, T> = [&'s [TypeParameterShape<'a, T>]];

fn mangle_callable<T: SignatureType>(
    scopes: &Scopes<'_, '_, T>,
    callable: &CallableShape<'_, T>,
) -> Result<String, ManglingError> {
    if let Some(vararg) = callable.vararg {
        if vararg >= callable.params.len() {
            return Err(ManglingError::new(format!(
                "{} marks parameter {vararg} as vararg but has {} value parameters",
                callable.name,
                callable.params.len()
            )));
        }
    }
    let mut out = String::from(callable.name);
    placement(callable.placement, callable.receiver, &mut out)?;
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
    placement(property.placement, property.receiver, &mut out)?;
    contexts(&property.contexts, scopes, &mut out)?;
    if let Some(receiver) = property.receiver {
        out.push('@');
        ty(receiver, scopes, &mut out)?;
    }
    type_parameters(&property.type_parameters, scopes, &mut out)?;
    out.push_str(property.name);
    Ok(out)
}

fn placement<T>(
    placement: Placement<'_>,
    receiver: Option<&T>,
    out: &mut String,
) -> Result<(), ManglingError> {
    match placement {
        Placement::Ordinary => {}
        Placement::Static => out.push_str(STATIC_MEMBER),
        Placement::CompanionExtension { .. } if receiver.is_some() => {
            return Err(ManglingError::new(
                "a companion extension has no extension receiver of its own",
            ));
        }
        Placement::CompanionExtension { class_id } => {
            out.push_str(COMPANION_EXTENSION);
            out.push_str(class_id);
        }
    }
    Ok(())
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
        TypeView::Parameter { id, nullable } => {
            let (container, index) = scopes
                .iter()
                .enumerate()
                .find_map(|(container, scope)| {
                    scope
                        .iter()
                        .position(|parameter| parameter.id == id)
                        .map(|index| (container, index))
                })
                .ok_or_else(|| {
                    ManglingError::new(format!("type parameter {id:?} is not in scope"))
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
mod tests;

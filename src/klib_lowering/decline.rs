//! Why a KLIB body is not lowered.
//!
//! Lowering models a closed set of KLIB IR forms. Anything outside that set declines with the
//! form's name, so a backend reports exactly what keeps a dependency body from compiling instead
//! of compiling something else in its place.

use crate::libraries::KlibDeclarationSignature;
use crate::metadata::id_signature::KlibPublicIdSignature;

/// A selected KLIB declaration whose body this lowering does not produce.
#[derive(Clone, Debug, PartialEq)]
pub struct KlibBodyDecline {
    declaration: Box<KlibDeclarationSignature>,
    reason: KlibBodyDeclineReason,
}

/// What keeps a body from lowering.
#[derive(Clone, Debug, PartialEq)]
pub enum KlibBodyDeclineReason {
    /// No library of the set serializes a declaration under the selected signature.
    Unjoined,
    /// The library serializes the declaration without a body.
    NoBody,
    /// The body is one the backend generates (an enum's `values`, `valueOf` or `entries`).
    SyntheticBody,
    /// The declaration has a shape lowering does not model, such as type parameters.
    UnsupportedDeclaration(&'static str),
    /// The body uses an expression or statement form lowering does not model, by its name.
    UnsupportedOperation(&'static str),
    /// The body calls a declaration lowering cannot realize.
    UnsupportedCallee(Box<KlibPublicIdSignature>),
    /// The body calls a declaration no loaded library declares, so nothing describes the callee.
    UndeclaredCallee(Box<KlibPublicIdSignature>),
    /// The body calls a dependency declaration that two frozen selections describe.
    AmbiguousCallee(Box<KlibPublicIdSignature>),
    /// The body reaches, through calls, a dependency body that declines.
    CalleeDeclined(Box<KlibBodyDecline>),
    /// The body names a type lowering does not model, by its form.
    UnsupportedType(&'static str),
    /// A value flows into a place of a different type, which the body leaves to an implicit
    /// conversion lowering does not model.
    ImplicitConversion,
    /// A `return` leaves a declaration other than the one being lowered.
    ForeignReturnTarget,
    /// The body reads a value it does not declare.
    ForeignValue,
    /// The body of a function whose result is not `Unit` can reach its end without returning.
    MissingReturn,
    /// An expression of the given form carries no type.
    UntypedExpression(&'static str),
    /// The serialized declaration disagrees with the selected declaration. Both describe one
    /// identity, so this is an internal inconsistency, never a user error.
    SignatureMismatch(String),
}

impl KlibBodyDecline {
    pub(super) fn new(
        declaration: KlibDeclarationSignature,
        reason: KlibBodyDeclineReason,
    ) -> Self {
        Self {
            declaration: Box::new(declaration),
            reason,
        }
    }

    pub fn declaration(&self) -> &KlibDeclarationSignature {
        &self.declaration
    }

    pub fn reason(&self) -> &KlibBodyDeclineReason {
        &self.reason
    }
}

/// The phrase names what is unsupported, for a backend's own wording: the Native backend reports
/// it as "krusty: the native backend does not support {decline} yet".
impl std::fmt::Display for KlibBodyDecline {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "the KLIB body of `")?;
        match self.declaration.as_ref() {
            KlibDeclarationSignature::Public(signature) => {
                write_qualified(formatter, signature)?;
            }
            KlibDeclarationSignature::Accessor(accessor) => {
                write_qualified(formatter, accessor.property())?;
                write!(formatter, ".{}", accessor.name())?;
            }
        }
        write!(formatter, "` (")?;
        match &self.reason {
            KlibBodyDeclineReason::Unjoined => {
                write!(formatter, "no library serializes its declaration")
            }
            KlibBodyDeclineReason::NoBody => write!(formatter, "its library serializes none"),
            KlibBodyDeclineReason::SyntheticBody => {
                write!(formatter, "the backend generates it")
            }
            KlibBodyDeclineReason::UnsupportedDeclaration(what) => {
                write!(formatter, "it declares {what}")
            }
            KlibBodyDeclineReason::UnsupportedOperation(what) => {
                write!(formatter, "it uses {what}")
            }
            KlibBodyDeclineReason::UnsupportedCallee(callee) => {
                write!(formatter, "it calls `")?;
                write_qualified(formatter, callee)?;
                write!(formatter, "`")
            }
            KlibBodyDeclineReason::UndeclaredCallee(callee) => {
                write!(formatter, "it calls `")?;
                write_qualified(formatter, callee)?;
                write!(formatter, "`, which no loaded library declares")
            }
            KlibBodyDeclineReason::AmbiguousCallee(callee) => {
                write!(formatter, "it calls `")?;
                write_qualified(formatter, callee)?;
                write!(formatter, "`, which two selected declarations describe")
            }
            KlibBodyDeclineReason::CalleeDeclined(callee) => {
                write!(formatter, "it reaches {callee}")
            }
            KlibBodyDeclineReason::UnsupportedType(what) => {
                write!(formatter, "it uses {what}")
            }
            KlibBodyDeclineReason::ImplicitConversion => {
                write!(formatter, "it converts a value implicitly")
            }
            KlibBodyDeclineReason::ForeignReturnTarget => {
                write!(formatter, "it returns from another declaration")
            }
            KlibBodyDeclineReason::ForeignValue => {
                write!(formatter, "it reads a value it does not declare")
            }
            KlibBodyDeclineReason::MissingReturn => {
                write!(formatter, "it ends without a `return`")
            }
            KlibBodyDeclineReason::UntypedExpression(what) => {
                write!(formatter, "its {what} has no type")
            }
            KlibBodyDeclineReason::SignatureMismatch(detail) => {
                write!(
                    formatter,
                    "its serialized declaration disagrees with the selected one: {detail}"
                )
            }
        }?;
        write!(formatter, ")")
    }
}

/// A public signature's package and declaration path, joined for a diagnostic.
fn write_qualified(
    formatter: &mut std::fmt::Formatter<'_>,
    signature: &KlibPublicIdSignature,
) -> std::fmt::Result {
    let segments = signature
        .package()
        .segments()
        .iter()
        .chain(signature.declaration().segments());
    for (index, segment) in segments.enumerate() {
        if index > 0 {
            write!(formatter, ".")?;
        }
        write!(formatter, "{segment}")?;
    }
    Ok(())
}

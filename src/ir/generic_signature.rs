//! The backend-agnostic generic signature of a declaration and its type-parameter entries.

use crate::types::Ty;

/// Backend-agnostic generic-signature shape of a declaration (the data a JVM `Signature` / a future
/// platform's equivalent needs). NO target descriptors here — each backend formats its own.
#[derive(Clone, Debug)]
pub struct IrGenericSig {
    /// Each declared type parameter with its complete semantic bound shape. Whether that bound is an
    /// interface is declaration metadata, not something a backend may infer from a physical name.
    pub type_params: Vec<IrTypeParameter>,
    /// Complete semantic callable parameters for a function signature. An extension receiver occupies
    /// its physical context-boundary slot so a backend signature retains its declared generic type;
    /// declaration metadata separates that slot back into a receiver. Empty for a class signature.
    pub params: Vec<Ty>,
    /// Complete semantic return type for a function signature. `None` for a class signature.
    pub ret: Option<Ty>,
    /// For a CLASS signature with a PARAMETERIZED supertype: the superclass + superinterfaces as
    /// platform-agnostic `Ty`s carrying their type arguments (`[Any, Operation<Result<Int>>]`), so a
    /// cross-module reader recovers a member's concrete generic return. The backend formats these into the
    /// JVM `Signature` string. Empty ⇒ no parameterized supertype (backend emits the default `Object`
    /// superclass). Empty for a function signature.
    pub supers: Vec<Ty>,
}

#[derive(Clone, Debug)]
pub struct IrTypeParameter {
    pub name: String,
    pub semantic_name: String,
    pub bounds: Vec<(Ty, bool)>,
    pub variance: crate::types::TypeVariance,
    /// Kotlin declaration capability retained for targets that materialize runtime type operations.
    /// The JVM consumes it when choosing its reified-operation marker representation.
    pub reified: bool,
    /// This entry is a type parameter a lifted local or anonymous function captures from an
    /// enclosing declaration, signed ahead of its own. It keeps that declaration's identity and
    /// reified capability; the function whose signature lists it does not declare it.
    pub captured_from_enclosing: bool,
}

impl IrTypeParameter {
    /// A use of this parameter as a type.
    pub fn ty(&self) -> Ty {
        Ty::ty_param(&self.semantic_name, self.upper_bound())
    }

    /// The representative upper bound: the first declared bound, else `Any?`.
    pub fn upper_bound(&self) -> Ty {
        self.bounds
            .first()
            .map_or(Ty::nullable(Ty::obj("kotlin/Any")), |(bound, _)| *bound)
    }
}

impl IrGenericSig {
    /// The type parameters the signed declaration itself declares, without the ones a lifted
    /// local or anonymous function captures from an enclosing declaration.
    pub fn declared_type_parameters(&self) -> impl Iterator<Item = &IrTypeParameter> {
        self.type_params
            .iter()
            .filter(|parameter| !parameter.captured_from_enclosing)
    }
}

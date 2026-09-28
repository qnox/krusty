//! How a generic `Signature` spells a type's arguments.
//!
//! Kotlin declaration-site variance becomes a wildcard where the position calls for one, and an
//! explicit projection keeps its own direction. The JVM has no spelling for `Nothing` as a type
//! argument, so kotlinc writes a type raw when one of its own arguments is `Nothing?`, or `Nothing`
//! for a type parameter not declared `in`: `Box<Nothing>` signs as `LBox;`, and
//! `Inv<List<Nothing?>>` as `LInv<Ljava/util/List;>;`. A `Nothing` for an `in` parameter is a star
//! (`Sink<*>`). A class signature left with no generic structure is omitted.

use super::{JvmSignatureFormatter, Wildcards};
use crate::types::{Ty, TypeName, TypeVariance};

impl JvmSignatureFormatter<'_> {
    pub(super) fn type_argument(
        &self,
        declaration: TypeVariance,
        argument: Ty,
        wildcards: Wildcards,
    ) -> Option<String> {
        // In a return-like position a projection that only restates the parameter's declared
        // variance is declaration-site variance too, and is not written.
        let restated = |projection: TypeVariance| {
            wildcards == Wildcards::Suppressed && declaration == projection
        };
        match argument {
            Ty::StarProjection(_) => Some("*".to_string()),
            argument if is_star(declaration, argument) => Some("*".to_string()),
            Ty::InProjection(inner) if restated(TypeVariance::In) => {
                self.ty_at(inner, wildcards.for_argument(TypeVariance::In))
            }
            Ty::OutProjection(inner) if restated(TypeVariance::Out) => {
                self.ty_at(inner, wildcards.for_argument(TypeVariance::Out))
            }
            Ty::InProjection(inner) => Some(format!(
                "-{}",
                self.ty_at(inner, wildcards.for_argument(TypeVariance::In))?
            )),
            Ty::OutProjection(inner) => Some(format!(
                "+{}",
                self.ty_at(inner, wildcards.for_argument(TypeVariance::Out))?
            )),
            argument => {
                let mut signature = String::new();
                if wildcards.writes_declaration_site()
                    && !(wildcards.drops_redundant()
                        && self.wildcard_is_redundant(declaration, argument)?)
                {
                    match declaration {
                        TypeVariance::In => signature.push('-'),
                        TypeVariance::Out => signature.push('+'),
                        TypeVariance::Invariant => {}
                    }
                }
                signature.push_str(&self.ty_at(&argument, wildcards.for_argument(declaration))?);
                Some(signature)
            }
        }
    }

    /// Whether `owner<arguments>` is written raw. `None` once an emit error is recorded for a
    /// classifier whose type parameters are unknown.
    pub(super) fn is_written_raw(&self, owner: TypeName, arguments: &[Ty]) -> Option<bool> {
        if arguments.is_empty() {
            return Some(false);
        }
        let chain = self.classifier_signature_chain(owner)?;
        let declared = chain.iter().map(|(_, variances)| variances.len()).sum();
        let arguments = self.classifier_usage_arguments(owner, arguments, declared)?;
        let variances = chain
            .iter()
            .flat_map(|(_, variances)| variances.iter().copied());
        for (variance, argument) in variances.zip(arguments) {
            let argument = match *argument {
                Ty::StarProjection(_) => continue,
                Ty::InProjection(inner) | Ty::OutProjection(inner) => *inner,
                argument => argument,
            };
            let nullable_nothing = matches!(
                argument,
                Ty::Nullable(inner) | Ty::PlatformNullable(inner) if *inner == Ty::Nothing
            );
            if nullable_nothing || (argument == Ty::Nothing && variance != TypeVariance::In) {
                return Some(true);
            }
        }
        Some(false)
    }
}

/// Whether an argument for a parameter declared with `variance` is written as a star.
pub(super) fn is_star(variance: TypeVariance, argument: Ty) -> bool {
    let argument = match argument {
        Ty::InProjection(inner) | Ty::OutProjection(inner) => *inner,
        argument => argument,
    };
    argument == Ty::Nothing && variance == TypeVariance::In
}

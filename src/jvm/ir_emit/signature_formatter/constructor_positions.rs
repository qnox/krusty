//! A constructor's generic `Signature`, written as kotlinc's `MethodSignatureMapper` writes it.
//!
//! Two kinds of JVM parameter stay out of the signature: the outer instance an inner class takes
//! first, and the name and ordinal an enum constructor takes first. Both still decide whether the
//! attribute is written at all (`BothSignatureWriter`). The enum parameters always do, as javac's
//! do, so every enum constructor signs its source parameters (`()V` for none). The outer instance is
//! written into a discarded visitor, so it does when its type writes a type argument: an inner class
//! of a generic class signs `()V` even when it declares no parameter of its own.

use super::JvmSignatureFormatter;
use crate::types::{Ty, TypeName};

/// One JVM constructor parameter, as the signature writer sees it.
pub(in crate::jvm::ir_emit) enum ConstructorParameter {
    /// A parameter the signature spells: its semantic type and the descriptor it is carried as.
    Regular { ty: Ty, descriptor: String },
    /// The outer instance of an inner class, of the given classifier applied to its own type
    /// parameters.
    OuterInstance(TypeName),
    /// The name or ordinal of an enum constructor.
    EnumSynthetic,
}

impl JvmSignatureFormatter<'_> {
    /// `(params)V`, or `None` when nothing the writer visited was generic.
    pub(in crate::jvm::ir_emit) fn constructor_signature(
        &self,
        parameters: &[ConstructorParameter],
    ) -> Option<String> {
        let mut generic = false;
        let mut signature = String::from("(");
        for parameter in parameters {
            match parameter {
                ConstructorParameter::Regular { ty, descriptor } => {
                    let written = self.parameter_ty(ty)?;
                    generic |= written != *descriptor;
                    signature.push_str(&written);
                }
                ConstructorParameter::OuterInstance(outer) => {
                    generic |= self
                        .classifier_signature_chain(*outer)?
                        .iter()
                        .any(|(_, variances)| !variances.is_empty());
                }
                ConstructorParameter::EnumSynthetic => generic = true,
            }
        }
        signature.push_str(")V");
        generic.then_some(signature)
    }
}

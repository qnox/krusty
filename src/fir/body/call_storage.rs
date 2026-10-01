//! Heap-payload accounting for a checked call and its retained semantic target facts.

use super::*;

impl FirCall {
    pub(super) fn storage_payload_bytes(&self) -> usize {
        let external_parameters = match &self.target {
            FirCallTarget::Module(_) => 0,
            FirCallTarget::External {
                receiver,
                declared_receiver,
                parameters,
                declared_result,
                overridden_results,
                ..
            } => {
                (parameters.len() + overridden_results.len()) * std::mem::size_of::<ResolvedTy>()
                    + usize::from(receiver.is_some()) * std::mem::size_of::<ResolvedTy>()
                    + usize::from(declared_receiver.is_some()) * std::mem::size_of::<ResolvedTy>()
                    + usize::from(declared_result.is_some()) * std::mem::size_of::<ResolvedTy>()
            }
            FirCallTarget::Super {
                parameters,
                name,
                descriptor,
                ..
            } => {
                parameters.len() * std::mem::size_of::<ResolvedTy>() + name.len() + descriptor.len()
            }
            FirCallTarget::Intrinsic {
                receiver,
                parameters,
                ..
            } => {
                parameters.len() * std::mem::size_of::<ResolvedTy>()
                    + usize::from(receiver.is_some()) * std::mem::size_of::<ResolvedTy>()
            }
            FirCallTarget::Classifier { parameters, .. } => {
                parameters.len() * std::mem::size_of::<ResolvedTy>()
            }
        };
        external_parameters
            + self.parameter_types.len() * std::mem::size_of::<ResolvedTy>()
            + self.arguments.len() * std::mem::size_of::<FirCallArgument>()
            + self
                .arguments
                .iter()
                .map(FirCallArgument::storage_payload_bytes)
                .sum::<usize>()
            + self.substitutions.len() * std::mem::size_of::<FirTypeSubstitution>()
    }
}

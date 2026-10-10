//! What the signature solver recorded about each selected top-level call, by call origin.
//!
//! Selection happens once per evaluation of a call; later graph nodes ask about the selected
//! callable without selecting it again: a [`crate::fir::SigExpr::ContractNarrowed`] read asks what
//! the call's contract proved, and a declaration's inferred result asks which typealias the call
//! constructed through.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::contracts::{Condition, Contract, Effect, ParamRef, ReturnsValue};
use crate::fir::{OriginId, ResolvedTypeAbbreviation};

#[derive(Default)]
pub(super) struct SelectedCallFacts {
    contracts: RefCell<HashMap<OriginId, SelectedCallContract>>,
    abbreviations: RefCell<HashMap<OriginId, ResolvedTypeAbbreviation>>,
}

struct SelectedCallContract {
    contract: std::sync::Arc<Contract>,
    /// Selected declaration parameter for each argument in source order. The contract refers to
    /// declaration parameters, while the compact graph retains source argument order.
    parameter_by_argument: Box<[Option<u32>]>,
}

impl SelectedCallFacts {
    pub(super) fn record_contract(
        &self,
        origin: OriginId,
        contract: std::sync::Arc<Contract>,
        parameter_by_argument: Box<[Option<u32>]>,
    ) {
        self.contracts.borrow_mut().insert(
            origin,
            SelectedCallContract {
                contract,
                parameter_by_argument,
            },
        );
    }

    /// Whether the call's contract has a `returns() implies (<parameter> != null)` effect for
    /// source `argument`, or `returns() implies <parameter>` for an argument spelled
    /// `value != null` (`condition`).
    pub(super) fn proves_argument_non_null(
        &self,
        origin: OriginId,
        argument: u32,
        condition: bool,
    ) -> bool {
        fn proves(conclusion: &Condition, argument: u32, condition: bool) -> bool {
            match conclusion {
                Condition::IsNull {
                    param: ParamRef::Param(index),
                    negated: true,
                } => !condition && *index == argument as usize,
                // `returns() implies actual`: the boolean argument itself holds, and the
                // extractor only takes this shape for an argument spelled `value != null`.
                Condition::BoolParam {
                    param: ParamRef::Param(index),
                    negated: false,
                } => condition && *index == argument as usize,
                Condition::And(left, right) => {
                    proves(left, argument, condition) || proves(right, argument, condition)
                }
                _ => false,
            }
        }
        let contracts = self.contracts.borrow();
        let Some(selected) = contracts.get(&origin) else {
            return false;
        };
        let Some(argument) = selected
            .parameter_by_argument
            .get(argument as usize)
            .copied()
            .flatten()
        else {
            return false;
        };
        selected.contract.effects.iter().any(|effect| {
            matches!(
                effect,
                Effect::ConditionalReturns {
                    returns: ReturnsValue::Any,
                    conclusion,
                } if proves(conclusion, argument, condition)
            )
        })
    }

    /// A call is being selected (again): whatever abbreviation an earlier selection recorded no
    /// longer describes it.
    pub(super) fn forget_abbreviation(&self, origin: OriginId) {
        self.abbreviations.borrow_mut().remove(&origin);
    }

    pub(super) fn record_abbreviation(
        &self,
        origin: OriginId,
        abbreviation: ResolvedTypeAbbreviation,
    ) {
        self.abbreviations.borrow_mut().insert(origin, abbreviation);
    }

    pub(super) fn abbreviation(&self, origin: OriginId) -> Option<ResolvedTypeAbbreviation> {
        self.abbreviations.borrow().get(&origin).cloned()
    }
}

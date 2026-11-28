//! The backend facts the value-class pass records beside the IR it rewrites.

use crate::bridge_adaptations::BridgeAdaptations;
use crate::default_call_operands::DefaultCallOperands;
use crate::override_results::OverrideResults;
use crate::property_references::PropertyReferenceRealizations;

pub(crate) struct RealizationFacts<'a> {
    pub(crate) bridge_adaptations: &'a mut BridgeAdaptations,
    pub(crate) override_results: &'a mut OverrideResults,
    /// What the property-reference pass already selected for each synthesized reference class.
    /// Realizing those accessors over a carrier is the one thing left to decide about them, and it
    /// is decided from these recorded facts rather than from the reference's spelling.
    pub(crate) property_reference_realizations: &'a mut PropertyReferenceRealizations,
    /// The operand plans of the default calls the pass realizes itself: a construction of a value
    /// class another source file declares (see `default_constructions`).
    pub(crate) default_call_operands: &'a mut DefaultCallOperands,
}

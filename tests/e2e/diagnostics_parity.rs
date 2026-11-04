//! Exact diagnostic parity with kotlinc, one suite per diagnostic family, grouped under the bounded
//! e2e test target. Every suite compares complete diagnostics through the observations in
//! `diagnostics_parity_support`.

pub(super) use super::common;
pub(super) use super::diagnostics_parity_support;

#[path = "../diagnostics_call_mapping_cases_e2e.rs"]
mod diagnostics_call_mapping_cases_e2e;
#[path = "../diagnostics_classifier_cast_cases_e2e.rs"]
mod diagnostics_classifier_cast_cases_e2e;
#[path = "../diagnostics_inference_parity_e2e.rs"]
mod diagnostics_inference_parity_e2e;
#[path = "../diagnostics_java_access_parity_e2e.rs"]
mod diagnostics_java_access_parity_e2e;
#[path = "../diagnostics_jvm_and_catch_parity_e2e.rs"]
mod diagnostics_jvm_and_catch_parity_e2e;
#[path = "../diagnostics_language_parity_e2e.rs"]
mod diagnostics_language_parity_e2e;
#[path = "../diagnostics_receiver_reference_cases_e2e.rs"]
mod diagnostics_receiver_reference_cases_e2e;
#[path = "../diagnostics_signature_ambiguity_e2e.rs"]
mod diagnostics_signature_ambiguity_e2e;
#[path = "../diagnostics_type_inference_cases_e2e.rs"]
mod diagnostics_type_inference_cases_e2e;

//! Array construction, member, and inference cases grouped under the bounded e2e test target.

pub(super) use super::common;

#[path = "../array_clone_e2e.rs"]
mod array_clone_e2e;
#[path = "../array_member_reference_e2e.rs"]
mod array_member_reference_e2e;
#[path = "../boxed_array_construction_e2e.rs"]
mod boxed_array_construction_e2e;
#[path = "../java_array_covariance_e2e.rs"]
mod java_array_covariance_e2e;
#[path = "../member_array_ctor_inference_e2e.rs"]
mod member_array_ctor_inference_e2e;
#[path = "../primitive_array_members_e2e.rs"]
mod primitive_array_members_e2e;
#[path = "../toplevel_array_inference_e2e.rs"]
mod toplevel_array_inference_e2e;

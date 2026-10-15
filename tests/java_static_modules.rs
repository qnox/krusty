//! Registry facade for Java static-member integration tests.

macro_rules! declare {
    () => {
        #[path = "java_static_callable_reference_e2e.rs"]
        mod java_static_callable_reference_e2e;
        #[path = "java_static_field_e2e.rs"]
        mod java_static_field_e2e;
        #[path = "java_static_sam_inferred_return_e2e.rs"]
        mod java_static_sam_inferred_return_e2e;
    };
}

pub(crate) use declare;

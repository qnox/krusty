//! Registry facade for metadata type-shape integration tests.

macro_rules! declare {
    () => {
        #[path = "capture_storage_metadata_e2e.rs"]
        mod capture_storage_metadata_e2e;
        #[path = "local_metadata_applications_e2e.rs"]
        mod local_metadata_applications_e2e;
        #[path = "metadata_array_signature_e2e.rs"]
        mod metadata_array_signature_e2e;
        #[path = "metadata_captured_type_parameters_e2e.rs"]
        mod metadata_captured_type_parameters_e2e;
        #[path = "metadata_inner_outer_type_e2e.rs"]
        mod metadata_inner_outer_type_e2e;
        #[path = "metadata_supertype_order_e2e.rs"]
        mod metadata_supertype_order_e2e;
        #[path = "metadata_type_reference_e2e.rs"]
        mod metadata_type_reference_e2e;
    };
}

pub(crate) use declare;

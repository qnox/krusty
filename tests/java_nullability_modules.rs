//! Registry facade for integration tests of Java nullability facts and the accessors they shape.

macro_rules! declare {
    () => {
        #[path = "java_fluent_setter_e2e.rs"]
        mod java_fluent_setter_e2e;
        #[path = "java_not_null_collection_mutability_e2e.rs"]
        mod java_not_null_collection_mutability_e2e;
        #[path = "java_not_null_null_argument_e2e.rs"]
        mod java_not_null_null_argument_e2e;
        #[path = "java_not_null_setter_pairing_e2e.rs"]
        mod java_not_null_setter_pairing_e2e;
        #[path = "java_operator_receiver_null_check_e2e.rs"]
        mod java_operator_receiver_null_check_e2e;
    };
}

pub(crate) use declare;

//! Registry facade for Java instance-member integration tests.

macro_rules! declare {
    () => {
        #[path = "java_member_flexible_return_e2e.rs"]
        mod java_member_flexible_return_e2e;
        #[path = "java_member_function_value_e2e.rs"]
        mod java_member_function_value_e2e;
    };
}

pub(crate) use declare;

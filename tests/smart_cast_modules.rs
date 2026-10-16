//! Root-module registration for the smart-cast conformance test family.

macro_rules! declare {
    () => {
        #[path = "smart_cast_join_e2e.rs"]
        mod smart_cast_join_e2e;
        #[path = "smart_cast_member_extension_e2e.rs"]
        mod smart_cast_member_extension_e2e;
        #[path = "smart_cast_receiver_property_e2e.rs"]
        mod smart_cast_receiver_property_e2e;
        #[path = "smartcast_and_e2e.rs"]
        mod smartcast_and_e2e;
    };
}

pub(crate) use declare;

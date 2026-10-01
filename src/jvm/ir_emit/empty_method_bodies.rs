//! Empty JVM realization provider for emitter unit tests.

use crate::jvm::classreader::MethodCode;
use crate::jvm::inline::MethodBodies;
use crate::types::TypeName;

pub(super) struct NoBodies;

impl MethodBodies for NoBodies {
    fn body(&self, _owner: &str, _name: &str, _descriptor: &str) -> Option<MethodCode> {
        None
    }

    fn body_name(&self, _owner: TypeName, _name: &str, _descriptor: &str) -> Option<MethodCode> {
        None
    }
}

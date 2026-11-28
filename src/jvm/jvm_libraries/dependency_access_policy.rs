use super::JvmLibraries;
use crate::types::TypeName;

impl crate::libraries::DependencyAccessPolicy for JvmLibraries {
    fn internal_classifier_accessible(&self, owner: TypeName) -> bool {
        self.cp.grants_internal_access(owner)
    }

    fn unexported_package_module(&self, owner: TypeName) -> Option<String> {
        self.cp.unexported_jdk_module(owner)
    }
}

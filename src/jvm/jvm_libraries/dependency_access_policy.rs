use super::JvmLibraries;
use crate::types::TypeName;

impl crate::libraries::DependencyAccessPolicy for JvmLibraries {
    fn internal_classifier_accessible(&self, owner: TypeName) -> bool {
        self.cp.grants_internal_access(owner)
    }
}

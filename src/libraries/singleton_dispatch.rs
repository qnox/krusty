use crate::types::{Ty, TypeName};

/// Semantic singleton value used as an implicit dispatch receiver for an imported object member.
/// Only the selected classifier crosses the provider boundary; each backend owns the target storage
/// or accessor used to materialize that singleton.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SingletonDispatch {
    pub classifier: TypeName,
}

impl SingletonDispatch {
    pub fn ty(self) -> Ty {
        Ty::obj_name(self.classifier)
    }
}

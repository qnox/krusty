//! Opaque boundary for provider-owned override-slot identities.

use crate::types::TypeName;
use std::any::Any;
use std::fmt;
use std::sync::Arc;

trait OpaqueOverrideKey: Send + Sync {
    fn as_any(&self) -> &dyn Any;

    fn same_key(&self, other: &dyn OpaqueOverrideKey) -> bool;
}

impl<T> OpaqueOverrideKey for T
where
    T: Eq + Send + Sync + 'static,
{
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn same_key(&self, other: &dyn OpaqueOverrideKey) -> bool {
        other.as_any().downcast_ref::<T>() == Some(self)
    }
}

/// Provider-normalized identity of one platform override slot.
///
/// The provider owns the key's concrete type and equality. Common hierarchy code can compare two
/// slots and read their semantic owners, but cannot inspect or reconstruct target representation
/// facts from the key.
#[derive(Clone)]
pub struct ProviderOverrideSlot {
    owner: TypeName,
    key: Arc<dyn OpaqueOverrideKey>,
}

impl ProviderOverrideSlot {
    pub(crate) fn new<K>(owner: TypeName, key: K) -> Self
    where
        K: Eq + Send + Sync + 'static,
    {
        Self {
            owner,
            key: Arc::new(key),
        }
    }

    pub(crate) fn owner(&self) -> TypeName {
        self.owner
    }

    pub(crate) fn same_key(&self, other: &Self) -> bool {
        self.key.same_key(other.key.as_ref())
    }
}

impl fmt::Debug for ProviderOverrideSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderOverrideSlot")
            .field("owner", &self.owner)
            .field("key", &"<provider-owned>")
            .finish()
    }
}

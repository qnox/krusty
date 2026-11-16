//! Exact serialized classifier identities retained across provider lifetimes.

use crate::metadata::id_signature::KlibPublicIdSignature;

/// One classifier declaration in one exact KLIB module.
///
/// This identifies the class independently of its constructors and carries no provider-local
/// type handles. Producers sign it with their declaration inventory; consumers retain it from
/// the same normalized classifier record selected during checking.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct KlibClassIdentity {
    pub module: String,
    pub signature: KlibPublicIdSignature,
}

//! Exact JVM call-site facts for protected dependency members.
//!
//! Common IR retains only the selected dependency identity. Once external-call realization has
//! chosen the physical JVM member, this table follows that exact expression through later JVM
//! representation rewrites so emission can synthesize an `access$` bridge without reopening the
//! classpath or recovering a declaration from owner/name text.

use crate::types::{Ty, TypeName};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProtectedDependencyCall {
    pub(crate) owner: TypeName,
    pub(crate) name: String,
    pub(crate) parameters: Box<[Ty]>,
    pub(crate) result: Ty,
}

//! Provider-normalized physical call targets carried with a selected semantic declaration.

use crate::types::{Ty, TypeName};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NonvirtualCallRealization {
    pub owner: TypeName,
    pub descriptor: String,
}

#[derive(Clone, Debug)]
pub struct DefaultCallRealization {
    /// Exact platform invocation identity selected by the provider. Consumers must not reconstruct
    /// any part of this target from the source declaration.
    pub owner: TypeName,
    pub name: String,
    pub descriptor: String,
    /// Classfile that declares the target body. This can differ from `owner` when a public facade is
    /// the legal invocation owner for a method stored on one of its package parts.
    pub declaration_owner: TypeName,
    pub real_params: Vec<Ty>,
    /// Number of platform mask words before the trailing marker. Zero denotes a marker-only
    /// realization; consumers must not infer this ABI fact from the source parameter count.
    pub mask_count: usize,
    pub ret: Ty,
    pub suspend: bool,
}

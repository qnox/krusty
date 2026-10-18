//! Where the JVM backend writes a function's method.

use std::borrow::Cow;

use super::StaticOwner;
use crate::jvm::classfile::{ACC_PRIVATE, ACC_PROTECTED, ACC_PUBLIC};

/// Where a method body is written: on its declaring class, or onto an interface's
/// `$DefaultImpls` holder, where the code and slots are the instance method's but the method is
/// `static` and its descriptor carries the receiver as parameter 0.
#[derive(Clone, Copy)]
pub(super) enum MethodPlacement {
    Declared {
        static_owner: StaticOwner,
        instance: bool,
    },
    Holder(crate::types::TypeName),
    /// The private `name$$forInline` copy beside a callable suspend inline function on its
    /// declaring class: the same body, left untransformed for an inlining caller.
    ForInlineCopy {
        static_owner: StaticOwner,
        instance: bool,
    },
}

impl MethodPlacement {
    pub(super) fn is_for_inline_copy(self) -> bool {
        matches!(self, MethodPlacement::ForInlineCopy { .. })
    }

    /// The name of the method written for a function declared as `name`.
    pub(super) fn method_name(self, name: &str) -> Cow<'_, str> {
        match self {
            MethodPlacement::ForInlineCopy { .. } => Cow::Owned(format!("{name}$$forInline")),
            _ => Cow::Borrowed(name),
        }
    }

    /// The access of the method written for a declaration with `declared` access: the copy is
    /// private and keeps every other flag.
    pub(super) fn method_access(self, declared: u16) -> u16 {
        match self {
            MethodPlacement::ForInlineCopy { .. } => {
                declared & !(ACC_PUBLIC | ACC_PROTECTED) | ACC_PRIVATE
            }
            _ => declared,
        }
    }
}

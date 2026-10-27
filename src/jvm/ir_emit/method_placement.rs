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
    /// A `@JvmStatic` member of a named object, written as a `static` method of the object. The
    /// common IR still numbers its receiver as value 0; that value reads the singleton's instance
    /// field instead of a parameter slot, and the descriptor carries no receiver.
    Singleton {
        static_owner: StaticOwner,
        singleton: crate::types::TypeName,
    },
    /// The `name$$forInline` copy beside a suspend function an inlining caller copies (`kind`
    /// says which) on its declaring class: the same body, left untransformed for that caller.
    /// A named object's `@JvmStatic` member keeps its copy static, as `singleton` names.
    ForInlineCopy {
        static_owner: StaticOwner,
        instance: bool,
        singleton: Option<crate::types::TypeName>,
        kind: crate::jvm::suspend_inline_functions::ForInlineCopy,
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

    /// The access of the method written for a declaration with `declared` access: an inline
    /// function's copy is private and keeps every other flag; a method's copy keeps its access.
    pub(super) fn method_access(self, declared: u16) -> u16 {
        match self {
            MethodPlacement::ForInlineCopy {
                kind: crate::jvm::suspend_inline_functions::ForInlineCopy::InlineFunction,
                ..
            } => declared & !(ACC_PUBLIC | ACC_PROTECTED) | ACC_PRIVATE,
            _ => declared,
        }
    }

    /// Whether the method starts by constructing its continuation between `InlineMarker.mark(4)`
    /// and `mark(5)`: a method's copy, whose inlining caller builds its machine over it.
    pub(super) fn constructs_fake_continuation(self) -> bool {
        matches!(
            self,
            MethodPlacement::ForInlineCopy {
                kind: crate::jvm::suspend_inline_functions::ForInlineCopy::CrossinlineCapture,
                ..
            }
        )
    }
}

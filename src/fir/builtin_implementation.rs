//! Checked implementation facts for members of represented builtin classes.
//!
//! The checker derives these from normalized declaration and override data. Common IR carries
//! them to a backend so a represented value can dispatch a supertype member without reopening the
//! member hierarchy.

use crate::libraries::CompilerIntrinsic;
use crate::types::Ty;

/// What one builtin class runs for a call of a supertype member on a value of that class.
#[derive(Clone, Debug, PartialEq)]
pub struct BuiltinImplementation {
    /// The builtin type (`String`, a primitive), whose classifier is a proper subtype of the
    /// member's.
    pub receiver: Ty,
    /// The builtin classifier's override; `None` when it inherits the member's own body.
    pub declaration: Option<BuiltinOverrideDeclaration>,
}

/// One builtin classifier's override of a supertype member, as its provider published it.
#[derive(Clone, Debug, PartialEq)]
pub struct BuiltinOverrideDeclaration {
    /// The operation the provider realizes the override as, if it is one.
    pub operation: Option<CompilerIntrinsic>,
    /// The override's declared parameter and result types; a property's are its getter's.
    pub params: Box<[Ty]>,
    pub ret: Ty,
}

//! The Kotlin properties a platform accessor method can be named as.

/// The properties whose getter or setter a platform method is named as: a Java `getA()` is the
/// getter of `a`, and `setOpen(v)` the setter of `open` or `isOpen`. Each candidate names the
/// methods that would be the same property's other accessor (`setA` for `getA`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertyAccessorRole {
    pub candidates: Box<[PropertyAccessorCandidate]>,
    pub setter: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertyAccessorCandidate {
    pub property: String,
    pub counterparts: Box<[String]>,
}

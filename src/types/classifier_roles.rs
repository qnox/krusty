//! Semantic roles a classifier declaration can play in type checks and casts.
//!
//! Providers publish these on the classifier records they normalize: the collection mapping comes
//! from the platform's builtin class map, and a function classifier's arity from its declared
//! callable signature. Consumers read the published role; they never recover it from a name.

/// Which Kotlin collection classifier a mapped builtin is: kotlinc's `JavaToKotlinClassMap` pairs
/// each read-only and mutable face with one platform collection interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CollectionKind {
    Iterator,
    Iterable,
    Collection,
    List,
    ListIterator,
    Set,
    Map,
    MapEntry,
}

/// A Kotlin collection classifier mapped onto a platform collection interface, and which of its two
/// faces it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct MappedCollection {
    pub kind: CollectionKind,
    pub mutable: bool,
}

/// What a classifier declaration is, for the checks a plain instance test or cast cannot decide.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ClassifierRole {
    /// One face of a mapped collection classifier.
    MappedCollection(MappedCollection),
    /// A non-suspend, non-reflective `FunctionN` classifier of this arity.
    FunctionOfArity(u8),
}

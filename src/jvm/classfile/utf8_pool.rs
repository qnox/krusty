//! Content-addressed `CONSTANT_Utf8` index.
//!
//! The pool's general dedup map owns each constant, so looking a string up there builds a temporary
//! `String` key. Repeated class names, descriptors, and method names are looked up far more often
//! than they are inserted. This index hashes the borrowed text and owns a spelling only on insert.

use std::borrow::Borrow;
use std::collections::HashMap;

#[derive(Eq, PartialEq, Hash)]
pub(super) struct Utf8Key(Box<str>);

impl Borrow<str> for Utf8Key {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[derive(Default)]
pub(super) struct Utf8Pool {
    index: HashMap<Utf8Key, u16>,
}

impl Utf8Pool {
    pub(super) fn get(&self, text: &str) -> Option<u16> {
        self.index.get(text).copied()
    }

    pub(super) fn insert(&mut self, text: String, slot: u16) {
        self.index.insert(Utf8Key(text.into_boxed_str()), slot);
    }

    pub(super) fn remove(&mut self, text: &str) {
        self.index.remove(text);
    }
}

impl super::ConstPool {
    pub(super) fn utf8(&mut self, text: &str) -> u16 {
        if let Some(index) = self.utf8_index.get(text) {
            return index;
        }
        self.intern(super::Const::Utf8(text.to_string()))
    }

    pub(super) fn class(&mut self, internal_name: &str) -> u16 {
        // Ty→bytecode boundary: a built-in type may reach here under its Kotlin name (`kotlin/Any`);
        // a `CONSTANT_Class` must carry the JVM name (`java/lang/Object`). Every bare class reference
        // (class_ref, method/field owner, super, interfaces) funnels through here, so this single
        // mapping keeps the rest of the compiler free of `java/lang/…` names.
        let physical = crate::jvm::names::classfile_internal_name(internal_name);
        let utf8 = self.utf8(&physical);
        self.intern(super::Const::Class(utf8))
    }

    pub(super) fn string(&mut self, text: &str) -> u16 {
        let utf8 = self.utf8(text);
        self.intern(super::Const::String(utf8))
    }

    /// Whether a `CONSTANT_Class` for `internal_name` is already in the pool (WITHOUT interning it).
    /// kotlinc emits an `InnerClasses` entry for a nested class only when it appears as a class
    /// constant (a `new`/`checkcast`/owner ref), not merely inside a descriptor string.
    pub(super) fn has_class(&self, internal_name: &str) -> bool {
        self.class_index(internal_name).is_some()
    }

    /// The index of the `CONSTANT_Class` for `internal_name`, if the pool holds one.
    pub(super) fn class_index(&self, internal_name: &str) -> Option<u16> {
        let mapped = crate::jvm::jvm_class_map::to_jvm_internal(internal_name);
        let utf8 = self.utf8_index.get(mapped)?;
        self.dedup.get(&super::Const::Class(utf8)).copied()
    }
}

#[test]
fn repeated_utf8_reuses_one_pool_slot() {
    let mut pool = super::ConstPool::default();
    let first = pool.utf8("sample/Utf8Owner");
    assert_eq!(pool.utf8("sample/Utf8Owner"), first);
    assert_eq!(pool.lookup_utf8("sample/Utf8Owner"), Some(first));
    assert_eq!(pool.entries.len(), 1);
}

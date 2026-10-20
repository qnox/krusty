//! Pointer-sized handles for the borrowed pieces of [`super::Ty`].
//!
//! A `&'static [Ty]` or `&'static str` is a two-word fat pointer, which made `Ty` four words wide
//! although every other variant holds one pointer. Each distinct argument list and parameter name
//! is interned once, so the handle can point at the interned fat pointer instead: one word, and
//! `Ty` fits in two. Equality and hashing still follow the contents, exactly as the fat pointers
//! did.

use super::interning::ShardedInterner;
use super::Ty;
use crate::fx_hash::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::OnceLock;
use std::sync::RwLock;

const NO_TYPES: &[Ty] = &[];

/// An interned type-argument (or intersection component) list. Reads as `&'static [Ty]`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TyList(&'static &'static [Ty]);

impl TyList {
    pub const EMPTY: TyList = TyList(&NO_TYPES);

    pub fn new(types: &[Ty]) -> TyList {
        if types.is_empty() {
            return TyList::EMPTY;
        }
        static I: OnceLock<ShardedInterner<&'static [Ty]>> = OnceLock::new();
        TyList(
            I.get_or_init(ShardedInterner::default)
                .intern_owned(super::intern_tys(types)),
        )
    }

    pub fn as_slice(self) -> &'static [Ty] {
        self.0
    }
}

impl Default for TyList {
    fn default() -> Self {
        TyList::EMPTY
    }
}

impl Deref for TyList {
    type Target = [Ty];

    fn deref(&self) -> &[Ty] {
        self.0
    }
}

impl fmt::Debug for TyList {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl IntoIterator for &TyList {
    type Item = &'static Ty;
    type IntoIter = std::slice::Iter<'static, Ty>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl IntoIterator for TyList {
    type Item = &'static Ty;
    type IntoIter = std::slice::Iter<'static, Ty>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl PartialEq<TyList> for &[Ty] {
    fn eq(&self, other: &TyList) -> bool {
        *self == *other.0
    }
}

impl PartialEq<TyList> for [Ty] {
    fn eq(&self, other: &TyList) -> bool {
        self == *other.0
    }
}

impl<const N: usize> PartialEq<[Ty; N]> for TyList {
    fn eq(&self, other: &[Ty; N]) -> bool {
        *self.0 == other.as_slice()
    }
}

impl<const N: usize> PartialEq<&[Ty; N]> for TyList {
    fn eq(&self, other: &&[Ty; N]) -> bool {
        *self.0 == other.as_slice()
    }
}

impl PartialEq<[Ty]> for TyList {
    fn eq(&self, other: &[Ty]) -> bool {
        *self.0 == other
    }
}

impl PartialEq<&[Ty]> for TyList {
    fn eq(&self, other: &&[Ty]) -> bool {
        *self.0 == *other
    }
}

/// An interned type-parameter name. Reads as `&'static str`.
///
/// A 4-byte id rather than a pointer: `TyParam` also carries its bound pointer, and a one-word
/// name would keep that variant three words wide. Equality compares ids (each name is interned
/// once); ordering and hashing follow the spelling, so they do not depend on which worker
/// interned a name first.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TyParamName(u32);

const NAME_CHUNK_BITS: u32 = 10;
const NAME_CHUNK: usize = 1 << NAME_CHUNK_BITS;
const NAME_CHUNKS: usize = 1 << 12;

/// One lazily allocated block of name slots.
type NameChunk = Box<[OnceLock<&'static str>]>;

struct NameTable {
    ids: RwLock<HashMap<&'static str, u32>>,
    chunks: Box<[OnceLock<NameChunk>]>,
}

fn name_table() -> &'static NameTable {
    static TABLE: OnceLock<NameTable> = OnceLock::new();
    TABLE.get_or_init(|| NameTable {
        ids: RwLock::new(HashMap::default()),
        chunks: (0..NAME_CHUNKS).map(|_| OnceLock::new()).collect(),
    })
}

impl TyParamName {
    pub fn new(name: &str) -> TyParamName {
        let table = name_table();
        if let Some(&id) = table.ids.read().unwrap().get(name) {
            return TyParamName(id);
        }
        let mut ids = table.ids.write().unwrap();
        if let Some(&id) = ids.get(name) {
            return TyParamName(id);
        }
        let id = u32::try_from(ids.len()).expect("type-parameter name table overflow");
        let spelling = super::intern(name);
        let chunk = table.chunks[(id >> NAME_CHUNK_BITS) as usize]
            .get_or_init(|| (0..NAME_CHUNK).map(|_| OnceLock::new()).collect());
        chunk[id as usize % NAME_CHUNK]
            .set(spelling)
            .expect("a type-parameter name id is published once");
        ids.insert(spelling, id);
        TyParamName(id)
    }

    pub fn as_str(self) -> &'static str {
        let chunk = name_table().chunks[(self.0 >> NAME_CHUNK_BITS) as usize]
            .get()
            .expect("type-parameter name ids come from TyParamName::new");
        chunk[self.0 as usize % NAME_CHUNK]
            .get()
            .expect("type-parameter name ids come from TyParamName::new")
    }
}

impl Hash for TyParamName {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl PartialOrd for TyParamName {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TyParamName {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl From<&str> for TyParamName {
    fn from(name: &str) -> Self {
        TyParamName::new(name)
    }
}

impl Deref for TyParamName {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Debug for TyParamName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(formatter)
    }
}

impl fmt::Display for TyParamName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(formatter)
    }
}

impl PartialEq<str> for TyParamName {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for TyParamName {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for TyParamName {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<TyParamName> for str {
    fn eq(&self, other: &TyParamName) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<TyParamName> for &str {
    fn eq(&self, other: &TyParamName) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<TyParamName> for String {
    fn eq(&self, other: &TyParamName) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<&String> for TyParamName {
    fn eq(&self, other: &&String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<TyParamName> for &String {
    fn eq(&self, other: &TyParamName) -> bool {
        self.as_str() == other.as_str()
    }
}

impl AsRef<str> for TyParamName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::borrow::Borrow<str> for TyParamName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_one_word_and_keep_ty_at_two() {
        assert_eq!(std::mem::size_of::<TyList>(), 8);
        assert_eq!(std::mem::size_of::<TyParamName>(), 4);
        assert_eq!(std::mem::size_of::<Ty>(), 16);
        let list = TyList::new(&[Ty::Unit, Ty::Null]);
        assert!(std::ptr::eq(list.0, TyList::new(&[Ty::Unit, Ty::Null]).0));
        assert_eq!(list, [Ty::Unit, Ty::Null][..]);
        assert_eq!(TyList::new(&[]), TyList::EMPTY);
        assert_eq!(TyParamName::new("T"), "T");
    }
}

//! One boolean fact per logical parameter of a callable, as an interned list.
//!
//! A callable signature carries several such facts (has a default, `@Exact`, `@NoInfer`, ...).
//! Held as `Vec<bool>`, each costs a 24-byte header and its own heap block in every declaration
//! the compiler keeps, although the lists repeat a handful of patterns. Interning shares one
//! allocation per distinct list and keeps a 16-byte `Copy` handle that reads as `&[bool]`.

use std::fmt;
use std::ops::Deref;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ParamFlags(&'static [bool]);

impl ParamFlags {
    pub const fn new() -> Self {
        ParamFlags(&[])
    }

    pub fn repeat(value: bool, len: usize) -> Self {
        std::iter::repeat_n(value, len).collect()
    }

    pub fn as_slice(&self) -> &'static [bool] {
        self.0
    }

    fn edit(&mut self, change: impl FnOnce(&mut Vec<bool>)) {
        let mut values = self.0.to_vec();
        change(&mut values);
        *self = ParamFlags::from(values.as_slice());
    }

    pub fn push(&mut self, value: bool) {
        self.edit(|values| values.push(value));
    }

    pub fn set(&mut self, index: usize, value: bool) {
        self.edit(|values| values[index] = value);
    }

    pub fn insert(&mut self, index: usize, value: bool) {
        self.edit(|values| values.insert(index, value));
    }

    pub fn remove(&mut self, index: usize) -> bool {
        let removed = self.0[index];
        self.edit(|values| {
            values.remove(index);
        });
        removed
    }

    pub fn truncate(&mut self, len: usize) {
        self.edit(|values| values.truncate(len));
    }

    pub fn resize(&mut self, len: usize, value: bool) {
        self.edit(|values| values.resize(len, value));
    }

    pub fn clear(&mut self) {
        *self = ParamFlags::new();
    }
}

impl Deref for ParamFlags {
    type Target = [bool];

    fn deref(&self) -> &[bool] {
        self.0
    }
}

impl FromIterator<bool> for ParamFlags {
    fn from_iter<I: IntoIterator<Item = bool>>(iter: I) -> Self {
        ParamFlags::from(iter.into_iter().collect::<Vec<_>>().as_slice())
    }
}

impl<'a> FromIterator<&'a bool> for ParamFlags {
    fn from_iter<I: IntoIterator<Item = &'a bool>>(iter: I) -> Self {
        iter.into_iter().copied().collect()
    }
}

impl Extend<bool> for ParamFlags {
    fn extend<I: IntoIterator<Item = bool>>(&mut self, iter: I) {
        self.edit(|values| values.extend(iter));
    }
}

impl From<Vec<bool>> for ParamFlags {
    fn from(values: Vec<bool>) -> Self {
        ParamFlags::from(values.as_slice())
    }
}

impl From<&[bool]> for ParamFlags {
    fn from(values: &[bool]) -> Self {
        ParamFlags(crate::types::intern_bools(values))
    }
}

impl From<ParamFlags> for Vec<bool> {
    fn from(flags: ParamFlags) -> Self {
        flags.0.to_vec()
    }
}

impl IntoIterator for &ParamFlags {
    type Item = &'static bool;
    type IntoIter = std::slice::Iter<'static, bool>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<const N: usize> PartialEq<[bool; N]> for ParamFlags {
    fn eq(&self, other: &[bool; N]) -> bool {
        self.0 == other.as_slice()
    }
}

impl PartialEq<Vec<bool>> for ParamFlags {
    fn eq(&self, other: &Vec<bool>) -> bool {
        self.0 == other.as_slice()
    }
}

impl fmt::Debug for ParamFlags {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_flag_lists_share_one_interned_allocation_and_edits_reintern() {
        let first = ParamFlags::from(vec![true, false, true]);
        let second = [true, false, true].iter().collect::<ParamFlags>();
        assert_eq!(first, second);
        assert!(std::ptr::eq(first.as_slice(), second.as_slice()));

        let mut edited = ParamFlags::repeat(false, 3);
        edited.set(0, true);
        edited.set(2, true);
        assert!(std::ptr::eq(edited.as_slice(), first.as_slice()));
        edited.insert(1, true);
        assert_eq!(edited, vec![true, true, false, true]);
        assert!(edited.remove(1));
        assert_eq!(edited, first);
        assert_eq!(ParamFlags::new().len(), 0);
    }
}

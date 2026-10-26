//! An ordered list of interned classifier names, rendered only at its text boundaries.

use super::{type_name, TypeName};
use std::fmt;

#[derive(Clone, Default)]
pub struct TypeNameList {
    names: Vec<TypeName>,
}

impl TypeNameList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, internal: &str) {
        self.names.push(type_name(internal));
    }

    pub fn push_name(&mut self, internal: TypeName) {
        self.names.push(internal);
    }

    pub fn iter_ids(&self) -> impl Iterator<Item = TypeName> + '_ {
        self.names.iter().copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = TypeName> + '_ {
        self.iter_ids()
    }

    pub fn iter_rendered(&self) -> impl Iterator<Item = String> + '_ {
        self.iter_ids().map(TypeName::render)
    }

    pub(crate) fn remap(&mut self, mut map: impl FnMut(TypeName) -> TypeName) {
        for name in &mut self.names {
            *name = map(*name);
        }
    }

    pub fn contains(&self, internal: &str) -> bool {
        self.names.iter().any(|name| name.matches(internal))
    }

    pub fn contains_name(&self, internal: TypeName) -> bool {
        self.names.contains(&internal)
    }

    pub fn to_vec(&self) -> Vec<String> {
        self.iter_rendered().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }
}

impl fmt::Debug for TypeNameList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter_rendered()).finish()
    }
}

impl From<Vec<String>> for TypeNameList {
    fn from(names: Vec<String>) -> Self {
        TypeNameList {
            names: names.into_iter().map(|name| type_name(&name)).collect(),
        }
    }
}

impl From<Vec<&str>> for TypeNameList {
    fn from(names: Vec<&str>) -> Self {
        TypeNameList {
            names: names.into_iter().map(type_name).collect(),
        }
    }
}

impl From<Vec<TypeName>> for TypeNameList {
    fn from(names: Vec<TypeName>) -> Self {
        TypeNameList { names }
    }
}

impl IntoIterator for TypeNameList {
    type Item = String;
    type IntoIter = std::vec::IntoIter<String>;

    fn into_iter(self) -> Self::IntoIter {
        self.to_vec().into_iter()
    }
}

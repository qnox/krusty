use crate::name_tree::{NameId, NameTree};
use crate::spelling::type_name;
use std::fmt;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeName(pub(crate) NameId);

// The tree is concurrent (lock-free reads, internally-locked inserts), so the global interner is a
// bare shared instance.
static TYPE_NAMES: OnceLock<NameTree> = OnceLock::new();

#[inline]
pub(crate) fn type_names() -> &'static NameTree {
    TYPE_NAMES.get_or_init(|| {
        let names = NameTree::default();
        for (path, expected) in BUILTIN_TYPE_NAMES {
            let actual = names.insert(path);
            assert_eq!(actual, expected.0, "built-in TypeName id for {path}");
        }
        names
    })
}

/// Find a global type-name identity in another name tree without mutating either tree.
pub fn existing_type_name_in(names: &NameTree, internal: TypeName) -> Option<NameId> {
    names.existing_from(type_names(), internal.name_id())
}

/// Copy a global type-name identity into another name tree without rendering it to text.
pub fn insert_type_name_in(names: &NameTree, internal: TypeName) -> NameId {
    names.insert_from(type_names(), internal.name_id())
}

pub const KOTLIN_BOOLEAN: TypeName = TypeName(NameId(2));
pub const KOTLIN_BYTE: TypeName = TypeName(NameId(3));
pub const KOTLIN_SHORT: TypeName = TypeName(NameId(4));
pub const KOTLIN_INT: TypeName = TypeName(NameId(5));
pub const KOTLIN_LONG: TypeName = TypeName(NameId(6));
pub const KOTLIN_CHAR: TypeName = TypeName(NameId(7));
pub const KOTLIN_FLOAT: TypeName = TypeName(NameId(8));
pub const KOTLIN_DOUBLE: TypeName = TypeName(NameId(9));
pub const KOTLIN_UBYTE: TypeName = TypeName(NameId(10));
pub const KOTLIN_USHORT: TypeName = TypeName(NameId(11));
pub const KOTLIN_UINT: TypeName = TypeName(NameId(12));
pub const KOTLIN_ULONG: TypeName = TypeName(NameId(13));
pub const KOTLIN_STRING: TypeName = TypeName(NameId(14));
pub const KOTLIN_UNIT: TypeName = TypeName(NameId(15));
pub const KOTLIN_NOTHING: TypeName = TypeName(NameId(16));
pub const KOTLIN_ARRAY: TypeName = TypeName(NameId(17));

const BUILTIN_TYPE_NAMES: [(&str, TypeName); 16] = [
    ("kotlin/Boolean", KOTLIN_BOOLEAN),
    ("kotlin/Byte", KOTLIN_BYTE),
    ("kotlin/Short", KOTLIN_SHORT),
    ("kotlin/Int", KOTLIN_INT),
    ("kotlin/Long", KOTLIN_LONG),
    ("kotlin/Char", KOTLIN_CHAR),
    ("kotlin/Float", KOTLIN_FLOAT),
    ("kotlin/Double", KOTLIN_DOUBLE),
    ("kotlin/UByte", KOTLIN_UBYTE),
    ("kotlin/UShort", KOTLIN_USHORT),
    ("kotlin/UInt", KOTLIN_UINT),
    ("kotlin/ULong", KOTLIN_ULONG),
    ("kotlin/String", KOTLIN_STRING),
    ("kotlin/Unit", KOTLIN_UNIT),
    ("kotlin/Nothing", KOTLIN_NOTHING),
    ("kotlin/Array", KOTLIN_ARRAY),
];

/// `parent/segment` as a `TypeName` without rendering `parent` — one child step in the name tree.
/// `segment` must be a single path segment; a multi-segment suffix falls back to the full insert.
pub fn type_name_child(parent: TypeName, segment: &str) -> TypeName {
    if segment.contains('/') {
        return segment
            .split('/')
            .filter(|segment| !segment.is_empty())
            .fold(parent, type_name_child);
    }
    TypeName(type_names().child_of(parent.name_id(), segment))
}

pub fn type_name_nested_child(owner: TypeName, nested: &str) -> TypeName {
    TypeName(type_names().nested_child_of(owner.name_id(), nested))
}

pub fn existing_type_name_nested_child(owner: TypeName, segment: &str) -> Option<TypeName> {
    type_names()
        .existing_nested_child_of(owner.name_id(), segment)
        .map(TypeName)
}

impl From<&String> for TypeName {
    fn from(internal: &String) -> Self {
        type_name(internal)
    }
}

impl TypeName {
    pub const ROOT: TypeName = TypeName(NameId(0));

    #[inline]
    pub fn name_id(self) -> NameId {
        self.0
    }

    pub fn matches(self, internal: &str) -> bool {
        type_names().matches_path(self.name_id(), internal)
    }

    pub fn starts_with(self, prefix: &str) -> bool {
        type_names().starts_with(self.name_id(), prefix)
    }

    pub fn contains(self, needle: &str) -> bool {
        type_names().contains(self.name_id(), needle)
    }

    pub fn qualifier_matches(self, qualifier: &str) -> bool {
        type_names().qualifier_matches(self.name_id(), qualifier)
    }

    pub fn package_matches(self, package: &str) -> bool {
        type_names().package_matches(self.name_id(), package)
    }

    pub fn package(self) -> String {
        type_names().package(self.name_id())
    }

    pub fn parent(self) -> Option<TypeName> {
        type_names().parent(self.name_id()).map(TypeName)
    }

    pub fn namespace(self) -> TypeName {
        self.parent().unwrap_or(Self::ROOT)
    }

    pub fn segment(self) -> String {
        type_names().segment(self.0).to_string()
    }

    pub fn segment_ref(self) -> &'static str {
        type_names().segment(self.0)
    }

    pub fn nested_segment_ref(self) -> &'static str {
        self.segment_ref()
            .rsplit_once(['$', '.'])
            .map_or(self.segment_ref(), |(_, nested)| nested)
    }

    /// Whether this classifier identity is `owner` or is nested in it. Callers must use this
    /// identity-tree relation instead of rendering names and interpreting string prefixes.
    pub fn same_or_nested_within(self, owner: TypeName) -> bool {
        type_names().same_or_nested_within(self.name_id(), owner.name_id())
    }

    /// Immediate lexical classifier owner, retained by nested-child construction or decoded from an
    /// already-interned nested-class segment.
    #[inline]
    pub fn nested_owner(self) -> Option<TypeName> {
        type_names().nested_owner(self.name_id()).map(TypeName)
    }

    /// Exact declaration nesting, without the backend spelling interpretation of `nested_owner`.
    pub fn exact_nested_owner(self) -> Option<TypeName> {
        type_names()
            .exact_nested_owner(self.name_id())
            .map(TypeName)
    }

    /// Backend-oriented candidates for textual JVM nesting, deepest first. Semantic resolution
    /// continues to use the single exact [`Self::nested_owner`] relation.
    pub fn existing_nested_owners(self) -> Vec<TypeName> {
        type_names()
            .existing_nested_owners(self.name_id())
            .into_iter()
            .map(TypeName)
            .collect()
    }

    /// The flattened JVM nested segment relative to an already-resolved enclosing classifier.
    pub fn nested_segment_within(self, owner: TypeName) -> Option<&'static str> {
        type_names().nested_segment_within(self.name_id(), owner.name_id())
    }

    /// Backend boundary split for a nested classifier whose enclosing declaration is external to
    /// the current file. Semantic callers should keep using [`Self::nested_owner`].
    pub fn jvm_nested_parts(self) -> Option<(String, String)> {
        type_names().jvm_nested_parts(self.name_id())
    }

    /// JVM classfile spelling of this classifier identity. Source/metadata nesting separators are
    /// converted by the name tree; semantic callers should retain the identity instead.
    pub fn jvm_binary_name(self) -> String {
        type_names().jvm_binary_name(self.name_id())
    }

    /// JVM classfile spelling for a nested classifier below this identity, without interning it.
    pub fn jvm_nested_binary_name(self, nested: &str) -> String {
        type_names().jvm_nested_binary_name(self.name_id(), nested)
    }

    /// Compare a boundary spelling with a direct JVM nested child of this classifier identity.
    pub fn nested_child_matches_path(self, nested: &str, path: &str) -> bool {
        type_names().nested_child_matches_path(self.name_id(), nested, path)
    }

    /// Lexicographic path ordering for deterministic internal traversal without boundary rendering.
    pub fn path_cmp(self, other: TypeName) -> std::cmp::Ordering {
        type_names().path_cmp(self.name_id(), other.name_id())
    }

    /// Existing classifier nested directly in `self`, without rendering or interning a probe.
    pub fn existing_nested_child(self, nested: &str) -> Option<TypeName> {
        type_names()
            .existing_nested_child_of(self.name_id(), nested)
            .map(TypeName)
    }

    /// Intern a classifier nested directly in this classifier. This is the identity-preserving
    /// construction used for compiler-generated nested classes; callers must not render, append a
    /// `$` spelling, and intern the result again.
    pub fn nested_child(self, nested: &str) -> TypeName {
        TypeName(type_names().nested_child_of(self.name_id(), nested))
    }

    pub fn strip_prefix(self, prefix: &str) -> Option<String> {
        type_names().strip_prefix(self.name_id(), prefix)
    }

    pub fn unsigned_suffix_after_prefix(self, prefix: &str) -> Option<usize> {
        type_names().unsigned_suffix_after_prefix(self.name_id(), prefix)
    }

    pub fn render(self) -> String {
        type_names().render(self.0)
    }
}

impl fmt::Debug for TypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("TypeName").field(&self.render()).finish()
    }
}

impl fmt::Display for TypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

impl From<&str> for TypeName {
    fn from(name: &str) -> Self {
        type_name(name)
    }
}

impl From<String> for TypeName {
    fn from(name: String) -> Self {
        type_name(&name)
    }
}

impl PartialEq<&str> for TypeName {
    fn eq(&self, other: &&str) -> bool {
        self.matches(other)
    }
}

impl PartialEq<TypeName> for &str {
    fn eq(&self, other: &TypeName) -> bool {
        other.matches(self)
    }
}

impl PartialEq<String> for TypeName {
    fn eq(&self, other: &String) -> bool {
        self.matches(other)
    }
}

impl PartialEq<TypeName> for String {
    fn eq(&self, other: &TypeName) -> bool {
        other.matches(self)
    }
}

impl PartialEq<&str> for &TypeName {
    fn eq(&self, other: &&str) -> bool {
        self.matches(other)
    }
}

impl PartialEq<&TypeName> for &str {
    fn eq(&self, other: &&TypeName) -> bool {
        other.matches(self)
    }
}

pub trait InternalName {
    fn internal_matches(&self, internal: &str) -> bool;

    /// The name's final path segment (`IntArray` for `kotlin/IntArray`).
    fn final_segment(&self) -> &str;
}

impl InternalName for TypeName {
    fn internal_matches(&self, internal: &str) -> bool {
        self.matches(internal)
    }

    fn final_segment(&self) -> &str {
        self.segment_ref()
    }
}

impl InternalName for &TypeName {
    fn internal_matches(&self, internal: &str) -> bool {
        self.matches(internal)
    }

    fn final_segment(&self) -> &str {
        self.segment_ref()
    }
}

impl InternalName for &str {
    fn internal_matches(&self, internal: &str) -> bool {
        *self == internal
    }

    fn final_segment(&self) -> &str {
        self.rsplit('/').next().unwrap_or(self)
    }
}

impl InternalName for &String {
    fn internal_matches(&self, internal: &str) -> bool {
        self.as_str() == internal
    }

    fn final_segment(&self) -> &str {
        self.rsplit('/').next().unwrap_or(self)
    }
}

#[inline]
pub fn same(a: TypeName, b: TypeName) -> bool {
    a == b
}

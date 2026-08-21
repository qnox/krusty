//! Type model: Kotlin scalar, object, array, function, nullable, platform-flexible, and type-parameter
//! shapes.
//! Backend-specific names and descriptors are kept out of this module.

use crate::name_tree::{NameId, NameTree};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeName(NameId);

#[derive(Clone, Default)]
pub struct TypeNameList {
    names: Vec<TypeName>,
}

// The tree is concurrent (lock-free reads, internally-locked inserts), so the global interner is a
// bare shared instance.
static TYPE_NAMES: OnceLock<NameTree> = OnceLock::new();
static TYPE_PARAMETER_SOURCES: OnceLock<Mutex<HashMap<&'static str, &'static str>>> =
    OnceLock::new();

fn type_names() -> &'static NameTree {
    TYPE_NAMES.get_or_init(|| {
        let names = NameTree::default();
        for (path, expected) in BUILTIN_TYPE_NAMES {
            let actual = names.insert(path);
            assert_eq!(actual, expected.0, "built-in TypeName id for {path}");
        }
        names
    })
}

pub fn type_name(internal: &str) -> TypeName {
    if let Some(id) = type_names().get(internal) {
        return TypeName(id);
    }
    let Some((base, nested)) = split_nested_name(internal) else {
        return TypeName(type_names().insert(internal));
    };
    let mut name = TypeName(type_names().insert(base));
    for segment in nested
        .split(['.', '$'])
        .filter(|segment| !segment.is_empty())
    {
        name = type_name_nested_child(name, segment);
    }
    name
}

pub fn type_name_from(names: &NameTree, id: NameId) -> TypeName {
    let Some((base, nested)) = split_nested_name(names.segment(id)) else {
        return TypeName(type_names().insert_from(names, id));
    };
    let parent = names.parent(id).map_or(NameTree::ROOT, |parent| {
        type_names().insert_from(names, parent)
    });
    let mut name = TypeName(type_names().child_of(parent, base));
    for segment in nested
        .split(['.', '$'])
        .filter(|segment| !segment.is_empty())
    {
        name = type_name_nested_child(name, segment);
    }
    name
}

/// Find a global type-name identity in another name tree without mutating either tree.
pub(crate) fn existing_type_name_in(names: &NameTree, internal: TypeName) -> Option<NameId> {
    names.existing_from(type_names(), internal.name_id())
}

/// Copy a global type-name identity into another name tree without rendering it to text.
pub(crate) fn insert_type_name_in(names: &NameTree, internal: TypeName) -> NameId {
    names.insert_from(type_names(), internal.name_id())
}

fn split_nested_name(internal: &str) -> Option<(&str, &str)> {
    let classifier_start = internal.rfind('/').map_or(0, |slash| slash + 1);
    let classifier = &internal[classifier_start..];
    let separator = classifier
        .as_bytes()
        .iter()
        .position(|byte| matches!(byte, b'.' | b'$'))?;
    let base_end = classifier_start + separator;
    Some((&internal[..base_end], &internal[base_end + 1..]))
}

const KOTLIN_BOOLEAN: TypeName = TypeName(NameId(2));
const KOTLIN_BYTE: TypeName = TypeName(NameId(3));
const KOTLIN_SHORT: TypeName = TypeName(NameId(4));
const KOTLIN_INT: TypeName = TypeName(NameId(5));
const KOTLIN_LONG: TypeName = TypeName(NameId(6));
const KOTLIN_CHAR: TypeName = TypeName(NameId(7));
const KOTLIN_FLOAT: TypeName = TypeName(NameId(8));
const KOTLIN_DOUBLE: TypeName = TypeName(NameId(9));
const KOTLIN_UBYTE: TypeName = TypeName(NameId(10));
const KOTLIN_USHORT: TypeName = TypeName(NameId(11));
const KOTLIN_UINT: TypeName = TypeName(NameId(12));
const KOTLIN_ULONG: TypeName = TypeName(NameId(13));
const KOTLIN_STRING: TypeName = TypeName(NameId(14));

const BUILTIN_TYPE_NAMES: [(&str, TypeName); 13] = [
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

pub fn existing_type_name(internal: &str) -> Option<TypeName> {
    if let Some(id) = type_names().get(internal) {
        return Some(TypeName(id));
    }
    let (base, nested) = split_nested_name(internal)?;
    let mut name = TypeName(type_names().get(base)?);
    for segment in nested
        .split(['.', '$'])
        .filter(|segment| !segment.is_empty())
    {
        name = TypeName(type_names().existing_nested_child_of(name.name_id(), segment)?);
    }
    Some(name)
}

pub fn existing_type_name_child(parent: TypeName, segment: &str) -> Option<TypeName> {
    let Some((base, nested)) = split_nested_name(segment) else {
        return type_names()
            .existing_child_of(parent.name_id(), segment)
            .map(TypeName);
    };
    let mut name = TypeName(type_names().existing_child_of(parent.name_id(), base)?);
    for segment in nested
        .split(['.', '$'])
        .filter(|segment| !segment.is_empty())
    {
        name = TypeName(type_names().existing_nested_child_of(name.name_id(), segment)?);
    }
    Some(name)
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

/// Intern a generic type-parameter name. This must not be used for class/FQN storage; class names live
/// in the type name tree as [`TypeName`].
pub fn intern(name: &str) -> &'static str {
    static I: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let set = I.get_or_init(|| Mutex::new(HashSet::new()));
    let mut set = set.lock().unwrap();
    if let Some(&v) = set.get(name) {
        return v;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    set.insert(leaked);
    leaked
}

/// Intern one declaration-owned type-parameter identity and retain its source spelling separately.
/// The semantic key is opaque: callers compare it only by identity and never parse declaration
/// coordinates or spelling out of it.
pub(crate) fn declaration_type_parameter(
    compilation: u64,
    file: u32,
    declaration_start: u32,
    index: usize,
    source: &str,
) -> &'static str {
    let semantic = intern(&format!(
        "\0tp:{compilation}:{file}:{declaration_start}:{index}"
    ));
    let source = intern(source);
    TYPE_PARAMETER_SOURCES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(semantic, source);
    semantic
}

impl TypeName {
    pub const ROOT: TypeName = TypeName(NameId(0));

    fn name_id(self) -> NameId {
        self.0
    }

    pub fn matches(self, internal: &str) -> bool {
        type_names().get(internal) == Some(self.0)
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

    /// Immediate lexical classifier owner, decoded from the interned nested-class segment.
    pub fn nested_owner(self) -> Option<TypeName> {
        type_names().nested_owner(self.name_id()).map(TypeName)
    }

    /// Existing classifier nested directly in `self`, without rendering or interning a probe.
    pub fn existing_nested_child(self, nested: &str) -> Option<TypeName> {
        type_names()
            .existing_nested_child_of(self.name_id(), nested)
            .map(TypeName)
    }

    pub fn strip_prefix(self, prefix: &str) -> Option<String> {
        type_names().strip_prefix(self.name_id(), prefix)
    }

    pub fn unsigned_suffix_after_prefix(self, prefix: &str) -> Option<usize> {
        type_names().unsigned_suffix_after_prefix(self.name_id(), prefix)
    }

    pub fn replace(self, from: char, to: &str) -> String {
        self.render().replace(from, to)
    }

    pub fn render(self) -> String {
        type_names().render(self.0)
    }
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
}

impl InternalName for TypeName {
    fn internal_matches(&self, internal: &str) -> bool {
        self.matches(internal)
    }
}

impl InternalName for &TypeName {
    fn internal_matches(&self, internal: &str) -> bool {
        self.matches(internal)
    }
}

impl InternalName for &str {
    fn internal_matches(&self, internal: &str) -> bool {
        *self == internal
    }
}

impl InternalName for &String {
    fn internal_matches(&self, internal: &str) -> bool {
        self.as_str() == internal
    }
}

#[inline]
pub fn same(a: TypeName, b: TypeName) -> bool {
    a == b
}

/// Well-known class internal names, each inserted ONCE into the type-name tree and reused for id
/// comparison against other object type names.
pub mod wk {
    use super::{type_name, TypeName};
    use std::sync::OnceLock;
    macro_rules! names {
        ($($f:ident => $lit:literal),* $(,)?) => { $(
            #[inline]
            pub fn $f() -> TypeName {
                static S: OnceLock<TypeName> = OnceLock::new();
                *S.get_or_init(|| type_name($lit))
            }
        )* };
    }
    names! {
        continuation => "kotlin/coroutines/Continuation",
        any => "kotlin/Any",
        java_object => "java/lang/Object",
        java_enum => "java/lang/Enum",
    }
}

/// Intern a `Ty` to a canonical `&'static Ty` so a wrapped inner type (a `Nullable`/`TyParam` bound)
/// compares by value — the derived `Eq`/`Hash` follow the reference, so equal inner types must share
/// one pointer.
pub fn intern_ty(t: Ty) -> &'static Ty {
    static I: OnceLock<Mutex<HashSet<&'static Ty>>> = OnceLock::new();
    let set = I.get_or_init(|| Mutex::new(HashSet::new()));
    let mut set = set.lock().unwrap();
    if let Some(&v) = set.get(&t) {
        return v;
    }
    let leaked: &'static Ty = Box::leak(Box::new(t));
    set.insert(leaked);
    leaked
}

/// Intern a generic type-argument list to a canonical `&'static [Ty]` so equal instantiations share a
/// pointer (the derived `Eq`/`Hash` on `Ty::Obj` compares the slice by reference). Empty → `&[]`.
pub fn intern_tys(ts: &[Ty]) -> &'static [Ty] {
    if ts.is_empty() {
        return &[];
    }
    static I: OnceLock<Mutex<HashSet<&'static [Ty]>>> = OnceLock::new();
    let set = I.get_or_init(|| Mutex::new(HashSet::new()));
    let mut set = set.lock().unwrap();
    if let Some(&v) = set.get(ts) {
        return v;
    }
    let leaked: &'static [Ty] = Box::leak(ts.to_vec().into_boxed_slice());
    set.insert(leaked);
    leaked
}

/// A function type's signature: parameter types and return type. Interned (`intern_fnsig`) so
/// `Ty::Fun` stays `Copy`. Lets a `Fun`-typed call recover its real return type (not erased `Object`).
#[derive(Clone, PartialEq, Eq, Debug, Hash)]
pub struct FnSig {
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// Leading parameters that bind as context receivers.
    pub context_count: usize,
    /// Whether the parameter after the context receivers binds as `this`.
    pub has_receiver: bool,
    /// A `suspend` function type (`suspend (A) -> R`).
    pub suspend: bool,
}

/// Intern a `FnSig` to a canonical `&'static FnSig` (leaked; the compiler is short-lived).
pub fn intern_fnsig(s: FnSig) -> &'static FnSig {
    static I: OnceLock<Mutex<HashSet<&'static FnSig>>> = OnceLock::new();
    let set = I.get_or_init(|| Mutex::new(HashSet::new()));
    let mut set = set.lock().unwrap();
    if let Some(&v) = set.get(&s) {
        return v;
    }
    let leaked: &'static FnSig = Box::leak(Box::new(s));
    set.insert(leaked);
    leaked
}

/// The element type of a primitive specialized array class (`kotlin/IntArray` → `Int`), or `None` for a
/// non-primitive-array name. The unsigned arrays (`UIntArray`, …) keep their unsigned element so their
/// value-class identity survives; they still erase to the signed primitive array descriptor (`[I`).
/// The single canonical table — [`Ty::array_elem`], the constructor, and the backend descriptor logic
/// all route through it rather than each carrying their own copy.
pub fn prim_array_element(internal: impl InternalName) -> Option<Ty> {
    if internal.internal_matches("kotlin/IntArray") {
        Some(Ty::Int)
    } else if internal.internal_matches("kotlin/LongArray") {
        Some(Ty::Long)
    } else if internal.internal_matches("kotlin/ShortArray") {
        Some(Ty::Short)
    } else if internal.internal_matches("kotlin/ByteArray") {
        Some(Ty::Byte)
    } else if internal.internal_matches("kotlin/BooleanArray") {
        Some(Ty::Boolean)
    } else if internal.internal_matches("kotlin/CharArray") {
        Some(Ty::Char)
    } else if internal.internal_matches("kotlin/FloatArray") {
        Some(Ty::Float)
    } else if internal.internal_matches("kotlin/DoubleArray") {
        Some(Ty::Double)
    } else if internal.internal_matches("kotlin/UIntArray") {
        Some(Ty::UInt)
    } else if internal.internal_matches("kotlin/ULongArray") {
        Some(Ty::ULong)
    } else {
        None
    }
}

/// Element type fixed by a specialized primitive-array creator. Both the class-shaped size
/// constructor (`IntArray`) and its vararg factory (`intArrayOf`) name the same semantic element.
/// Resolution, signature inference, and intrinsic emission share this table.
pub fn primitive_array_creator_element(name: &str) -> Option<Ty> {
    Some(match name {
        "IntArray" | "intArrayOf" => Ty::Int,
        "LongArray" | "longArrayOf" => Ty::Long,
        "ShortArray" | "shortArrayOf" => Ty::Short,
        "ByteArray" | "byteArrayOf" => Ty::Byte,
        "BooleanArray" | "booleanArrayOf" => Ty::Boolean,
        "CharArray" | "charArrayOf" => Ty::Char,
        "FloatArray" | "floatArrayOf" => Ty::Float,
        "DoubleArray" | "doubleArrayOf" => Ty::Double,
        "UIntArray" | "uintArrayOf" => Ty::UInt,
        "ULongArray" | "ulongArrayOf" => Ty::ULong,
        _ => return None,
    })
}

/// The primitive specialized array class name for a primitive element (`Int` → `kotlin/IntArray`), or
/// `None` for a reference element (which lives in a boxed `Array<T>`). Inverse of [`prim_array_element`].
pub fn prim_array_name(elem: Ty) -> Option<&'static str> {
    Some(match elem {
        Ty::Int => "kotlin/IntArray",
        Ty::Long => "kotlin/LongArray",
        Ty::Short => "kotlin/ShortArray",
        Ty::Byte => "kotlin/ByteArray",
        Ty::Boolean => "kotlin/BooleanArray",
        Ty::Char => "kotlin/CharArray",
        Ty::Float => "kotlin/FloatArray",
        Ty::Double => "kotlin/DoubleArray",
        Ty::UInt => "kotlin/UIntArray",
        Ty::ULong => "kotlin/ULongArray",
        _ => return None,
    })
}

/// The JVM functional-interface internal name for each arity, `kotlin/jvm/functions/Function0..22`
/// (the arities the Kotlin stdlib declares). Indexed by arity; higher arities have no fixed interface.
pub const FUNCTION_N_INTERNAL: [&str; 23] = [
    "kotlin/jvm/functions/Function0",
    "kotlin/jvm/functions/Function1",
    "kotlin/jvm/functions/Function2",
    "kotlin/jvm/functions/Function3",
    "kotlin/jvm/functions/Function4",
    "kotlin/jvm/functions/Function5",
    "kotlin/jvm/functions/Function6",
    "kotlin/jvm/functions/Function7",
    "kotlin/jvm/functions/Function8",
    "kotlin/jvm/functions/Function9",
    "kotlin/jvm/functions/Function10",
    "kotlin/jvm/functions/Function11",
    "kotlin/jvm/functions/Function12",
    "kotlin/jvm/functions/Function13",
    "kotlin/jvm/functions/Function14",
    "kotlin/jvm/functions/Function15",
    "kotlin/jvm/functions/Function16",
    "kotlin/jvm/functions/Function17",
    "kotlin/jvm/functions/Function18",
    "kotlin/jvm/functions/Function19",
    "kotlin/jvm/functions/Function20",
    "kotlin/jvm/functions/Function21",
    "kotlin/jvm/functions/Function22",
];

/// `java.lang.annotation.ElementType` constants in DECLARATION order. kotlinc projects a Kotlin
/// `@Target` onto an `EnumSet<ElementType>`, so the `@java.lang.annotation.Target` mirror it writes is
/// always in this order — never the order the Kotlin targets were written in.
pub const JAVA_ELEMENT_TYPES: [&str; 10] = [
    "TYPE",
    "FIELD",
    "METHOD",
    "PARAMETER",
    "CONSTRUCTOR",
    "LOCAL_VARIABLE",
    "ANNOTATION_TYPE",
    "PACKAGE",
    "TYPE_PARAMETER",
    "TYPE_USE",
];

/// Index into [`JAVA_ELEMENT_TYPES`] for a `kotlin.annotation.AnnotationTarget` constant — the Java
/// counterpart kotlinc mirrors that target onto. `None` for a Kotlin-only target (`PROPERTY`, `FILE`,
/// `TYPEALIAS`, `EXPRESSION`, which the JVM cannot express) or an unknown name; such a target
/// contributes nothing to the mirror, though the mirror itself is still emitted — a set of only
/// Kotlin-only targets mirrors to an EMPTY array, matching kotlinc, rather than being omitted.
///
/// The rows are measured against kotlinc, not derived: the mapping is neither an identity (`CLASS`
/// becomes `TYPE`, `VALUE_PARAMETER` becomes `PARAMETER`) nor injective (`FUNCTION`,
/// `PROPERTY_GETTER` and `PROPERTY_SETTER` all become `METHOD`, and collapse to one entry).
pub fn java_element_type_of_annotation_target(target: &str) -> Option<usize> {
    let element = match target {
        "CLASS" => "TYPE",
        "ANNOTATION_CLASS" => "ANNOTATION_TYPE",
        "TYPE_PARAMETER" => "TYPE_PARAMETER",
        "FIELD" => "FIELD",
        "LOCAL_VARIABLE" => "LOCAL_VARIABLE",
        "VALUE_PARAMETER" => "PARAMETER",
        "CONSTRUCTOR" => "CONSTRUCTOR",
        "FUNCTION" | "PROPERTY_GETTER" | "PROPERTY_SETTER" => "METHOD",
        "TYPE" => "TYPE_USE",
        // PROPERTY / FILE / TYPEALIAS / EXPRESSION have no `ElementType` counterpart.
        _ => return None,
    };
    JAVA_ELEMENT_TYPES.iter().position(|&e| e == element)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Ty {
    Unit,
    /// A reference type by internal name (e.g. `demo/Point`), with its generic type arguments
    /// (`List<Int>` → `Obj("kotlin/collections/List", [Int])`). Arguments are interned (`intern_tys`)
    /// so equal instantiations share a pointer and the front end can recover element/member types.
    /// Empty for a non-generic class.
    Obj(TypeName, &'static [Ty]),
    /// The type of the `null` literal — assignable only to nullable types.
    Null,
    /// The bottom type (`Nothing`): the type of `throw`/`return` expressions. Assignable to every
    /// type; an expression of this type never yields a value (it always diverges).
    Nothing,
    /// Placeholder after a type error, suppresses cascading diagnostics.
    Error,
    /// A declaration whose type is NOT DETERMINED YET — recorded by signature collection for an
    /// implicitly-typed declaration the resolution engine has not resolved.
    ///
    /// Distinct from [`Ty::Error`] on purpose. Spelling "not yet" and "gave up" the same way is what
    /// forces every read site to be taught the difference by hand: a reader that finds `Error`
    /// cannot know whether asking the engine would help, so each place has to be patched
    /// individually and any place that is missed silently publishes a wrong type. `Pending` is
    /// answerable — a read of it demands the declaration, and it must never reach emission or
    /// `@Metadata`, which is an invariant that can be asserted rather than hoped for.
    Pending,
    /// A Kotlin function type `(A, B) -> R`. The front end keeps the real parameter/return types
    /// (interned `FnSig`) so a call through a `Fun` value recovers its return type.
    Fun(&'static FnSig),
    /// A nullable type `T?`. Wraps the interned non-null type. Kotlin has no `T??`, so the inner type
    /// is never itself `Nullable` (the [`Ty::nullable`] constructor enforces this).
    Nullable(&'static Ty),
    /// A Java platform type `T!`: flexible between `T` and `T?`. Unlike [`Nullable`](Ty::Nullable),
    /// direct member access is permitted; assignability accepts it at either nullability bound.
    PlatformNullable(&'static Ty),
    /// Use-site type projections. These occur only as generic arguments (`Box<in T>` / `Box<out T>`)
    /// and remain semantic until assignability/inference consumes their variance.
    InProjection(&'static Ty),
    OutProjection(&'static Ty),
    /// A generic type-parameter reference (`T`), carrying its name and declared upper bound
    /// (`<T : CharSequence>` → bound `CharSequence`; unbounded `<T>` → bound `kotlin/Any`). The checker
    /// reasons about `T` as `T` (subtyping against the bound, substitution at instantiation); runtime
    /// erasure is a backend concern.
    TyParam(&'static str, &'static Ty),
}

pub(crate) fn stored_value_ty(ty: Ty) -> Ty {
    if ty == Ty::Unit {
        Ty::obj("kotlin/Unit")
    } else {
        ty
    }
}

/// Replace every NOT-DETERMINED marker inside `ty` with `replacement`.
pub fn ty_replace_pending(ty: Ty, replacement: Ty) -> Ty {
    match ty {
        Ty::Pending => replacement,
        Ty::Nullable(inner) => Ty::nullable(ty_replace_pending(*inner, replacement)),
        Ty::Obj(name, args) if args.iter().any(|argument| argument.mentions_pending()) => {
            Ty::obj_args_name(
                name,
                &args
                    .iter()
                    .map(|argument| ty_replace_pending(*argument, replacement))
                    .collect::<Vec<_>>(),
            )
        }
        // A FUNCTION type carries the marker in its return or its parameters, not in type arguments:
        // `C::foo` is `(C) -> <not determined>` while `foo`'s own return is being determined. Without
        // this arm such a type passed through untouched and the declaration declined even though the
        // answer had already been obtained.
        Ty::Fun(signature) if signature.mentions_pending() => Ty::fun_with_shape(
            signature
                .params
                .iter()
                .map(|parameter| ty_replace_pending(*parameter, replacement))
                .collect::<Vec<_>>(),
            ty_replace_pending(signature.ret, replacement),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ),
        other => other,
    }
}

impl FnSig {
    /// Whether this signature carries the NOT-DETERMINED marker in its return or parameters.
    pub fn mentions_pending(&self) -> bool {
        self.ret.mentions_pending()
            || self
                .params
                .iter()
                .any(|parameter| parameter.mentions_pending())
    }
}

impl Ty {
    // Kotlin built-ins are ordinary classifiers. These associated constants preserve concise call
    // sites and const-pattern matching without creating scalar/string enum variants alongside `Obj`.
    #[allow(non_upper_case_globals)]
    pub const Boolean: Ty = Ty::Obj(KOTLIN_BOOLEAN, &[]);
    #[allow(non_upper_case_globals)]
    pub const Byte: Ty = Ty::Obj(KOTLIN_BYTE, &[]);
    #[allow(non_upper_case_globals)]
    pub const Short: Ty = Ty::Obj(KOTLIN_SHORT, &[]);
    #[allow(non_upper_case_globals)]
    pub const Int: Ty = Ty::Obj(KOTLIN_INT, &[]);
    #[allow(non_upper_case_globals)]
    pub const Long: Ty = Ty::Obj(KOTLIN_LONG, &[]);
    #[allow(non_upper_case_globals)]
    pub const Char: Ty = Ty::Obj(KOTLIN_CHAR, &[]);
    #[allow(non_upper_case_globals)]
    pub const Float: Ty = Ty::Obj(KOTLIN_FLOAT, &[]);
    #[allow(non_upper_case_globals)]
    pub const Double: Ty = Ty::Obj(KOTLIN_DOUBLE, &[]);
    #[allow(non_upper_case_globals)]
    pub const UByte: Ty = Ty::Obj(KOTLIN_UBYTE, &[]);
    #[allow(non_upper_case_globals)]
    pub const UShort: Ty = Ty::Obj(KOTLIN_USHORT, &[]);
    #[allow(non_upper_case_globals)]
    pub const UInt: Ty = Ty::Obj(KOTLIN_UINT, &[]);
    #[allow(non_upper_case_globals)]
    pub const ULong: Ty = Ty::Obj(KOTLIN_ULONG, &[]);
    #[allow(non_upper_case_globals)]
    pub const String: Ty = Ty::Obj(KOTLIN_STRING, &[]);

    /// A class reference type from an internal name (no generic arguments).
    pub fn obj(internal: &str) -> Ty {
        Ty::Obj(type_name(internal), &[])
    }

    /// A class reference type from an existing tree-backed internal name.
    pub fn obj_name(internal: TypeName) -> Ty {
        Ty::Obj(internal, &[])
    }

    /// A generic class reference type — `internal<args…>`.
    pub fn obj_args(internal: &str, args: &[Ty]) -> Ty {
        Ty::Obj(type_name(internal), intern_tys(args))
    }

    pub fn obj_args_name(internal: TypeName, args: &[Ty]) -> Ty {
        Ty::Obj(internal, intern_tys(args))
    }

    /// The generic type arguments of a reference type (empty for non-generic / non-`Obj`).
    pub fn type_args(self) -> &'static [Ty] {
        match self {
            Ty::Obj(_, args) => args,
            Ty::TyParam(_, b) | Ty::PlatformNullable(b) => b.type_args(),
            _ => &[],
        }
    }

    /// Substitute known type parameters and erase an unbound parameter to its non-null upper bound.
    /// This is a type-shape operation used while publishing and specializing metadata; it is not an
    /// overload-selection decision.
    pub fn substitute_erased(self, bindings: &std::collections::HashMap<String, Ty>) -> Ty {
        match self {
            Ty::TyParam(name, bound) => bindings
                .get(name)
                .copied()
                .map(|binding| {
                    if bound.upper_bound_admits_null() {
                        binding
                    } else {
                        binding.non_null()
                    }
                })
                .unwrap_or_else(|| bound.non_null()),
            Ty::Fun(signature) => Ty::fun_with_shape(
                signature
                    .params
                    .iter()
                    .map(|parameter| parameter.substitute_erased(bindings))
                    .collect(),
                signature.ret.substitute_erased(bindings),
                signature.context_count,
                signature.has_receiver,
                signature.suspend,
            ),
            Ty::Nullable(inner) => Ty::nullable(inner.substitute_erased(bindings)),
            Ty::PlatformNullable(inner) => Ty::platform_nullable(inner.substitute_erased(bindings)),
            Ty::InProjection(inner) => Ty::in_projection(inner.substitute_erased(bindings)),
            Ty::OutProjection(inner) => Ty::out_projection(inner.substitute_erased(bindings)),
            Ty::Obj(internal, arguments) if !arguments.is_empty() => {
                let arguments = arguments
                    .iter()
                    .map(|argument| argument.substitute_erased(bindings))
                    .collect::<Vec<_>>();
                Ty::obj_args_name(internal, &arguments)
            }
            _ => self,
        }
    }

    /// An array whose element is `elem`, choosing the array *kind* the way Kotlin does: a primitive
    /// element yields the specialized primitive array (`Int` → `IntArray` = `Obj("kotlin/IntArray")`,
    /// `[I`), any reference element yields the boxed `Array<T>` (`String` → `Obj("kotlin/Array", [String])`,
    /// `[Ljava/lang/String;`). To force a boxed `Array<Int>` (`[Ljava/lang/Integer;`) construct it
    /// directly as `Ty::obj_args("kotlin/Array", &[Ty::Int])`.
    pub fn array(elem: Ty) -> Ty {
        match prim_array_name(elem) {
            Some(n) => Ty::obj(n),
            None => Ty::obj_args("kotlin/Array", &[elem]),
        }
    }

    /// The element type if this is an array — a primitive specialized array (`IntArray` → `Int`) or a
    /// Kotlin `Array<T>` carried as `Obj("kotlin/Array", [T])` (its *logical* element, e.g. `Int` for
    /// `Array<Int>`; the wrapper boxing is the backend's concern, not the type's).
    pub fn array_elem(self) -> Option<Ty> {
        match self {
            Ty::Obj(n, args) if n.matches("kotlin/Array") => args.first().copied(),
            Ty::Obj(n, _) => prim_array_element(n),
            Ty::TyParam(_, b) | Ty::PlatformNullable(b) => b.array_elem(),
            _ => None,
        }
    }

    /// The type produced by READING one element from this array. [`Ty::array_elem`] preserves the
    /// classifier argument exactly, including a use-site projection; an expression cannot itself
    /// have that projection wrapper. Reading `Array<out T>` yields `T`, while an `in`-projected
    /// array exposes only nullable `Any`. Primitive and invariant arrays keep their declared element.
    pub fn array_read_elem(self) -> Option<Ty> {
        Some(match self.array_elem()? {
            Ty::OutProjection(inner) => *inner,
            Ty::InProjection(_) => Ty::nullable(Ty::obj("kotlin/Any")),
            element => element,
        })
    }

    /// Whether this type is any array — a primitive specialized array (`kotlin/IntArray`, …) or a boxed
    /// `Array<T>` (`Obj("kotlin/Array", [T])`). The single array-ness predicate; consumers must use this
    /// instead of pattern-matching a specific spelling so the representation can migrate under them.
    pub fn is_array(self) -> bool {
        matches!(self, Ty::Obj(n, _) if n.matches("kotlin/Array") || prim_array_element(n).is_some())
    }

    /// Whether this is a boxed `Array<T>` (`aaload`/`aastore`, elements stored as objects) as opposed to
    /// a primitive specialized array (`IntArray` → `iaload`/`iastore`). The only bit the backend needs to
    /// pick array opcodes; a reference array boxes primitive [`array_elem`]s at the store boundary.
    pub fn is_reference_array(self) -> bool {
        matches!(self, Ty::Obj(n, _) if n.matches("kotlin/Array"))
    }

    /// The nullable form `T?` of a type. Idempotent (Kotlin has no `T??`), and degenerate inputs
    /// collapse: `Null?` = `Null`, `Error?` = `Error`. `Nothing?` is kept — it's the real type of the
    /// `null` literal.
    pub fn nullable(inner: Ty) -> Ty {
        match inner {
            Ty::Nullable(_) | Ty::Null | Ty::Error => inner,
            Ty::PlatformNullable(inner) => Ty::Nullable(inner),
            _ => Ty::Nullable(intern_ty(inner)),
        }
    }

    /// Java's flexible platform-nullability form `T!`.
    pub fn platform_nullable(inner: Ty) -> Ty {
        match inner {
            Ty::Nullable(inner) | Ty::PlatformNullable(inner) => Ty::PlatformNullable(inner),
            Ty::Null | Ty::Error => inner,
            _ => Ty::PlatformNullable(intern_ty(inner)),
        }
    }

    pub fn in_projection(inner: Ty) -> Ty {
        Ty::InProjection(intern_ty(inner))
    }

    pub fn out_projection(inner: Ty) -> Ty {
        Ty::OutProjection(intern_ty(inner))
    }

    pub fn projection_inner(self) -> Option<Ty> {
        match self {
            Ty::InProjection(inner) | Ty::OutProjection(inner) => Some(*inner),
            _ => None,
        }
    }

    /// Whether this type is nullable (`T?`).
    pub fn is_nullable(self) -> bool {
        matches!(self, Ty::Nullable(_))
    }

    /// Whether this type occurrence admits `null`: an explicit nullable type or a flexible platform
    /// type. A bare type parameter does not admit `null`, even when its upper bound is `Any?`: the
    /// parameter may still be instantiated with a non-null type, and only `T?` accepts the literal.
    /// This is distinct from [`Self::is_nullable`], which identifies source `T?` syntax and must not
    /// collapse `T!` to that single bound.
    pub fn admits_null(self) -> bool {
        matches!(self, Ty::Nullable(_) | Ty::PlatformNullable(_) | Ty::Null)
    }

    /// Whether this type's upper-bound chain admits `null`. This is for substitution and generic
    /// constraint reasoning only; it deliberately does not make a bare [`Ty::TyParam`] nullable as
    /// a source type occurrence (see [`Self::admits_null`]).
    pub fn upper_bound_admits_null(self) -> bool {
        let mut current = self;
        let mut seen = std::collections::HashSet::new();
        loop {
            match current {
                Ty::Nullable(_) | Ty::PlatformNullable(_) | Ty::Null => return true,
                Ty::TyParam(name, bound) if seen.insert(name) => current = *bound,
                _ => return false,
            }
        }
    }

    /// The non-null form: strips a `?` if present, else returns `self`.
    pub fn non_null(self) -> Ty {
        match self {
            Ty::Nullable(inner) => *inner,
            Ty::PlatformNullable(inner) => *inner,
            _ => self,
        }
    }

    /// Lower bound of a flexible Java platform type; fixed Kotlin types are unchanged.
    pub fn platform_lower_bound(self) -> Ty {
        match self {
            Ty::PlatformNullable(inner) => *inner,
            _ => self,
        }
    }

    /// Kotlin class identity for types that have one in source-level member/subtype lookup.
    ///
    /// This is not a JVM descriptor mapping: it returns Kotlin internal names (`kotlin/Int`,
    /// `kotlin/String`, user class names), and deliberately ignores backend wrapper/internal names.
    /// Nullable and type-parameter forms delegate to their non-null/bound class identity.
    pub fn kotlin_class_internal(self) -> Option<TypeName> {
        match self {
            Ty::Obj(i, _) => Some(i),
            Ty::Nullable(inner) | Ty::PlatformNullable(inner) => inner.kotlin_class_internal(),
            Ty::TyParam(_, bound) => bound.kotlin_class_internal(),
            _ => None,
        }
    }

    /// Whether this is Kotlin's semantic top reference type, including a physical JVM spelling that may
    /// arrive from classpath metadata.
    pub fn is_erased_top(self) -> bool {
        self.non_null()
            .obj_internal()
            .is_some_and(|n| same(n, wk::any()) || same(n, wk::java_object()))
    }

    /// The JVM functional-interface internal name (`kotlin/jvm/functions/FunctionN`) a function type
    /// implements — used for subtype/assignability tests against a user class that declares a
    /// function-type supertype. Kept SEPARATE from [`kotlin_class_internal`] (which returns `None` for a
    /// `Ty::Fun`): a function value is not, in general, interchangeable with its `FunctionN` class in the
    /// backend, so only the assignability checks that want the interface identity opt in here.
    pub fn function_interface_internal(self) -> Option<&'static str> {
        match self {
            // A `suspend` function type erases to the arity+1 interface (the trailing
            // `Continuation` parameter): `suspend () -> Unit` is a `Function1` at runtime, so an
            // `as`/`is` against it must test `Function1`, not `Function0`.
            Ty::Fun(s) => FUNCTION_N_INTERNAL
                .get(s.params.len() + usize::from(s.suspend))
                .copied(),
            _ => None,
        }
    }

    /// The canonical **extension-receiver key**: the `Ty` two receivers must share for an extension
    /// declared on one to resolve on the other. A Kotlin-level erasure that reproduces the equivalence
    /// the old JVM descriptor key gave for *reference* receivers, without referencing the backend — it
    /// drops a nullable reference's `?` (`String?` and `String` take the same extensions), generic
    /// arguments (`List<Int>` and `List<String>` share `List`'s extensions, recursively through
    /// arrays), and a type parameter to its (also-erased) bound (`fun T.f()` keys under the bound).
    /// `null`/`Nothing`/error key under `Any` (a `null` receiver reaches an `Any?` extension). Replaces
    /// a computed JVM descriptor string, which leaked the backend representation and allocated on every
    /// insert and lookup.
    ///
    /// It is deliberately NOT a faithful descriptor clone in two corners the descriptor folded only by
    /// accident of JVM erasure: signed vs unsigned primitives stay distinct (the descriptor merged
    /// `Int`/`UInt` because both erase to `I`), and function-type receivers stay distinct by full
    /// signature (the descriptor merged every arity-N `Fun` to `FunctionN`; that merge let an
    /// `((Int)->Int).f()` extension resolve on an `((String)->String)` receiver, which kotlinc rejects).
    /// A nullable *primitive* IS kept distinct from the unboxed primitive (`Int?` boxes — same key as an
    /// already-boxed `Array<Int>` element — while `Int` does not), matching the descriptor.
    pub fn erased_recv(self) -> Ty {
        match self {
            // Nullability is semantic and remains in the receiver key. No JVM wrapper classifier is
            // manufactured in core merely because this receiver may use one on a JVM boundary.
            Ty::Nullable(inner) if inner.scalar_value_repr().is_some() => {
                Ty::nullable(inner.erased_recv())
            }
            Ty::Nullable(inner) => inner.erased_recv(),
            Ty::PlatformNullable(inner) => inner.erased_recv(),
            Ty::TyParam(_, b) => b.erased_recv(),
            // `Array<T>` keeps its array-ness but erases the ELEMENT's own generics (`Array<List<Int>>` →
            // `Array<List>`) — an array receiver keys per element class. Use `obj_args` (NOT `Ty::array`,
            // which collapses a bare-primitive element to a `IntArray` = `[I`, breaking the boxed
            // `Array<Int>` = `[Integer;` receiver) so the boxed array form is preserved.
            Ty::Obj(n, args) if n.matches("kotlin/Array") => {
                let e = args
                    .first()
                    .copied()
                    .unwrap_or_else(|| Ty::obj("kotlin/Any"));
                Ty::obj_args("kotlin/Array", &[e.erased_recv()])
            }
            Ty::Obj(n, _) => Ty::Obj(n, &[]),
            // `null`/`Nothing` (and the error placeholder) are subtypes of every reference type, so a
            // receiver of one of these can invoke an `Any`/`Any?`-receiver extension — key them under
            // `Any` (`null.unsafeCast()` reaches `fun <T> Any?.unsafeCast()`).
            Ty::Null | Ty::Nothing | Ty::Error => Ty::obj("kotlin/Any"),
            _ => self,
        }
    }

    /// Semantic extension receiver key preserving nullability and generic receiver shape.
    pub fn extension_recv_key(self) -> Ty {
        match self {
            Ty::Nullable(inner) => Ty::nullable(inner.extension_recv_key()),
            Ty::PlatformNullable(inner) => Ty::platform_nullable(inner.extension_recv_key()),
            Ty::TyParam(_, bound) => Ty::ty_param("\u{0}", bound.extension_recv_key()),
            Ty::Obj(n, args) if n.matches("kotlin/Array") => {
                let element = args
                    .first()
                    .copied()
                    .unwrap_or_else(|| Ty::obj("kotlin/Any"));
                Ty::obj_args("kotlin/Array", &[element.extension_recv_key()])
            }
            Ty::Obj(n, _) => Ty::Obj(n, &[]),
            _ => self,
        }
    }

    /// Candidate extension-receiver lookup keys, most-specific first. Generic receivers such as
    /// `val <T> T.p` or `val <T> Array<T>.p` register under `Any`/`Array<Any>`, while concrete receivers
    /// keep their precise erased key first so concrete overloads still win.
    pub fn erased_recv_candidates(self) -> Vec<Ty> {
        let mut keys = vec![self.erased_recv()];
        if matches!(keys[0], Ty::Obj(n, _) if n.matches("kotlin/Array")) {
            keys.push(Ty::obj_args("kotlin/Array", &[Ty::obj("kotlin/Any")]));
        }
        keys.push(Ty::obj("kotlin/Any"));
        keys.dedup();
        keys
    }

    /// A generic type-parameter type `T` with the given declared upper bound (`kotlin/Any` if unbounded).
    pub fn ty_param(name: &str, bound: Ty) -> Ty {
        Ty::TyParam(intern(name), intern_ty(bound))
    }

    /// Whether this is a generic type-parameter reference (`T`).
    pub fn is_ty_param(self) -> bool {
        matches!(self, Ty::TyParam(..))
    }

    /// Whether a type parameter appears ANYWHERE in this type — as the type itself, a type argument,
    /// an array element, a function parameter/return, or under a `?`. A type that mentions one is not
    /// yet a concrete answer: it still needs the use site's substitution, so asserting it (as a
    /// reference type's argument, say) records `T.() -> String` where `Int.() -> String` is meant.
    /// Whether this type carries the NOT-DETERMINED marker anywhere inside it.
    ///
    /// `Ty::Pending` is not only a whole answer: a declaration can be typed `KProperty1<C, Pending>`
    /// by referencing a member whose own type is still being determined. Publishing that is exactly
    /// what publishing the bare marker would be, and the emission boundary rejects it the same way,
    /// so "is this determined" has to ask about the whole type rather than its outermost layer.
    pub fn mentions_pending(self) -> bool {
        self.mentions_marker(&|ty| ty == Ty::Pending)
    }

    /// Whether this type carries the ERROR placeholder anywhere inside it.
    ///
    /// The same containment question as [`mentions_pending`](Ty::mentions_pending), asked about
    /// "gave up" rather than "not yet". A publish boundary needs both: an answer of `Error` is not a
    /// type either, and it erases to `java/lang/Object` in a descriptor and to `<error>` in
    /// `@Metadata`, so publishing one is how a resolution failure turns into a wrong program rather
    /// than a diagnostic.
    pub fn mentions_error(self) -> bool {
        self.mentions_marker(&|ty| ty == Ty::Error)
    }

    /// The containment walk both markers share: the type itself, a nullability/projection wrapper's
    /// inner type, a reference type's arguments, and a function type's parameters and return.
    fn mentions_marker(self, marker: &dyn Fn(Ty) -> bool) -> bool {
        if marker(self) {
            return true;
        }
        match self {
            Ty::Nullable(inner) | Ty::PlatformNullable(inner) => inner.mentions_marker(marker),
            Ty::InProjection(inner) | Ty::OutProjection(inner) => inner.mentions_marker(marker),
            Ty::Obj(_, args) => args.iter().any(|a| a.mentions_marker(marker)),
            Ty::Fun(signature) => {
                signature.ret.mentions_marker(marker)
                    || signature.params.iter().any(|p| p.mentions_marker(marker))
            }
            _ => false,
        }
    }

    pub fn mentions_ty_param(self) -> bool {
        match self {
            Ty::TyParam(..) => true,
            Ty::Nullable(inner) | Ty::PlatformNullable(inner) => inner.mentions_ty_param(),
            Ty::Obj(_, args) => args.iter().any(|a| a.mentions_ty_param()),
            Ty::Fun(signature) => {
                signature.ret.mentions_ty_param()
                    || signature.params.iter().any(|p| p.mentions_ty_param())
            }
            _ => false,
        }
    }

    /// The name of a type-parameter type (`T`), else `None`.
    pub fn ty_param_name(self) -> Option<&'static str> {
        match self {
            Ty::TyParam(n, _) => Some(n),
            _ => None,
        }
    }

    /// The declared upper bound of a type-parameter type, else `None`.
    pub fn ty_param_bound(self) -> Option<Ty> {
        match self {
            Ty::TyParam(_, b) => Some(*b),
            _ => None,
        }
    }

    /// The unboxed primitive of a nullable primitive (`Int?` → `Int`), else `None`. Replaces the old
    /// "is this a boxed-wrapper `Obj`?" probe (`t.obj_internal().and_then(prim_of_wrapper)`).
    pub fn nullable_primitive(self) -> Option<Ty> {
        match self {
            Ty::Nullable(inner) | Ty::PlatformNullable(inner) if inner.boxed_ref().is_some() => {
                Some(*inner)
            }
            _ => None,
        }
    }

    /// The nullable form `T?` of a primitive that krusty can box (`Int` → `Int?`, `UInt` → `UInt?` boxed
    /// as `kotlin/UInt`). `None` for a non-primitive (already a reference). Unsigned boxes via its wrapper,
    /// so it is supported (parallel to [`Ty::nullable_primitive`], which already admits unsigned).
    pub fn nullable_boxed(self) -> Option<Ty> {
        self.boxed_ref().is_some().then(|| Ty::nullable(self))
    }

    /// Source-level nullable form for non-reference values that still have a valid reference
    /// representation. `Unit?` and `Nothing?` are real source types; primitive `T?` is represented as
    /// `Nullable(T)` until the backend picks its boxed carrier.
    pub fn nullable_non_ref(self) -> Option<Ty> {
        match self {
            Ty::Nothing | Ty::Unit => Some(Ty::nullable(self)),
            _ => self.nullable_boxed(),
        }
    }

    /// A function type `(params) -> ret`.
    pub fn fun(params: Vec<Ty>, ret: Ty) -> Ty {
        Self::fun_with_shape(params, ret, 0, false, false)
    }

    pub fn fun_with_shape(
        params: Vec<Ty>,
        ret: Ty,
        context_count: usize,
        has_receiver: bool,
        suspend: bool,
    ) -> Ty {
        Ty::Fun(intern_fnsig(FnSig {
            context_count: context_count.min(params.len()),
            params,
            ret,
            has_receiver,
            suspend,
        }))
    }

    /// A context function type.
    pub fn fun_context(params: Vec<Ty>, ret: Ty, context_count: usize) -> Ty {
        Self::fun_with_shape(params, ret, context_count, false, false)
    }

    /// A `suspend` function type `suspend (params) -> ret`.
    pub fn fun_suspend(params: Vec<Ty>, ret: Ty) -> Ty {
        Self::fun_with_shape(params, ret, 0, false, true)
    }

    /// A suspend context function type.
    pub fn fun_suspend_context(params: Vec<Ty>, ret: Ty, context_count: usize) -> Ty {
        Self::fun_with_shape(params, ret, context_count, false, true)
    }

    /// Arity of a function type.
    pub fn fun_arity(self) -> Option<u8> {
        match self {
            Ty::Fun(s) => Some(s.params.len() as u8),
            _ => None,
        }
    }

    /// Return type of a function type.
    pub fn fun_ret(self) -> Option<Ty> {
        match self {
            Ty::Fun(s) => Some(s.ret),
            _ => None,
        }
    }

    /// Parameter types of a function type.
    pub fn fun_params(self) -> Option<&'static [Ty]> {
        match self {
            Ty::Fun(s) => Some(&s.params),
            _ => None,
        }
    }

    pub fn from_name(name: &str) -> Option<Ty> {
        // Qualified source spellings denote the same builtin classifier as their default-import
        // spelling. Canonicalize them here so `kotlin.Unit` cannot leak into semantic checking as
        // `Obj("kotlin/Unit")` while `Unit` is `Ty::Unit` (and likewise for scalar classifiers).
        let name = name.strip_prefix("kotlin.").unwrap_or(name);
        Some(match name {
            "Int" => Ty::Int,
            "Byte" => Ty::Byte,
            "Short" => Ty::Short,
            "Long" => Ty::Long,
            "Float" => Ty::Float,
            "Double" => Ty::Double,
            "Boolean" => Ty::Boolean,
            "Char" => Ty::Char,
            "UByte" => Ty::UByte,
            "UShort" => Ty::UShort,
            "UInt" => Ty::UInt,
            "ULong" => Ty::ULong,
            "String" => Ty::String,
            "Unit" => Ty::Unit,
            "Nothing" => Ty::Nothing,
            "Any" => Ty::obj("kotlin/Any"),
            _ => return None,
        })
    }

    /// The element type of a specialized primitive array type name (`IntArray` → `Int`, …).
    /// `Array<T>` is handled separately (it carries its element as a type argument).
    pub fn primitive_array_element(name: &str) -> Option<Ty> {
        primitive_array_creator_element(name).filter(|_| name.ends_with("Array"))
    }

    /// JVM wrapper classifier for a Kotlin scalar. This is a temporary compatibility seam used by the
    /// JVM pipeline; semantic types remain the Kotlin classifiers above.
    pub fn boxed_ref(self) -> Option<Ty> {
        Some(Ty::obj(match self {
            Ty::Int => "java/lang/Integer",
            Ty::Byte => "java/lang/Byte",
            Ty::Short => "java/lang/Short",
            Ty::Long => "java/lang/Long",
            Ty::Float => "java/lang/Float",
            Ty::Double => "java/lang/Double",
            Ty::Boolean => "java/lang/Boolean",
            Ty::Char => "java/lang/Character",
            // Unsigned types box to their OWN inline-class wrapper (`UInt` → `kotlin/UInt`), not a
            // `java/lang/*`; `kotlin_prim_to_wrapper` maps the wrapper to itself.
            Ty::UByte => "kotlin/UByte",
            Ty::UShort => "kotlin/UShort",
            Ty::UInt => "kotlin/UInt",
            Ty::ULong => "kotlin/ULong",
            _ => return None,
        }))
    }

    /// Boxed JVM wrapper for a primitive, excluding unsigned inline classes.
    pub fn jvm_boxed_ref(self) -> Option<Ty> {
        self.boxed_ref().filter(|_| !self.is_unsigned())
    }

    /// Inverse JVM-wrapper mapping. Semantic `kotlin/Int` is already the classifier and does not match.
    pub fn unboxed_primitive(self) -> Option<Ty> {
        Some(match self {
            Ty::Obj(n, _) if n.matches("java/lang/Integer") => Ty::Int,
            Ty::Obj(n, _) if n.matches("java/lang/Byte") => Ty::Byte,
            Ty::Obj(n, _) if n.matches("java/lang/Short") => Ty::Short,
            Ty::Obj(n, _) if n.matches("java/lang/Long") => Ty::Long,
            Ty::Obj(n, _) if n.matches("java/lang/Float") => Ty::Float,
            Ty::Obj(n, _) if n.matches("java/lang/Double") => Ty::Double,
            Ty::Obj(n, _) if n.matches("java/lang/Boolean") => Ty::Boolean,
            Ty::Obj(n, _) if n.matches("java/lang/Character") => Ty::Char,
            _ => return None,
        })
    }

    /// Render a Kotlin source type.
    pub fn source_name(self) -> String {
        self.source_name_with_type_parameter(&|name| type_parameter_source_name(name).to_string())
    }

    /// Render a Kotlin source type while letting a diagnostic qualify type-parameter names with
    /// their declaration owner. Semantic type identities remain unchanged.
    pub(crate) fn source_name_with_type_parameter(
        self,
        type_parameter: &dyn Fn(&str) -> String,
    ) -> String {
        self.source_name_with_type_parameter_in(std::slice::from_ref(&self), type_parameter)
    }

    /// Render one type among the types named by the same diagnostic. Classifiers normally use their
    /// simple source name; colliding classifiers retain their package so the message distinguishes
    /// them.
    pub(crate) fn source_name_with_type_parameter_in(
        self,
        context: &[Ty],
        type_parameter: &dyn Fn(&str) -> String,
    ) -> String {
        match self {
            Ty::Int => "Int".to_string(),
            Ty::Byte => "Byte".to_string(),
            Ty::Short => "Short".to_string(),
            Ty::Long => "Long".to_string(),
            Ty::Float => "Float".to_string(),
            Ty::Double => "Double".to_string(),
            Ty::Boolean => "Boolean".to_string(),
            Ty::Char => "Char".to_string(),
            Ty::UByte => "UByte".to_string(),
            Ty::UShort => "UShort".to_string(),
            Ty::UInt => "UInt".to_string(),
            Ty::ULong => "ULong".to_string(),
            Ty::String => "String".to_string(),
            Ty::Unit => "Unit".to_string(),
            Ty::Obj(n, args) => {
                let base = if context
                    .iter()
                    .copied()
                    .any(|ty| ty.contains_distinct_classifier_with_segment(n))
                {
                    n.render().replace(['/', '$'], ".")
                } else {
                    n.segment_ref().replace('$', ".")
                };
                if args.is_empty() {
                    base
                } else {
                    let arguments = args
                        .iter()
                        .map(|argument| {
                            argument.source_name_with_type_parameter_in(context, type_parameter)
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{base}<{arguments}>")
                }
            }
            Ty::Null => "Null".to_string(),
            Ty::Nothing => "Nothing".to_string(),
            Ty::Error => "<error>".to_string(),
            Ty::Fun(signature) => {
                let parameters = signature
                    .params
                    .iter()
                    .map(|parameter| {
                        parameter.source_name_with_type_parameter_in(context, type_parameter)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let suspend = if signature.suspend { "suspend " } else { "" };
                format!(
                    "{suspend}({parameters}) -> {}",
                    signature
                        .ret
                        .source_name_with_type_parameter_in(context, type_parameter)
                )
            }
            Ty::Nullable(inner) => {
                let rendered = inner.source_name_with_type_parameter_in(context, type_parameter);
                if matches!(*inner, Ty::Fun(_)) {
                    format!("({rendered})?")
                } else {
                    format!("{rendered}?")
                }
            }
            Ty::PlatformNullable(inner) => {
                format!(
                    "{}!",
                    inner.source_name_with_type_parameter_in(context, type_parameter)
                )
            }
            Ty::InProjection(inner) => format!(
                "in {}",
                inner.source_name_with_type_parameter_in(context, type_parameter)
            ),
            Ty::OutProjection(inner) => format!(
                "out {}",
                inner.source_name_with_type_parameter_in(context, type_parameter)
            ),
            Ty::TyParam(n, _) => type_parameter(n),
            // Only reachable from a diagnostic rendered while the declaration is still being
            // resolved; it never names a real type.
            Ty::Pending => "<not determined>".to_string(),
        }
    }

    fn contains_distinct_classifier_with_segment(self, classifier: TypeName) -> bool {
        match self {
            Ty::Obj(name, arguments) => {
                (name != classifier && name.segment_ref() == classifier.segment_ref())
                    || arguments.iter().copied().any(|argument| {
                        argument.contains_distinct_classifier_with_segment(classifier)
                    })
            }
            Ty::Fun(signature) => {
                signature.params.iter().copied().any(|parameter| {
                    parameter.contains_distinct_classifier_with_segment(classifier)
                }) || signature
                    .ret
                    .contains_distinct_classifier_with_segment(classifier)
            }
            Ty::Nullable(inner)
            | Ty::PlatformNullable(inner)
            | Ty::InProjection(inner)
            | Ty::OutProjection(inner) => {
                inner.contains_distinct_classifier_with_segment(classifier)
            }
            _ => false,
        }
    }

    pub fn name(self) -> String {
        match self {
            Ty::Int => "Int".to_string(),
            Ty::Byte => "Byte".to_string(),
            Ty::Short => "Short".to_string(),
            Ty::Long => "Long".to_string(),
            Ty::Float => "Float".to_string(),
            Ty::Double => "Double".to_string(),
            Ty::Boolean => "Boolean".to_string(),
            Ty::Char => "Char".to_string(),
            Ty::UByte => "UByte".to_string(),
            Ty::UShort => "UShort".to_string(),
            Ty::UInt => "UInt".to_string(),
            Ty::ULong => "ULong".to_string(),
            Ty::String => "String".to_string(),
            Ty::Unit => "Unit".to_string(),
            Ty::Obj(name, _) => name.render(),
            Ty::Null => "Null".to_string(),
            Ty::Nothing => "Nothing".to_string(),
            Ty::Error => "<error>".to_string(),
            Ty::Pending => "<not determined>".to_string(),
            Ty::Fun(_) => "Function".to_string(),
            Ty::Nullable(inner) => format!("{}?", inner.name()),
            Ty::PlatformNullable(inner) => format!("{}!", inner.name()),
            Ty::InProjection(inner) => format!("in {}", inner.name()),
            Ty::OutProjection(inner) => format!("out {}", inner.name()),
            Ty::TyParam(name, _) => name.to_string(),
        }
    }

    /// Internal class name if this is an object type.
    pub fn obj_internal(self) -> Option<TypeName> {
        match self {
            Ty::Obj(n, _) => Some(n),
            // A type parameter follows its bound for object identity queries.
            Ty::TyParam(_, b) | Ty::PlatformNullable(b) => b.obj_internal(),
            _ => None,
        }
    }

    /// True for values that can carry `null` in the language model. Any nullable type is reference-like;
    /// a type parameter follows its bound.
    pub fn is_reference(self) -> bool {
        match self {
            scalar if scalar.scalar_value_repr().is_some() => false,
            Ty::TyParam(_, b) => b.is_reference(),
            // A flexible Java `T!` can be consumed as its non-null lower bound, but until that
            // commitment it also admits null and is represented by a reference — including a method
            // type variable specialized to a source primitive (`Boolean!`).
            Ty::PlatformNullable(_) => true,
            _ => matches!(self, Ty::Obj(..) | Ty::Null | Ty::Fun(_) | Ty::Nullable(_)),
        }
    }

    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            Ty::Int | Ty::Byte | Ty::Short | Ty::Long | Ty::Float | Ty::Double
        )
    }

    pub fn is_numeric_or_char(self) -> bool {
        self.is_numeric() || self == Ty::Char
    }

    /// True for a member/property read result that can be used as an expression value in the current
    /// lowering model. `Unit`/`Error` entries are ignored when resolving zero-arg property-like reads.
    pub fn is_read_value_result(self) -> bool {
        !matches!(self, Ty::Unit | Ty::Error)
    }

    /// Whether an otherwise-structured type contains an unresolved component.
    ///
    /// Resolution deliberately preserves generic/function structure around [`Ty::Error`] so later
    /// diagnostics can name the failing nested `TypeRef`: `List<Missing>` is an `Obj` whose argument
    /// is `Error`, and `(Missing) -> String` is a `Fun` whose parameter is `Error`. Consumers that
    /// check only `ty == Ty::Error` silently miss both shapes. Keep the recursion here as the single
    /// semantic predicate instead of teaching individual expression forms about every `Ty` variant.
    pub fn contains_error(self) -> bool {
        match self {
            Ty::Error => true,
            Ty::Obj(_, arguments) => arguments.iter().copied().any(Ty::contains_error),
            Ty::Fun(signature) => {
                signature.params.iter().copied().any(Ty::contains_error)
                    || signature.ret.contains_error()
            }
            Ty::Nullable(inner) => inner.contains_error(),
            _ => false,
        }
    }

    /// True for the signed integral types whose Kotlin range overload yields `IntRange`.
    pub fn is_int_range_operand(self) -> bool {
        matches!(self.range_operand_bound(), Ty::Byte | Ty::Short | Ty::Int)
    }

    /// Classifier that participates in built-in range overload selection. A type parameter uses its
    /// declared upper bound: `<T : Char>` has the same `rangeTo`/`contains` surface as `Char`, while
    /// the expression itself remains `T` everywhere else in the type system.
    pub fn range_operand_bound(self) -> Ty {
        match self {
            Ty::TyParam(_, bound) => bound.range_operand_bound(),
            other => other,
        }
    }

    /// Loop counter type for a same-typed Kotlin range bound, if krusty can lower it as counted.
    pub fn range_counter_type(self) -> Option<Ty> {
        let operand = self.range_operand_bound();
        Some(match operand {
            Ty::Byte | Ty::Short => Ty::Int,
            Ty::UByte | Ty::UShort => Ty::UInt,
            Ty::Int | Ty::Long | Ty::UInt | Ty::ULong | Ty::Char => operand,
            _ => return None,
        })
    }

    /// Kotlin range value type for `lo..hi`/`lo..<hi`, if the operand pair is supported.
    pub fn range_value_type(lo: Ty, hi: Ty) -> Option<Ty> {
        let lo = lo.range_operand_bound();
        let hi = hi.range_operand_bound();
        Some(match (lo, hi) {
            (Ty::Char, Ty::Char) => Ty::obj("kotlin/ranges/CharRange"),
            (Ty::UInt, Ty::UInt) => Ty::obj("kotlin/ranges/UIntRange"),
            (Ty::ULong, Ty::ULong) => Ty::obj("kotlin/ranges/ULongRange"),
            (Ty::Double, Ty::Double) | (Ty::Float, Ty::Float) => {
                Ty::obj("kotlin/ranges/ClosedFloatingPointRange")
            }
            (l, r) if l.is_int_range_operand() && r.is_int_range_operand() => {
                Ty::obj("kotlin/ranges/IntRange")
            }
            (l, r)
                if (l.is_int_range_operand() || l == Ty::Long)
                    && (r.is_int_range_operand() || r == Ty::Long) =>
            {
                Ty::obj("kotlin/ranges/LongRange")
            }
            _ => return None,
        })
    }

    /// Scalar type used while evaluating Kotlin operations that widen small integral values to `Int`.
    pub fn int_arithmetic_repr(self) -> Ty {
        match self {
            Ty::Byte | Ty::Short | Ty::Char => Ty::Int,
            t => t,
        }
    }

    /// Whether a numeric `actual` can be assigned to this numeric target in source checking.
    pub fn accepts_numeric(self, actual: Ty) -> bool {
        match self {
            Ty::Byte | Ty::Short => matches!(actual, Ty::Int | Ty::Byte | Ty::Short),
            Ty::Long => matches!(actual, Ty::Int | Ty::Byte | Ty::Short | Ty::Char),
            Ty::Float | Ty::Double => matches!(
                actual,
                Ty::Int | Ty::Long | Ty::Byte | Ty::Short | Ty::Char | Ty::Float
            ),
            _ => false,
        }
    }

    /// True for the unsigned integer types (inline classes over a signed primitive).
    pub fn is_unsigned(self) -> bool {
        matches!(self, Ty::UByte | Ty::UShort | Ty::UInt | Ty::ULong)
    }

    /// The zero-extension mask an unsigned value needs when it leaves its own representation, or
    /// `None` when the representation already spans the whole operation width (`UInt` = `int`,
    /// `ULong` = `long`). `UByte`/`UShort` live in a `byte`/`short`, which the JVM SIGN-extends on
    /// every load, so `UByte.toInt()` is `iload; sipush 255; iand` — exactly what kotlinc emits.
    pub fn unsigned_widen_mask(self) -> Option<i32> {
        match self {
            Ty::UByte => Some(0xFF),
            Ty::UShort => Some(0xFFFF),
            _ => None,
        }
    }

    /// The unsigned type an operator on `self` actually computes in. Kotlin gives `UByte`/`UShort` no
    /// arithmetic of their own: each operator is defined as `toInt()` (zero-extend) followed by the
    /// `UInt` operator, so both promote to `UInt` and an operation on them yields `UInt`. `UInt`/`ULong`
    /// operate in themselves. `None` for a signed type.
    pub fn unsigned_op_type(self) -> Option<Ty> {
        match self {
            Ty::UByte | Ty::UShort | Ty::UInt => Some(Ty::UInt),
            Ty::ULong => Some(Ty::ULong),
            _ => None,
        }
    }

    /// True for Kotlin scalar values that the JVM backend carries in primitive slots.
    pub fn is_jvm_scalar(self) -> bool {
        self.scalar_value_repr().is_some()
    }

    /// The primitive representation used for built-in scalar values.
    pub fn scalar_value_repr(self) -> Option<Ty> {
        Some(match self {
            Ty::Int
            | Ty::Byte
            | Ty::Short
            | Ty::Long
            | Ty::Float
            | Ty::Double
            | Ty::Boolean
            | Ty::Char => self,
            Ty::UByte => Ty::Byte,
            Ty::UShort => Ty::Short,
            Ty::UInt => Ty::Int,
            Ty::ULong => Ty::Long,
            Ty::TyParam(_, bound) => return bound.scalar_value_repr(),
            _ => return None,
        })
    }

    /// A primitive whose generic upper bound (`fun <T: Int>`) specializes a function type parameter
    /// to its scalar representation, matching kotlinc's callable descriptor.
    pub fn is_specializable_bound(self) -> bool {
        matches!(
            self,
            Ty::Int
                | Ty::Byte
                | Ty::Short
                | Ty::Long
                | Ty::Float
                | Ty::Double
                | Ty::Boolean
                | Ty::Char
        )
    }

    /// Numeric promotion rank for binary arithmetic (Int < Long < Double).
    fn rank(self) -> u8 {
        match self {
            // Byte/Short share Int's rank: Kotlin arithmetic on them produces `Int`.
            Ty::Byte | Ty::Short | Ty::Int => 1,
            Ty::Long => 2,
            Ty::Float => 3,
            Ty::Double => 4,
            _ => 0,
        }
    }

    /// Result type of numeric promotion, or `None` if either side isn't numeric. `Byte`/`Short`
    /// promote to `Int` (Kotlin has no byte/short arithmetic — operands widen to `Int`).
    pub fn promote(a: Ty, b: Ty) -> Option<Ty> {
        if a.is_numeric() && b.is_numeric() {
            let r = if a.rank() >= b.rank() { a } else { b };
            Some(r.int_arithmetic_repr())
        } else {
            None
        }
    }
}

/// The source spelling carried by a declaration-scoped semantic type-parameter key. Diagnostics and
/// metadata show the written name even though inference uses the full identity.
pub(crate) fn type_parameter_source_name(name: &str) -> &str {
    TYPE_PARAMETER_SOURCES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .get(name)
        .copied()
        .unwrap_or(name)
}

/// Semantic type of one declared value-parameter slot. A `vararg element: T` is callable as an
/// `Array<T>` slot even though the source annotation itself denotes the element type.
pub(crate) fn semantic_value_parameter_ty(declared: Ty, is_vararg: bool) -> Ty {
    if is_vararg {
        Ty::array(declared)
    } else {
        declared
    }
}

#[cfg(test)]
mod declaration_type_parameter_tests {
    use super::{declaration_type_parameter, type_parameter_source_name};

    #[test]
    fn source_spelling_is_not_parsed_from_the_semantic_identity() {
        let semantic = declaration_type_parameter(11, 7, 19, 0, "T$nested");
        assert_eq!(type_parameter_source_name(semantic), "T$nested");
        assert_ne!(semantic, "T$nested");
    }
}

/// Whether `ty` mentions any of the named type parameters (`T` itself, `List<T>`, `(T) -> T`,
/// `T?`). A pure `Ty` predicate shared by the checker (generic-shape classification) and lowering
/// (generic-shape gates).
pub(crate) fn ty_mentions_param(ty: Ty, names: &[String]) -> bool {
    match ty {
        Ty::TyParam(name, _) => names.iter().any(|parameter| parameter == name),
        Ty::Obj(_, arguments) => arguments
            .iter()
            .any(|argument| ty_mentions_param(*argument, names)),
        Ty::Fun(signature) => {
            signature
                .params
                .iter()
                .any(|parameter| ty_mentions_param(*parameter, names))
                || ty_mentions_param(signature.ret, names)
        }
        Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner) => ty_mentions_param(*inner, names),
        _ => false,
    }
}

/// Whether `ty` mentions ANY type parameter, whatever its identity. Semantic type-parameter names
/// are checker-generated (`\0tp:…`), so a source-name list cannot match them — use this where the
/// only question is "does a type variable appear at all".
pub(crate) fn ty_mentions_any_param(ty: Ty) -> bool {
    match ty {
        Ty::TyParam(..) => true,
        Ty::Obj(_, arguments) => arguments
            .iter()
            .any(|argument| ty_mentions_any_param(*argument)),
        Ty::Fun(signature) => {
            signature
                .params
                .iter()
                .any(|parameter| ty_mentions_any_param(*parameter))
                || ty_mentions_any_param(signature.ret)
        }
        Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner) => ty_mentions_any_param(*inner),
        _ => false,
    }
}

/// Kotlin declaration visibility — the modifier on a `fun`/`val`/`class` (from source) or the
/// `@Metadata`/bytecode flags of a library declaration. `PRIVATE_TO_THIS` folds into `Private`;
/// `LOCAL` is not represented (locals are never surfaced as declarations). This records what a
/// declaration IS; whether a given call site may access it (`protected`/`internal`/`private`) is a
/// separate context-dependent decision made during resolution.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Visibility {
    #[default]
    Public,
    Internal,
    Protected,
    Private,
    /// A Java class-file declaration with no access modifier.
    PackagePrivate,
}

/// Kotlin annotation retention after frontend resolution. `Default` is runtime retention without
/// an explicit `@Retention` declaration; keeping it distinct lets metadata emission omit the Kotlin
/// meta-annotation while still stamping the JVM runtime policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnotationRetention {
    Default,
    Runtime,
    Binary,
    Source,
}

/// One frontend-checked annotation argument. Every classifier identity is resolved before this
/// crosses into common lowering; backends only choose the physical encoding of the recorded value.
#[derive(Clone, Debug, PartialEq)]
pub enum AnnotationValue {
    Int(i32),
    /// A `byte`/`short` element. The JVM writes these with their own element tags (`B`, `S`), so a
    /// narrower element cannot borrow `Int`: reading such an annotation back throws
    /// `AnnotationTypeMismatchException`.
    Byte(i8),
    Short(i16),
    Long(i64),
    Float(f32),
    Double(f64),
    Boolean(bool),
    Char(u16),
    String(crate::kt_string::KtString),
    Enum(TypeName, String),
    Class(TypeName),
    Annotation {
        internal: TypeName,
        values: Vec<(String, AnnotationValue)>,
    },
    Array(Vec<AnnotationValue>),
}

/// A resolved annotation application, including its declaration-ordered element values and
/// semantic retention. This is a frontend decision; common lowering must not reopen source or a
/// symbol provider to reconstruct it.
#[derive(Clone, Debug, PartialEq)]
pub struct AppliedAnnotation {
    pub internal: TypeName,
    pub values: Vec<(String, AnnotationValue)>,
    pub retention: AnnotationRetention,
    /// The annotation's DECLARED `@Target` set, as far as it decides where an application written
    /// without a use-site prefix lands (see [`AnnotationTargets`]).
    pub targets: AnnotationTargets,
}

/// The subset of an annotation's declared `@Target` set that decides the USE-SITE of an application
/// written on a property declaration. Kotlin picks the first applicable of
/// `param` → `property` → `field`, and the three land in three different places in the class file,
/// so this is a semantic fact, never a guess at emission time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnnotationTargets {
    pub value_parameter: bool,
    pub property: bool,
    pub field: bool,
}

/// Where an annotation written on a property declaration with no use-site prefix belongs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyAnnotationSite {
    /// A primary-constructor `val`/`var` parameter's own annotation — `RuntimeVisible…
    /// ParameterAnnotations` on the constructor.
    ValueParameter,
    /// The Kotlin PROPERTY, which has no class-file declaration of its own: the annotation goes on a
    /// synthetic `get<Name>$annotations()` marker method.
    Property,
    /// The backing field.
    Field,
}

impl AnnotationTargets {
    /// An annotation class that declares no `@Target` is applicable everywhere.
    pub const DEFAULT: Self = Self {
        value_parameter: true,
        property: true,
        field: true,
    };

    /// Kotlin's use-site default for an annotation written on a property declaration: the first
    /// applicable of `param` (a primary-constructor property parameter only) → `property` → `field`.
    /// `None` when the annotation targets none of the three (its application is a frontend error).
    pub fn property_declaration_site(
        self,
        on_constructor_parameter: bool,
    ) -> Option<PropertyAnnotationSite> {
        if on_constructor_parameter && self.value_parameter {
            return Some(PropertyAnnotationSite::ValueParameter);
        }
        if self.property {
            return Some(PropertyAnnotationSite::Property);
        }
        self.field.then_some(PropertyAnnotationSite::Field)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TypeVariance {
    #[default]
    Invariant,
    In,
    Out,
}

#[derive(Clone, Debug, Default)]
pub struct TypeParameterView<B> {
    pub(crate) type_params: Vec<String>,
    pub(crate) type_param_bounds: B,
    pub(crate) type_param_variances: Vec<TypeVariance>,
}

#[derive(Clone, Debug, Default)]
pub struct TypeParameters<B>(TypeParameterView<B>);

impl<B> std::ops::Deref for TypeParameters<B> {
    type Target = TypeParameterView<B>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub trait TypeParameterBounds {
    fn valid_for(&self, names: &[String]) -> bool;
}

impl<T> TypeParameterBounds for Vec<Vec<T>> {
    fn valid_for(&self, names: &[String]) -> bool {
        self.len() == names.len()
    }
}

impl TypeParameterBounds for Vec<Ty> {
    fn valid_for(&self, names: &[String]) -> bool {
        self.len() == names.len()
    }
}

impl<T> TypeParameterBounds for Vec<(String, T)> {
    fn valid_for(&self, _names: &[String]) -> bool {
        true
    }
}

impl<B: TypeParameterBounds> TypeParameters<B> {
    pub fn new(
        type_params: Vec<String>,
        type_param_bounds: B,
        type_param_variances: Vec<TypeVariance>,
    ) -> Self {
        assert_eq!(type_params.len(), type_param_variances.len());
        assert!(type_param_bounds.valid_for(&type_params));
        Self(TypeParameterView {
            type_params,
            type_param_bounds,
            type_param_variances,
        })
    }

    pub fn type_params(&self) -> &Vec<String> {
        &self.0.type_params
    }

    pub fn type_param_bounds(&self) -> &B {
        &self.0.type_param_bounds
    }

    pub fn type_param_variances(&self) -> &Vec<TypeVariance> {
        &self.0.type_param_variances
    }

    pub fn replace(&mut self, names: Vec<String>, bounds: B, variances: Vec<TypeVariance>) {
        *self = Self::new(names, bounds, variances);
    }

    pub fn map_bounds(&mut self, map: impl FnOnce(&mut B))
    where
        B: Clone,
    {
        let mut bounds = self.0.type_param_bounds.clone();
        map(&mut bounds);
        self.replace(
            self.0.type_params.clone(),
            bounds,
            self.0.type_param_variances.clone(),
        );
    }
}

impl<B> TypeParameters<B>
where
    B: Default + TypeParameterBounds,
{
    pub fn invariant(type_params: Vec<String>, type_param_bounds: B) -> Self {
        let type_param_variances = vec![TypeVariance::Invariant; type_params.len()];
        Self::new(type_params, type_param_bounds, type_param_variances)
    }
}

impl Visibility {
    /// The kotlin-metadata `Flags.VISIBILITY` enum value → `Visibility`. Order:
    /// INTERNAL=0, PRIVATE=1, PROTECTED=2, PUBLIC=3, PRIVATE_TO_THIS=4, LOCAL=5. Unknown/`LOCAL`
    /// conservatively map to `Private` (never wrongly widens access).
    pub fn from_metadata(v: u64) -> Visibility {
        match v {
            0 => Visibility::Internal,
            2 => Visibility::Protected,
            3 => Visibility::Public,
            _ => Visibility::Private,
        }
    }

    /// The source visibility modifier keyword → `Visibility`; no/unknown modifier is `public`
    /// (Kotlin's default). `PRIVATE_TO_THIS` is not a source keyword.
    pub fn from_modifier(m: &str) -> Visibility {
        match m {
            "private" => Visibility::Private,
            "protected" => Visibility::Protected,
            "internal" => Visibility::Internal,
            _ => Visibility::Public,
        }
    }

    /// Coarse map from a legacy `is_public` bool, for synthetic/top-level callables that never carry a
    /// finer visibility (a top-level or extension can be `public`/`internal`/`private` but NEVER
    /// `protected`, so no protected information is lost here). `internal` top-levels still read back as
    /// `Private` until the finer decode reaches those arms — a deliberate interim under-approximation.
    pub fn from_public(is_public: bool) -> Visibility {
        if is_public {
            Visibility::Public
        } else {
            Visibility::Private
        }
    }

    /// Whether this is the `public` visibility — the exact predicate the pre-context resolver used
    /// (`is_public`). Kept so the current public-only filter is expressible verbatim while the
    /// context-aware `accessible(...)` gate is introduced separately.
    pub fn is_public(self) -> bool {
        self == Visibility::Public
    }

    /// Whether this is `private` — the source `is_private` bool the parser/AST previously carried.
    pub fn is_private(self) -> bool {
        self == Visibility::Private
    }
}

/// The metadata-declared reflection function interface. Callable-reference parameter/return shape is
/// retained independently as `Ty::Fun`; it is never reconstructed from a classifier spelling.
pub const KFUNCTION_INTERNAL: &str = "kotlin/reflect/KFunction";

/// Substitute semantic type parameters throughout one type shape. This belongs to the type model:
/// providers, overload selection, checking, and lowering all consume the same transformation.
fn substitute_type_parameters(
    ty: Ty,
    bindings: &std::collections::HashMap<String, Ty>,
    preserve_unbound: bool,
) -> Ty {
    match ty {
        Ty::TyParam(name, bound) => bindings
            .get(name)
            .copied()
            .map(|binding| {
                if bound.upper_bound_admits_null() {
                    binding
                } else {
                    binding.non_null()
                }
            })
            .unwrap_or_else(|| {
                if preserve_unbound {
                    ty
                } else {
                    bound.non_null()
                }
            }),
        Ty::Fun(signature) => Ty::fun_with_shape(
            signature
                .params
                .iter()
                .map(|parameter| substitute_type_parameters(*parameter, bindings, preserve_unbound))
                .collect(),
            substitute_type_parameters(signature.ret, bindings, preserve_unbound),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ),
        Ty::Nullable(inner) => Ty::nullable(substitute_type_parameters(
            *inner,
            bindings,
            preserve_unbound,
        )),
        Ty::PlatformNullable(inner) => Ty::platform_nullable(substitute_type_parameters(
            *inner,
            bindings,
            preserve_unbound,
        )),
        Ty::InProjection(inner) => Ty::in_projection(substitute_type_parameters(
            *inner,
            bindings,
            preserve_unbound,
        )),
        Ty::OutProjection(inner) => Ty::out_projection(substitute_type_parameters(
            *inner,
            bindings,
            preserve_unbound,
        )),
        Ty::Obj(name, arguments) if !arguments.is_empty() => Ty::obj_args_name(
            name,
            &arguments
                .iter()
                .map(|argument| substitute_type_parameters(*argument, bindings, preserve_unbound))
                .collect::<Vec<_>>(),
        ),
        _ => ty,
    }
}

pub(crate) fn ty_subst(ty: Ty, bindings: &std::collections::HashMap<String, Ty>) -> Ty {
    substitute_type_parameters(ty, bindings, false)
}

pub(crate) fn ty_subst_all(
    types: &[Ty],
    bindings: &std::collections::HashMap<String, Ty>,
) -> Vec<Ty> {
    types.iter().map(|ty| ty_subst(*ty, bindings)).collect()
}

/// Replace the inline bounds carried by type-parameter references throughout a type shape. Signature
/// decoders first discover formal declarations and type uses independently; this single type-model
/// operation joins them without each decoder growing its own recursive walk.
pub(crate) fn ty_with_param_bounds(ty: Ty, bounds: &std::collections::HashMap<String, Ty>) -> Ty {
    match ty {
        Ty::TyParam(name, current) => {
            Ty::ty_param(name, bounds.get(name).copied().unwrap_or(*current))
        }
        Ty::Fun(signature) => Ty::fun_with_shape(
            signature
                .params
                .iter()
                .map(|parameter| ty_with_param_bounds(*parameter, bounds))
                .collect(),
            ty_with_param_bounds(signature.ret, bounds),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ),
        Ty::Nullable(inner) => Ty::nullable(ty_with_param_bounds(*inner, bounds)),
        Ty::PlatformNullable(inner) => Ty::platform_nullable(ty_with_param_bounds(*inner, bounds)),
        Ty::InProjection(inner) => Ty::in_projection(ty_with_param_bounds(*inner, bounds)),
        Ty::OutProjection(inner) => Ty::out_projection(ty_with_param_bounds(*inner, bounds)),
        Ty::Obj(name, arguments) if !arguments.is_empty() => Ty::obj_args_name(
            name,
            &arguments
                .iter()
                .map(|argument| ty_with_param_bounds(*argument, bounds))
                .collect::<Vec<_>>(),
        ),
        _ => ty,
    }
}

/// Replace declaration-local type-parameter identities throughout a semantic type, including the
/// inline upper bounds carried by nested `TyParam` nodes. Renaming only the outer occurrence leaves
/// chains such as `D : B, B : A` partly keyed by source spelling and breaks bound member lookup.
pub(crate) fn ty_rename_params(
    ty: Ty,
    identities: &std::collections::HashMap<&str, &'static str>,
) -> Ty {
    match ty {
        Ty::TyParam(name, bound) => Ty::ty_param(
            identities.get(name).copied().unwrap_or(name),
            ty_rename_params(*bound, identities),
        ),
        Ty::Fun(signature) => Ty::fun_with_shape(
            signature
                .params
                .iter()
                .map(|parameter| ty_rename_params(*parameter, identities))
                .collect(),
            ty_rename_params(signature.ret, identities),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ),
        Ty::Nullable(inner) => Ty::nullable(ty_rename_params(*inner, identities)),
        Ty::PlatformNullable(inner) => Ty::platform_nullable(ty_rename_params(*inner, identities)),
        Ty::InProjection(inner) => Ty::in_projection(ty_rename_params(*inner, identities)),
        Ty::OutProjection(inner) => Ty::out_projection(ty_rename_params(*inner, identities)),
        Ty::Obj(name, arguments) if !arguments.is_empty() => Ty::obj_args_name(
            name,
            &arguments
                .iter()
                .map(|argument| ty_rename_params(*argument, identities))
                .collect::<Vec<_>>(),
        ),
        _ => ty,
    }
}

pub(crate) fn ty_subst_keep_unbound(
    ty: Ty,
    bindings: &std::collections::HashMap<String, Ty>,
) -> Ty {
    substitute_type_parameters(ty, bindings, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_read_element_approximates_use_site_projections() {
        let platform_string = Ty::platform_nullable(Ty::String);
        assert_eq!(
            Ty::obj_args("kotlin/Array", &[Ty::out_projection(platform_string)]).array_read_elem(),
            Some(platform_string)
        );
        assert_eq!(
            Ty::obj_args("kotlin/Array", &[Ty::in_projection(Ty::String)]).array_read_elem(),
            Some(Ty::nullable(Ty::obj("kotlin/Any")))
        );
        assert_eq!(Ty::array(Ty::Int).array_read_elem(), Some(Ty::Int));
        assert_eq!(Ty::array(Ty::String).array_read_elem(), Some(Ty::String));
    }

    #[test]
    fn type_name_tree_operations_preserve_identity_without_text_round_trip() {
        let names = NameTree::default();
        let int = type_name_from(&names, names.insert("kotlin/Int"));
        assert_eq!(int, Ty::Int.kotlin_class_internal().unwrap());

        let dotted = type_name("kotlin/collections/Map.Entry");
        let dollar = type_name("kotlin/collections/Map$Entry");
        assert_eq!(dotted, dollar);
        assert_eq!(
            existing_type_name("kotlin/collections/Map.Entry"),
            Some(dollar)
        );
        assert_eq!(
            type_name_from(&names, names.insert("kotlin/collections/Map.Entry")),
            dollar
        );
        let copied = insert_type_name_in(&names, dollar);
        assert_eq!(type_name_from(&names, copied), dollar);

        let nested = type_name_child(type_name("demo"), "Outer/Inner");
        assert!(nested.matches("demo/Outer/Inner"));
        assert!(nested
            .parent()
            .is_some_and(|parent| parent.matches("demo/Outer")));
    }

    #[test]
    fn builtin_aliases_are_the_ordinary_classifier_types() {
        assert_eq!(Ty::Int, Ty::obj("kotlin/Int"));
        assert_eq!(Ty::String, Ty::obj("kotlin/String"));
    }

    #[test]
    fn textual_name_comparison_does_not_intern_a_miss() {
        let missing = "__type_name_comparison_must_not_intern__/Missing";
        assert_eq!(existing_type_name(missing), None);
        assert!(!type_name("kotlin/String").matches(missing));
        assert_eq!(existing_type_name(missing), None);
    }

    #[test]
    fn ty_param_carries_name_and_bound() {
        let t = Ty::ty_param("T", Ty::obj("kotlin/CharSequence"));
        assert!(t.is_ty_param());
        assert_eq!(t.ty_param_name(), Some("T"));
        assert_eq!(t.ty_param_bound(), Some(Ty::obj("kotlin/CharSequence")));
    }

    #[test]
    fn ty_param_is_reference_follows_its_bound() {
        assert!(Ty::ty_param("T", Ty::obj("kotlin/Any")).is_reference());
        // A primitive-bounded `<T : Int>` is not a reference (it specializes to the primitive).
        assert!(!Ty::ty_param("T", Ty::Int).is_reference());
    }

    #[test]
    fn non_ty_param_reports_none() {
        assert!(!Ty::Int.is_ty_param());
        assert_eq!(Ty::Int.ty_param_name(), None);
        assert_eq!(Ty::Int.ty_param_bound(), None);
    }

    #[test]
    fn kotlin_class_internal_is_source_class_identity() {
        assert!(Ty::Int
            .kotlin_class_internal()
            .is_some_and(|n| n.matches("kotlin/Int")));
        assert!(Ty::String
            .kotlin_class_internal()
            .is_some_and(|n| n.matches("kotlin/String")));
        assert!(Ty::obj_args("demo/Box", &[Ty::Int])
            .kotlin_class_internal()
            .is_some_and(|n| n.matches("demo/Box")));
        assert!(Ty::nullable(Ty::UInt)
            .kotlin_class_internal()
            .is_some_and(|n| n.matches("kotlin/UInt")));
        assert_eq!(
            Ty::ty_param("T", Ty::obj("kotlin/CharSequence")).kotlin_class_internal(),
            Some(type_name("kotlin/CharSequence"))
        );
        assert_eq!(Ty::Null.kotlin_class_internal(), None);
    }

    #[test]
    fn erased_recv_folds_nullability_generics_and_type_params() {
        // Nullability: `String?` and `String` resolve the same extensions.
        assert_eq!(Ty::nullable(Ty::String).erased_recv(), Ty::String);
        // Generic arguments are dropped (instantiation-independent).
        assert_eq!(
            Ty::obj_args("kotlin/collections/List", &[Ty::Int]).erased_recv(),
            Ty::obj_args("kotlin/collections/List", &[Ty::String]).erased_recv()
        );
        assert_eq!(
            Ty::obj_args("kotlin/collections/List", &[Ty::Int]).erased_recv(),
            Ty::obj("kotlin/collections/List")
        );
        // A type parameter keys under its (also-erased) bound.
        assert_eq!(
            Ty::ty_param("T", Ty::obj_args("kotlin/collections/List", &[Ty::Int])).erased_recv(),
            Ty::obj("kotlin/collections/List")
        );
        // Array element generics erase too, but the array-ness is kept.
        assert_eq!(
            Ty::array(Ty::obj_args("kotlin/collections/List", &[Ty::Int])).erased_recv(),
            Ty::array(Ty::obj("kotlin/collections/List"))
        );
        // Reference vs primitive and signed vs unsigned stay distinct.
        assert_ne!(Ty::Int.erased_recv(), Ty::UInt.erased_recv());
        assert_eq!(Ty::Int.erased_recv(), Ty::Int);
        // A nullable scalar stays distinct without inventing a wrapper classifier in core.
        assert_ne!(Ty::nullable(Ty::Int).erased_recv(), Ty::Int.erased_recv());
        assert_eq!(Ty::nullable(Ty::Int).erased_recv(), Ty::nullable(Ty::Int));
        assert_eq!(Ty::nullable(Ty::UInt).erased_recv(), Ty::nullable(Ty::UInt));
        // A nullable reference shares the non-null key.
        assert_eq!(Ty::nullable(Ty::String).erased_recv(), Ty::String);
        // `null`/`Nothing`/error all key under `Any` (a `null` receiver reaches an `Any?` extension).
        assert_eq!(Ty::Null.erased_recv(), Ty::obj("kotlin/Any"));
        assert_eq!(Ty::Nothing.erased_recv(), Ty::obj("kotlin/Any"));
        assert_eq!(
            Ty::nullable(Ty::obj("kotlin/Any")).erased_recv(),
            Ty::obj("kotlin/Any")
        );
    }

    #[test]
    fn extension_receiver_key_retains_nullability_and_generic_identity() {
        assert_ne!(
            Ty::nullable(Ty::String).extension_recv_key(),
            Ty::String.extension_recv_key()
        );
        assert_eq!(
            Ty::obj_args("kotlin/collections/List", &[Ty::Int]).extension_recv_key(),
            Ty::obj_args("kotlin/collections/List", &[Ty::String]).extension_recv_key()
        );
        assert!(Ty::ty_param("T", Ty::obj("kotlin/Any"))
            .extension_recv_key()
            .is_ty_param());
        assert_eq!(
            Ty::ty_param("T", Ty::obj("kotlin/Any")).extension_recv_key(),
            Ty::ty_param("R", Ty::obj("kotlin/Any")).extension_recv_key()
        );
        let array_key = Ty::obj_args(
            "kotlin/Array",
            &[Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")))],
        )
        .extension_recv_key();
        assert!(array_key
            .type_args()
            .first()
            .is_some_and(|element| element.is_ty_param()));
    }

    #[test]
    fn nullable_wraps_inner_and_reports_nullable() {
        let t = Ty::nullable(Ty::Int);
        assert!(t.is_nullable());
        assert_eq!(t.non_null(), Ty::Int);
    }

    #[test]
    fn platform_nullability_has_flexible_source_shape_and_erased_identity() {
        let platform = Ty::platform_nullable(Ty::String);
        assert_eq!(platform.source_name(), "String!");
        assert!(!platform.is_nullable());
        assert_eq!(platform.platform_lower_bound(), Ty::String);
        assert_eq!(
            platform.kotlin_class_internal(),
            Ty::String.kotlin_class_internal()
        );
        assert_eq!(Ty::nullable(platform), Ty::nullable(Ty::String));
    }

    #[test]
    fn contains_error_descends_through_semantic_type_shapes() {
        // Generic and function resolvers intentionally retain their outer type around an erroneous
        // nested reference. The shared predicate must see both without declaring resolved bottom or
        // scalar types erroneous.
        assert!(Ty::obj_args("demo/Box", &[Ty::Error]).contains_error());
        assert!(Ty::fun(vec![Ty::Error], Ty::String).contains_error());
        assert!(Ty::fun(vec![Ty::String], Ty::Error).contains_error());
        assert!(!Ty::obj_args("demo/Box", &[Ty::Nothing]).contains_error());
        assert!(!Ty::String.contains_error());
    }

    #[test]
    fn non_null_type_is_not_nullable() {
        assert!(!Ty::Int.is_nullable());
        assert_eq!(Ty::Int.non_null(), Ty::Int);
    }

    #[test]
    fn nullable_is_idempotent_no_double_wrap() {
        // Kotlin has no `T??`: wrapping an already-nullable type is a no-op.
        let once = Ty::nullable(Ty::obj("demo/Point"));
        assert_eq!(Ty::nullable(once), once);
    }

    #[test]
    fn nullable_primitive_is_a_reference_so_null_is_valid() {
        // `Int?` boxes — it accepts `null`, unlike the unboxed `Int`.
        assert!(!Ty::Int.is_reference());
        assert!(Ty::nullable(Ty::Int).is_reference());
    }

    #[test]
    fn nullable_idempotent_over_a_primitive() {
        let once = Ty::nullable(Ty::Int);
        assert_eq!(Ty::nullable(once), once);
    }

    #[test]
    fn nullable_of_null_or_error_collapses() {
        // `Null?`/`Error?` are degenerate — wrapping them is meaningless.
        assert_eq!(Ty::nullable(Ty::Null), Ty::Null);
        assert_eq!(Ty::nullable(Ty::Error), Ty::Error);
    }

    #[test]
    fn nullable_of_nothing_is_a_real_distinct_type() {
        // Kotlin's `Nothing?` is the type of the `null` literal — a real nullable type, kept.
        assert!(Ty::nullable(Ty::Nothing).is_nullable());
        assert_eq!(Ty::nullable(Ty::Nothing).non_null(), Ty::Nothing);
    }

    #[test]
    fn nullable_primitive_recovers_the_unboxed_primitive() {
        assert_eq!(Ty::nullable(Ty::Int).nullable_primitive(), Some(Ty::Int));
        // Not a nullable primitive → None.
        assert_eq!(Ty::Int.nullable_primitive(), None);
        assert_eq!(Ty::nullable(Ty::String).nullable_primitive(), None);
        assert_eq!(Ty::obj("demo/Point").nullable_primitive(), None);
    }

    #[test]
    fn nullable_boxed_is_the_supported_nullable_form_of_a_primitive() {
        assert_eq!(Ty::Int.nullable_boxed(), Some(Ty::nullable(Ty::Int)));
        assert_eq!(Ty::Char.nullable_boxed(), Some(Ty::nullable(Ty::Char)));
        // Unsigned boxes to its inline-class wrapper (`UInt?` → boxed `kotlin/UInt`).
        assert_eq!(Ty::UInt.nullable_boxed(), Some(Ty::nullable(Ty::UInt)));
        assert_eq!(Ty::ULong.nullable_boxed(), Some(Ty::nullable(Ty::ULong)));
        // Already a reference → not a primitive to box.
        assert_eq!(Ty::String.nullable_boxed(), None);
    }

    #[test]
    fn nullable_non_ref_keeps_source_forms() {
        assert_eq!(Ty::Unit.nullable_non_ref(), Some(Ty::nullable(Ty::Unit)));
        assert_eq!(
            Ty::Nothing.nullable_non_ref(),
            Some(Ty::nullable(Ty::Nothing))
        );
        assert_eq!(Ty::Int.nullable_non_ref(), Some(Ty::nullable(Ty::Int)));
        assert_eq!(Ty::String.nullable_non_ref(), None);
    }

    #[test]
    fn source_name_renders_simple_generic_nullable_types() {
        let ty = Ty::nullable(Ty::obj_args(
            "demo/Outer$Holder",
            &[
                Ty::String,
                Ty::obj_args("kotlin/collections/List", &[Ty::Int]),
            ],
        ));
        assert_eq!(ty.source_name(), "Outer.Holder<String, List<Int>>?");
    }

    #[test]
    fn diagnostic_source_names_qualify_colliding_classifiers() {
        let actual = Ty::nullable(Ty::obj("right/Box"));
        let expected = Ty::obj("left/Box");
        let context = [actual, expected];

        assert_eq!(
            actual.source_name_with_type_parameter_in(&context, &|name: &str| {
                type_parameter_source_name(name).to_string()
            }),
            "right.Box?"
        );
        assert_eq!(
            expected.source_name_with_type_parameter_in(&context, &|name: &str| {
                type_parameter_source_name(name).to_string()
            }),
            "left.Box"
        );
    }
}

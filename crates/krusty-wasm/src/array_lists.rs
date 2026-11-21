//! `kotlin.collections.ArrayList`, laid out by the runtime.
//!
//! The target stdlib's `ArrayList` is a generic class whose body allocates its storage, its
//! iterator and the collections it inherits. This backend does not run that body. The runtime
//! lays the list out instead: an identity object holding a growable array of references and the
//! count in use. Its iterator is another identity object. Interface calls of `hasNext` and `next`
//! reach that iterator by its struct type, and a call through a collection interface's member
//! reaches the list's class (`super::builtin_members`).

use super::encode::{BlockType, Code, CompositeType, FieldType, Function, HeapType, Module};
use super::encode::{StorageType, SubType, ValType};
use super::identity::{IdentityObjects, PREFIX_FIELDS};
use super::interface_dispatch::ClassRef;
use super::objects::{immutable, ObjectRoot, REFERENCE};
use super::runtime::{ADD32, EQZ32, GE_S32, LT_S32, SHL32};
use krusty::types::{Ty, TypeName};

const ELEMENTS: u32 = PREFIX_FIELDS;
const LENGTH: u32 = ELEMENTS + 1;
/// How often the list's shape changed: every insertion and removal counts, an overwrite does not.
const MODIFICATIONS: u32 = LENGTH + 1;
/// Where a range's first element sits in the shared array. A list's own is zero.
const OFFSET: u32 = MODIFICATIONS + 1;
/// The list a range was taken from, directly or through other ranges: `null` for a list.
const ROOT: u32 = OFFSET + 1;
/// The root's `MODIFICATIONS` when the range was taken. The range answers while they still match.
const EXPECTED: u32 = ROOT + 1;
/// The list a range was taken from directly, which a write through the range resizes in turn:
/// `null` for a list.
const PARENT: u32 = EXPECTED + 1;
const ITER_LIST: u32 = PREFIX_FIELDS;
const ITER_INDEX: u32 = ITER_LIST + 1;
/// The shape of the list when the iterator was taken (`Layout::shape`). A step of the iterator
/// answers while the list still has it.
const ITER_EXPECTED: u32 = ITER_INDEX + 1;

/// Whether `classifier` is `kotlin.collections.ArrayList`, which the runtime lays out.
pub(super) fn lays_out(classifier: TypeName) -> bool {
    classifier.matches("kotlin/collections/ArrayList")
}

/// A collection operation the runtime realizes on its `ArrayList`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ListOperation {
    Add,
    AddAt,
    AddAll,
    Size,
    IsEmpty,
    Get,
    Contains,
    ContainsAll,
    Remove,
    RemoveAt,
    RemoveAll,
    RetainAll,
    Clear,
    Iterator,
    ListIterator,
    ListIteratorAt,
    Set,
    SubList,
}

impl ListOperation {
    /// The bound the operation's index argument must stay within, which it throws
    /// `IndexOutOfBoundsException` past.
    pub(super) fn index_bound(self) -> Option<IndexBound> {
        match self {
            Self::Get | Self::Set | Self::RemoveAt => Some(IndexBound::BelowSize),
            Self::AddAt | Self::ListIteratorAt => Some(IndexBound::AtMostSize),
            _ => None,
        }
    }
}

/// The indices a list operation accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IndexBound {
    /// An element's index: `0 until size`.
    BelowSize,
    /// A position between elements: `0..size`.
    AtMostSize,
}

/// The operation the runtime runs for the `ArrayList` member `name` declared with `params`, or
/// why it runs none. `ArrayList` inherits several of these from `AbstractMutableList` and
/// `AbstractCollection`; a direct call and a call through an interface's member select alike.
pub(super) fn list_operation(name: &str, params: &[Ty]) -> Result<ListOperation, String> {
    use ListOperation::*;
    Ok(match (name, params) {
        ("add", [_]) => Add,
        ("add", [Ty::Int, _]) => AddAt,
        ("addAll", [_]) => AddAll,
        ("<get-size>" | "getSize" | "size", []) => Size,
        ("isEmpty", []) => IsEmpty,
        ("get", [Ty::Int]) => Get,
        ("set", [Ty::Int, _]) => Set,
        ("subList", [Ty::Int, Ty::Int]) => SubList,
        ("contains", [_]) => Contains,
        ("containsAll", [_]) => ContainsAll,
        ("removeAt", [Ty::Int]) => RemoveAt,
        ("remove", [Ty::Int]) => return Err("an ArrayList removal at an index".into()),
        ("remove", [_]) => Remove,
        ("removeAll", [_]) => RemoveAll,
        ("retainAll", [_]) => RetainAll,
        ("clear", []) => Clear,
        ("iterator", []) => Iterator,
        ("listIterator", []) => ListIterator,
        ("listIterator", [Ty::Int]) => ListIteratorAt,
        _ => {
            return Err(format!(
                "the ArrayList member `{name}`, which the runtime does not realize"
            ));
        }
    })
}

/// A runtime function realizing one `ArrayList` member, with its wasm parameters (the list
/// first) and results.
pub(super) struct ListMember {
    pub(super) function: u32,
    pub(super) params: Vec<ValType>,
    pub(super) results: Vec<ValType>,
}

/// The runtime's `ArrayList` and the iterator its `iterator` answers.
pub(super) struct ArrayLists {
    /// The list's class, which implements the collection members `ArrayList` overrides.
    pub(super) class: ClassRef,
    /// The list's struct type.
    pub(super) list: u32,
    /// The `equals`, `hashCode` and `toString` slot bodies of the list's table, which compare, hash
    /// and print its elements as `java.util.AbstractList` does. They read an argument and print
    /// elements through the collection members recorded with the list's operations
    /// (`crate::collection_arguments`), which a program records before it can hold a list.
    pub(super) value_slots: [u32; 3],
    /// The operations that read or refuse a range, whose bodies throw runtime exceptions and so
    /// are written once the checked classes are known (`crate::list_ranges`).
    pub(super) ranges: RangeOperations,
    /// The list's dispatch table, which a range of it shares.
    pub(super) table: u32,
    /// `identity.allocate`, which a range takes its identity hash from.
    pub(super) allocate: u32,
    pub(super) set: u32,
    /// `(list, index, element) -> ()`: insert at an index, in a list or through a range.
    pub(super) insert: u32,
    /// `(list, index) -> element`: remove at an index, in a list or through a range.
    pub(super) remove_at: u32,
    /// `(list, from, to) -> ()`: remove a span of elements, in a list or through a range.
    remove_range: u32,
    /// `(list, delta) -> ()`: after a write through a range changed its root's length by `delta`,
    /// the range and every range it was taken from adopt the root's elements and shape.
    resync: u32,
    pub(super) iterator: u32,
    pub(super) new: u32,
    pub(super) add: u32,
    pub(super) size: u32,
    pub(super) is_empty: u32,
    pub(super) get: u32,
    pub(super) contains: u32,
    pub(super) remove: u32,
    pub(super) clear: u32,
    pub(super) iterator_new: u32,
    /// `listIterator(index)`: an iterator whose next element is the one at `index`.
    pub(super) iterator_at: u32,
    pub(super) has_next: u32,
    /// The element after the iterator, which advances; the caller has checked `has_next`
    /// (`crate::list_iterator_ends`).
    pub(super) next: u32,
    pub(super) has_previous: u32,
    /// The element before the iterator, which steps back; the caller has checked `has_previous`.
    pub(super) previous: u32,
    pub(super) next_index: u32,
    pub(super) previous_index: u32,
    /// `next()` and `previous()` that throw `NoSuchElementException` past the list's ends, once
    /// something calls them.
    pub(super) checked_steps: [Option<u32>; 2],
    /// The operations reading a collection argument, once the members they read it through are
    /// recorded (`crate::collection_arguments`).
    pub(super) bulk: Option<BulkOperations>,
    layout: Layout,
}

/// The list operations that answer a range of a list.
#[derive(Clone, Copy)]
pub(super) struct RangeOperations {
    /// `(list) -> ()`: throws `ConcurrentModificationException` for a range whose root list
    /// changed shape since the range was taken.
    pub(super) validate: u32,
    /// `(list) -> ()`: throws `UnsupportedOperationException` for a range, for the operations that
    /// cannot be written through a range yet (`removeAll` and `retainAll`).
    pub(super) reject_write: u32,
    /// `(iterator) -> ()`: throws `ConcurrentModificationException` when the iterator's range has
    /// lost its root's shape, or the shape of the iterator's list is not the one the iterator was
    /// taken with.
    pub(super) check_walk: u32,
    /// `(list, from, to) -> range`. Like the JVM's, it checks the bounds against the size the range
    /// was taken with and does not look at the root: a range of a stale range is itself stale.
    pub(super) sub_list: u32,
}

/// The list operations that read a collection argument, and the two reads they share.
#[derive(Clone, Copy)]
pub(super) struct BulkOperations {
    pub(super) add_all: u32,
    pub(super) contains_all: u32,
    pub(super) remove_all: u32,
    pub(super) retain_all: u32,
    /// `(collection) -> list`: a collection argument as a list the runtime laid out, the argument
    /// itself when it is one.
    pub(super) elements_of: u32,
    /// `(collection, element) -> Boolean`: whether a collection argument contains the element, as
    /// its own `contains` answers.
    pub(super) argument_contains: u32,
}

impl ArrayLists {
    /// Declare the list and iterator types and define their functions. `elements` is the mutable
    /// reference-array type the list stores into. `equals` is structural equality of two
    /// references, defined later in the same module.
    pub(super) fn declare(
        module: &mut Module,
        objects: &mut ObjectRoot,
        identity: &IdentityObjects,
        elements: u32,
        chars: u32,
        string: u32,
        equals: u32,
    ) -> Self {
        let list = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(objects.any_table)),
                immutable(ValType::I32),
                mutable(ValType::reference(elements)),
                mutable(ValType::I32),
                mutable(ValType::I32),
                immutable(ValType::I32),
                immutable(REFERENCE),
                mutable(ValType::I32),
                immutable(REFERENCE),
            ]),
            supertype: Some(identity.object),
            open: false,
        });
        let iterator = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(objects.any_table)),
                immutable(ValType::I32),
                immutable(ValType::reference(list)),
                mutable(ValType::I32),
                immutable(ValType::I32),
            ]),
            supertype: Some(identity.object),
            open: false,
        });
        let value_slots = [module.declare(), module.declare(), module.declare()];
        let (class, list_table) = dispatch_table(module, objects, list, value_slots);
        let iter_text = empty_string_function(module, objects, chars, string);
        let (_, iter_table) = dispatch_table(
            module,
            objects,
            iterator,
            [objects.identity_equals, identity.hash_code, iter_text],
        );
        let layout = Layout {
            list,
            iterator,
            elements,
        };
        let lists = Self {
            class,
            list,
            value_slots,
            ranges: RangeOperations {
                validate: module.declare(),
                reject_write: module.declare(),
                check_walk: module.declare(),
                sub_list: module.declare(),
            },
            table: list_table,
            allocate: identity.allocate,
            set: module.declare(),
            insert: module.declare(),
            remove_at: module.declare(),
            remove_range: module.declare(),
            resync: module.declare(),
            iterator,
            new: module.declare(),
            add: module.declare(),
            size: module.declare(),
            is_empty: module.declare(),
            get: module.declare(),
            contains: module.declare(),
            remove: module.declare(),
            clear: module.declare(),
            iterator_new: module.declare(),
            iterator_at: module.declare(),
            has_next: module.declare(),
            next: module.declare(),
            has_previous: module.declare(),
            previous: module.declare(),
            next_index: module.declare(),
            previous_index: module.declare(),
            checked_steps: [None; 2],
            bulk: None,
            layout,
        };
        lists.define_new(module, identity, layout, list_table);
        let reserve = lists.define_reserve(module, layout);
        lists.define_size(module, layout);
        lists.define_is_empty(module);
        lists.define_get(module, layout);
        lists.define_set(module, layout);
        lists.define_insert(module, layout, reserve);
        lists.define_add(module);
        lists.define_remove_range(module, layout);
        lists.define_remove_at(module);
        lists.define_resync(module, layout);
        lists.define_contains(module, layout, equals);
        lists.define_clear(module);
        lists.define_remove(module, equals);
        lists.define_iterator(module, identity, layout, iter_table);
        lists.define_has_next(module, layout);
        lists.define_next(module, layout);
        lists.define_has_previous(module, layout);
        lists.define_previous(module, layout);
        lists.define_indices(module, layout);
        lists
    }

    /// The function realizing the list operation `operation`.
    /// An operation reading a collection argument has none until the members it reads the
    /// argument through are recorded.
    pub(super) fn member(&self, operation: ListOperation) -> Result<ListMember, String> {
        use ListOperation::*;
        let bulk = |pick: fn(&BulkOperations) -> u32| {
            self.bulk.as_ref().map(pick).ok_or_else(|| {
                "an ArrayList operation reading a collection argument, before the collection \
                 members it reads are recorded"
                    .to_string()
            })
        };
        let (function, params, results) = match operation {
            Add => (self.add, vec![REFERENCE, REFERENCE], vec![ValType::I32]),
            AddAt => (
                self.insert,
                vec![REFERENCE, ValType::I32, REFERENCE],
                Vec::new(),
            ),
            RemoveAt => (
                self.remove_at,
                vec![REFERENCE, ValType::I32],
                vec![REFERENCE],
            ),
            AddAll => (
                bulk(|bulk| bulk.add_all)?,
                vec![REFERENCE, REFERENCE],
                vec![ValType::I32],
            ),
            Size => (self.size, vec![REFERENCE], vec![ValType::I32]),
            IsEmpty => (self.is_empty, vec![REFERENCE], vec![ValType::I32]),
            Get => (self.get, vec![REFERENCE, ValType::I32], vec![REFERENCE]),
            Contains => (
                self.contains,
                vec![REFERENCE, REFERENCE],
                vec![ValType::I32],
            ),
            ContainsAll => (
                bulk(|bulk| bulk.contains_all)?,
                vec![REFERENCE, REFERENCE],
                vec![ValType::I32],
            ),
            Remove => (self.remove, vec![REFERENCE, REFERENCE], vec![ValType::I32]),
            RemoveAll => (
                bulk(|bulk| bulk.remove_all)?,
                vec![REFERENCE, REFERENCE],
                vec![ValType::I32],
            ),
            RetainAll => (
                bulk(|bulk| bulk.retain_all)?,
                vec![REFERENCE, REFERENCE],
                vec![ValType::I32],
            ),
            Clear => (self.clear, vec![REFERENCE], Vec::new()),
            Iterator | ListIterator => (self.iterator_new, vec![REFERENCE], vec![REFERENCE]),
            ListIteratorAt => (
                self.iterator_at,
                vec![REFERENCE, ValType::I32],
                vec![REFERENCE],
            ),
            Set => (
                self.set,
                vec![REFERENCE, ValType::I32, REFERENCE],
                vec![REFERENCE],
            ),
            SubList => (
                self.ranges.sub_list,
                vec![REFERENCE, ValType::I32, ValType::I32],
                vec![REFERENCE],
            ),
        };
        Ok(ListMember {
            function,
            params,
            results,
        })
    }

    fn define_new(
        &self,
        module: &mut Module,
        identity: &IdentityObjects,
        layout: Layout,
        table: u32,
    ) {
        let ty = module.func_type(vec![ValType::I32], vec![REFERENCE]);
        let mut code = Code::default();
        // A negative capacity is not a list. Zero is an empty array that the first add grows.
        code.local_get(0)
            .i32_const(0)
            .op(LT_S32)
            .if_(BlockType::Empty)
            .unreachable()
            .end();
        code.global_get(table)
            .call(identity.allocate)
            .local_get(0)
            .array_new_default(layout.elements)
            .i32_const(0)
            .i32_const(0)
            .i32_const(0)
            .ref_null(HeapType::Eq)
            .i32_const(0)
            .ref_null(HeapType::Eq)
            .struct_new(layout.list);
        module.define(
            self.new,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    /// Grow `list` so one more element fits. The argument is the list.
    fn define_reserve(&self, module: &mut Module, layout: Layout) -> u32 {
        let reserve = module.declare();
        let ty = module.func_type(vec![REFERENCE], Vec::new());
        let (list, capacity, fresh) = (0, 1, 2);
        let mut code = Code::default();
        layout.elements(&mut code, list);
        code.array_len().local_set(capacity);
        layout.length(&mut code, list);
        code.local_get(capacity).op(GE_S32).if_(BlockType::Empty);
        code.local_get(capacity)
            .op(EQZ32)
            .if_(BlockType::Value(ValType::I32))
            .i32_const(10)
            .else_()
            .local_get(capacity)
            .i32_const(1)
            .op(SHL32)
            .end()
            .array_new_default(layout.elements)
            .local_set(fresh);
        code.local_get(fresh).i32_const(0);
        layout.elements(&mut code, list);
        code.i32_const(0)
            .local_get(capacity)
            .array_copy(layout.elements, layout.elements);
        code.local_get(list)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .local_get(fresh)
            .struct_set(layout.list, ELEMENTS);
        code.end();
        module.define(
            reserve,
            Function {
                ty,
                locals: vec![ValType::I32, ValType::reference(layout.elements)],
                code,
            },
        );
        reserve
    }

    fn define_size(&self, module: &mut Module, layout: Layout) {
        let ty = module.func_type(vec![REFERENCE], vec![ValType::I32]);
        let mut code = Code::default();
        code.local_get(0).call(self.ranges.validate);
        layout.length(&mut code, 0);
        module.define(
            self.size,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    fn define_is_empty(&self, module: &mut Module) {
        let ty = module.func_type(vec![REFERENCE], vec![ValType::I32]);
        let mut code = Code::default();
        code.local_get(0)
            .call(self.size)
            .i32_const(0)
            .op(super::runtime::EQ32);
        module.define(
            self.is_empty,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    fn define_get(&self, module: &mut Module, layout: Layout) {
        let ty = module.func_type(vec![REFERENCE, ValType::I32], vec![REFERENCE]);
        let (list, index) = (0, 1);
        let mut code = Code::default();
        code.local_get(list).call(self.ranges.validate);
        code.local_get(index)
            .i32_const(0)
            .op(LT_S32)
            .local_get(index);
        layout.length(&mut code, list);
        code.op(GE_S32)
            .op(super::runtime::OR32)
            .if_(BlockType::Empty)
            .unreachable()
            .end();
        layout.elements(&mut code, list);
        code.local_get(index);
        layout.offset(&mut code, list);
        code.op(ADD32).array_get(layout.elements);
        module.define(
            self.get,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    /// `set(index, element)`: the element it replaces. The caller has checked the index. An
    /// overwrite resizes nothing, so a range takes it at its offset in the shared elements; `get`
    /// first answers a range whose root changed shape with `ConcurrentModificationException`.
    fn define_set(&self, module: &mut Module, layout: Layout) {
        let ty = module.func_type(vec![REFERENCE, ValType::I32, REFERENCE], vec![REFERENCE]);
        let (list, index, element, old) = (0, 1, 2, 3);
        let mut code = Code::default();
        code.local_get(list)
            .local_get(index)
            .call(self.get)
            .local_set(old);
        layout.elements(&mut code, list);
        code.local_get(index);
        layout.offset(&mut code, list);
        code.op(ADD32)
            .local_get(element)
            .array_set(layout.elements)
            .local_get(old);
        module.define(
            self.set,
            Function {
                ty,
                locals: vec![REFERENCE],
                code,
            },
        );
    }

    /// `add(element)`: insert at the end of the list or range.
    fn define_add(&self, module: &mut Module) {
        let ty = module.func_type(vec![REFERENCE, REFERENCE], vec![ValType::I32]);
        let (list, element) = (0, 1);
        let mut code = Code::default();
        code.local_get(list)
            .local_get(list)
            .call(self.size)
            .local_get(element)
            .call(self.insert)
            .i32_const(1);
        module.define(
            self.add,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    /// `insert(index, element)`: the element lands at `index` of the list or range, the elements
    /// from there on move one place up, and the list's shape changes. A range inserts into its
    /// root at the range's offset, then it and every range it was taken from take the new shape.
    /// The caller has checked the index against the size.
    fn define_insert(&self, module: &mut Module, layout: Layout, reserve: u32) {
        let ty = module.func_type(vec![REFERENCE, ValType::I32, REFERENCE], Vec::new());
        let (list, index, element, root, at) = (0, 1, 2, 3, 4);
        let mut code = Code::default();
        code.local_get(list).call(self.ranges.validate);
        layout.root_of(&mut code, list);
        code.local_set(root);
        code.local_get(root).call(reserve);
        layout.offset(&mut code, list);
        code.local_get(index).op(ADD32).local_set(at);
        layout.elements(&mut code, root);
        code.local_get(at).i32_const(1).op(ADD32);
        layout.elements(&mut code, root);
        code.local_get(at);
        layout.length(&mut code, root);
        code.local_get(at)
            .op(super::runtime::SUB32)
            .array_copy(layout.elements, layout.elements);
        layout.elements(&mut code, root);
        code.local_get(at)
            .local_get(element)
            .array_set(layout.elements);
        code.local_get(root)
            .ref_cast(false, HeapType::Concrete(layout.list));
        layout.length(&mut code, root);
        code.i32_const(1).op(ADD32).struct_set(layout.list, LENGTH);
        layout.bump(&mut code, root);
        code.local_get(list).i32_const(1).call(self.resync);
        module.define(
            self.insert,
            Function {
                ty,
                locals: vec![REFERENCE, ValType::I32],
                code,
            },
        );
    }

    /// `removeRange(from, to)`: the elements of the list or range from `from` up to `to` leave,
    /// the ones after them close the gap, and the list's shape changes, even for an empty span, as
    /// the JVM's does.
    fn define_remove_range(&self, module: &mut Module, layout: Layout) {
        let ty = module.func_type(vec![REFERENCE, ValType::I32, ValType::I32], Vec::new());
        let (list, from, to, root, first, last) = (0, 1, 2, 3, 4, 5);
        let mut code = Code::default();
        code.local_get(list).call(self.ranges.validate);
        layout.root_of(&mut code, list);
        code.local_set(root);
        layout.offset(&mut code, list);
        code.local_get(from).op(ADD32).local_set(first);
        layout.offset(&mut code, list);
        code.local_get(to).op(ADD32).local_set(last);
        layout.elements(&mut code, root);
        code.local_get(first);
        layout.elements(&mut code, root);
        code.local_get(last);
        layout.length(&mut code, root);
        code.local_get(last)
            .op(super::runtime::SUB32)
            .array_copy(layout.elements, layout.elements);
        code.local_get(root)
            .ref_cast(false, HeapType::Concrete(layout.list));
        layout.length(&mut code, root);
        code.local_get(last)
            .local_get(first)
            .op(super::runtime::SUB32)
            .op(super::runtime::SUB32)
            .struct_set(layout.list, LENGTH);
        layout.bump(&mut code, root);
        code.local_get(list)
            .local_get(from)
            .local_get(to)
            .op(super::runtime::SUB32)
            .call(self.resync);
        module.define(
            self.remove_range,
            Function {
                ty,
                locals: vec![REFERENCE, ValType::I32, ValType::I32],
                code,
            },
        );
    }

    /// `removeAt(index)`: the element it removes. The caller has checked the index.
    fn define_remove_at(&self, module: &mut Module) {
        let ty = module.func_type(vec![REFERENCE, ValType::I32], vec![REFERENCE]);
        let (list, index, old) = (0, 1, 2);
        let mut code = Code::default();
        code.local_get(list)
            .local_get(index)
            .call(self.get)
            .local_set(old);
        code.local_get(list)
            .local_get(index)
            .local_get(index)
            .i32_const(1)
            .op(ADD32)
            .call(self.remove_range)
            .local_get(old);
        module.define(
            self.remove_at,
            Function {
                ty,
                locals: vec![REFERENCE],
                code,
            },
        );
    }

    /// After a write through the range in local 0 resized its root by local 1, the range and every
    /// range it was taken from have the root's elements, the new length and the root's shape, so
    /// they go on answering. Any other range of the root keeps its old shape and no longer does.
    /// A list has nothing to take.
    fn define_resync(&self, module: &mut Module, layout: Layout) {
        let ty = module.func_type(vec![REFERENCE, ValType::I32], Vec::new());
        let (list, delta, range, root) = (0, 1, 2, 3);
        let mut code = Code::default();
        layout.root_of(&mut code, list);
        code.local_set(root);
        code.local_get(list).local_set(range);
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(range)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .struct_get(layout.list, ROOT)
            .ref_is_null()
            .br_if(1);
        code.local_get(range)
            .ref_cast(false, HeapType::Concrete(layout.list));
        layout.length(&mut code, range);
        code.local_get(delta)
            .op(ADD32)
            .struct_set(layout.list, LENGTH);
        code.local_get(range)
            .ref_cast(false, HeapType::Concrete(layout.list));
        layout.elements(&mut code, root);
        code.struct_set(layout.list, ELEMENTS);
        code.local_get(range)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .local_get(root)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .struct_get(layout.list, MODIFICATIONS)
            .struct_set(layout.list, EXPECTED);
        code.local_get(range)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .struct_get(layout.list, PARENT)
            .local_set(range)
            .br(0)
            .end()
            .end();
        module.define(
            self.resync,
            Function {
                ty,
                locals: vec![REFERENCE, REFERENCE],
                code,
            },
        );
    }

    fn define_contains(&self, module: &mut Module, layout: Layout, equals: u32) {
        let ty = module.func_type(vec![REFERENCE, REFERENCE], vec![ValType::I32]);
        let (list, element, index) = (0, 1, 2);
        let mut code = Code::default();
        code.local_get(list).call(self.ranges.validate);
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(index);
        layout.length(&mut code, list);
        code.op(GE_S32).br_if(1);
        code.local_get(element);
        layout.elements(&mut code, list);
        code.local_get(index);
        layout.offset(&mut code, list);
        code.op(ADD32)
            .array_get(layout.elements)
            .call(equals)
            .if_(BlockType::Empty)
            .i32_const(1)
            .return_()
            .end();
        code.local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index)
            .br(0)
            .end()
            .end()
            .i32_const(0);
        module.define(
            self.contains,
            Function {
                ty,
                locals: vec![ValType::I32],
                code,
            },
        );
    }

    fn define_clear(&self, module: &mut Module) {
        let ty = module.func_type(vec![REFERENCE], Vec::new());
        let mut code = Code::default();
        // `size` first: an empty range of a list that changed still answers to the list.
        code.local_get(0)
            .i32_const(0)
            .local_get(0)
            .call(self.size)
            .call(self.remove_range);
        module.define(
            self.clear,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    fn define_remove(&self, module: &mut Module, equals: u32) {
        let ty = module.func_type(vec![REFERENCE, REFERENCE], vec![ValType::I32]);
        let (list, element, index) = (0, 1, 2);
        let mut code = Code::default();
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(index)
            .local_get(list)
            .call(self.size)
            .op(GE_S32)
            .br_if(1);
        code.local_get(element)
            .local_get(list)
            .local_get(index)
            .call(self.get)
            .call(equals)
            .if_(BlockType::Empty)
            .local_get(list)
            .local_get(index)
            .local_get(index)
            .i32_const(1)
            .op(ADD32)
            .call(self.remove_range)
            .i32_const(1)
            .return_()
            .end();
        code.local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index)
            .br(0)
            .end()
            .end()
            .i32_const(0);
        module.define(
            self.remove,
            Function {
                ty,
                locals: vec![ValType::I32],
                code,
            },
        );
    }

    /// Close the gap at local `index` of `list` and drop the length by one. `tail` is a free
    /// `i32` local.
    fn shift_down(&self, code: &mut Code, layout: Layout, list: u32, index: u32, tail: u32) {
        layout.length(code, list);
        code.local_get(index)
            .op(super::runtime::SUB32)
            .i32_const(1)
            .op(super::runtime::SUB32)
            .local_set(tail);
        layout.elements(code, list);
        code.local_get(index);
        layout.elements(code, list);
        code.local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_get(tail)
            .array_copy(layout.elements, layout.elements);
        code.local_get(list)
            .ref_cast(false, HeapType::Concrete(layout.list));
        layout.length(code, list);
        code.i32_const(1)
            .op(super::runtime::SUB32)
            .struct_set(layout.list, LENGTH);
        layout.bump(code, list);
    }

    /// `addAll(elements)`. The elements are taken out before the first is appended, as
    /// `ArrayList.addAll` takes `c.toArray()`: appending changes the shape of a list the argument
    /// may be a range of.
    pub(super) fn define_add_all(&self, module: &mut Module, bulk: &BulkOperations) {
        let layout = self.layout;
        let ty = module.func_type(vec![REFERENCE, REFERENCE], vec![ValType::I32]);
        let (list, other, count, index, taken) = (0, 1, 2, 3, 4);
        let mut code = Code::default();
        code.local_get(other)
            .call(bulk.elements_of)
            .local_set(other);
        layout.length(&mut code, other);
        code.local_tee(count)
            .array_new_default(layout.elements)
            .local_set(taken);
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(index).local_get(count).op(GE_S32).br_if(1);
        code.local_get(taken)
            .local_get(index)
            .local_get(other)
            .local_get(index)
            .call(self.get)
            .array_set(layout.elements);
        code.local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index)
            .br(0)
            .end()
            .end();
        code.i32_const(0).local_set(index);
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(index).local_get(count).op(GE_S32).br_if(1);
        code.local_get(list)
            .local_get(taken)
            .local_get(index)
            .array_get(layout.elements)
            .call(self.add)
            .drop_();
        code.local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index)
            .br(0)
            .end()
            .end();
        code.local_get(count)
            .i32_const(0)
            .op(super::runtime::GT_S32);
        module.define(
            bulk.add_all,
            Function {
                ty,
                locals: vec![
                    ValType::I32,
                    ValType::I32,
                    ValType::reference(layout.elements),
                ],
                code,
            },
        );
    }

    /// `removeAll` when `drop_contained` is set, otherwise `retainAll`. Both walk the receiver and
    /// ask the argument whether it contains each element.
    pub(super) fn define_remove_where(
        &self,
        module: &mut Module,
        bulk: &BulkOperations,
        drop_contained: bool,
    ) {
        let layout = self.layout;
        let function = if drop_contained {
            bulk.remove_all
        } else {
            bulk.retain_all
        };
        let ty = module.func_type(vec![REFERENCE, REFERENCE], vec![ValType::I32]);
        let (list, other, index, changed) = (0, 1, 2, 3);
        let mut code = Code::default();
        code.local_get(list).call(self.ranges.reject_write);
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(index);
        layout.length(&mut code, list);
        code.op(GE_S32).br_if(1);
        code.local_get(other)
            .local_get(list)
            .local_get(index)
            .call(self.get)
            .call(bulk.argument_contains);
        if !drop_contained {
            code.op(EQZ32);
        }
        code.if_(BlockType::Empty);
        self.shift_down(&mut code, layout, list, index, 4);
        code.i32_const(1).local_set(changed).else_();
        code.local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index)
            .end()
            .br(0)
            .end()
            .end();
        code.local_get(changed);
        module.define(
            function,
            Function {
                ty,
                locals: vec![ValType::I32, ValType::I32, ValType::I32],
                code,
            },
        );
    }

    fn define_iterator(
        &self,
        module: &mut Module,
        identity: &IdentityObjects,
        layout: Layout,
        table: u32,
    ) {
        let ty = module.func_type(vec![REFERENCE, ValType::I32], vec![REFERENCE]);
        let mut code = Code::default();
        code.local_get(0).call(self.ranges.validate);
        code.global_get(table)
            .call(identity.allocate)
            .local_get(0)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .local_get(1);
        layout.shape(&mut code, 0);
        code.struct_new(layout.iterator);
        module.define(
            self.iterator_at,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
        let ty = module.func_type(vec![REFERENCE], vec![REFERENCE]);
        let mut code = Code::default();
        code.local_get(0).i32_const(0).call(self.iterator_at);
        module.define(
            self.iterator_new,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    fn define_has_next(&self, module: &mut Module, layout: Layout) {
        let ty = module.func_type(vec![REFERENCE], vec![ValType::I32]);
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(layout.iterator))
            .struct_get(layout.iterator, ITER_INDEX);
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(layout.iterator))
            .struct_get(layout.iterator, ITER_LIST)
            .struct_get(layout.list, LENGTH)
            .op(LT_S32);
        module.define(
            self.has_next,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    fn define_next(&self, module: &mut Module, layout: Layout) {
        let ty = module.func_type(vec![REFERENCE], vec![REFERENCE]);
        let (iter, index, list) = (0, 1, 2);
        let mut code = Code::default();
        code.local_get(iter).call(self.ranges.check_walk);
        code.local_get(iter)
            .ref_cast(false, HeapType::Concrete(layout.iterator))
            .struct_get(layout.iterator, ITER_INDEX)
            .local_set(index);
        code.local_get(iter)
            .ref_cast(false, HeapType::Concrete(layout.iterator))
            .struct_get(layout.iterator, ITER_LIST)
            .local_set(list);
        code.local_get(iter)
            .ref_cast(false, HeapType::Concrete(layout.iterator))
            .local_get(index)
            .i32_const(1)
            .op(ADD32)
            .struct_set(layout.iterator, ITER_INDEX);
        code.local_get(list).local_get(index).call(self.get);
        module.define(
            self.next,
            Function {
                ty,
                locals: vec![ValType::I32, REFERENCE],
                code,
            },
        );
    }
}

/// The runtime exception classes a range throws, by the struct of each.
#[derive(Clone, Copy)]
pub(super) struct RangeExceptions {
    pub(super) concurrent_modification: u32,
    pub(super) unsupported_operation: u32,
    pub(super) index_out_of_bounds: u32,
    pub(super) illegal_argument: u32,
}

/// Construct the runtime exception whose struct is the argument and throw it.
pub(super) type Throw<'a> = &'a dyn Fn(&mut Code, u32) -> Result<(), String>;

impl ArrayLists {
    /// The bodies of the operations that answer a range of a list: `validate`, `reject_write`,
    /// `check_walk` and `sub_list`. A range shares the list's elements, starting at an offset, and
    /// stays valid while its root list keeps the shape it had. A write that resizes it goes to the
    /// root and takes the new shape in the range and the ranges it was taken from
    /// (`ArrayLists::define_resync`); `removeAll` and `retainAll` through a range are not supported.
    pub(super) fn define_ranges(
        &self,
        module: &mut Module,
        throw: Throw<'_>,
        exceptions: RangeExceptions,
    ) -> Result<(), String> {
        let layout = self.layout;
        // validate(list)
        let ty = module.func_type(vec![REFERENCE], Vec::new());
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .struct_get(layout.list, ROOT)
            .ref_is_null()
            .op(EQZ32)
            .if_(BlockType::Empty);
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .struct_get(layout.list, ROOT)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .struct_get(layout.list, MODIFICATIONS)
            .local_get(0)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .struct_get(layout.list, EXPECTED)
            .op(super::runtime::NE32)
            .if_(BlockType::Empty);
        throw(&mut code, exceptions.concurrent_modification)?;
        code.end().end();
        module.define(
            self.ranges.validate,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
        // reject_write(list)
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .struct_get(layout.list, ROOT)
            .ref_is_null()
            .op(EQZ32)
            .if_(BlockType::Empty);
        throw(&mut code, exceptions.unsupported_operation)?;
        code.end();
        module.define(
            self.ranges.reject_write,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
        // check_walk(iterator)
        let ty = module.func_type(vec![REFERENCE], Vec::new());
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(layout.iterator))
            .struct_get(layout.iterator, ITER_LIST)
            .local_set(1);
        // A range answers for its root's actual shape, not the one it cached when it was taken.
        code.local_get(1).call(self.ranges.validate);
        layout.shape(&mut code, 1);
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(layout.iterator))
            .struct_get(layout.iterator, ITER_EXPECTED)
            .op(super::runtime::NE32)
            .if_(BlockType::Empty);
        throw(&mut code, exceptions.concurrent_modification)?;
        code.end();
        module.define(
            self.ranges.check_walk,
            Function {
                ty,
                locals: vec![REFERENCE],
                code,
            },
        );
        // sub_list(list, from, to)
        let ty = module.func_type(vec![REFERENCE, ValType::I32, ValType::I32], vec![REFERENCE]);
        let (list, from, to, root) = (0, 1, 2, 3);
        let mut code = Code::default();
        code.local_get(from).i32_const(0).op(LT_S32).local_get(to);
        layout.length(&mut code, list);
        code.op(super::runtime::GT_S32)
            .op(super::runtime::OR32)
            .if_(BlockType::Empty);
        throw(&mut code, exceptions.index_out_of_bounds)?;
        code.end();
        code.local_get(from)
            .local_get(to)
            .op(super::runtime::GT_S32)
            .if_(BlockType::Empty);
        throw(&mut code, exceptions.illegal_argument)?;
        code.end();
        // The root: the list itself, or what the range it is was taken from.
        code.local_get(list)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .struct_get(layout.list, ROOT)
            .ref_is_null()
            .if_(BlockType::Value(REFERENCE))
            .local_get(list)
            .else_()
            .local_get(list)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .struct_get(layout.list, ROOT)
            .end()
            .local_set(root);
        code.global_get(self.table).call(self.allocate);
        layout.elements(&mut code, list);
        code.local_get(to).local_get(from).op(super::runtime::SUB32);
        code.i32_const(0);
        layout.offset(&mut code, list);
        code.local_get(from).op(ADD32).local_get(root);
        // A range of a list expects the shape the list has now; a range of a range expects the one
        // that range expects, so one the root has left behind stays left behind.
        code.local_get(list)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .struct_get(layout.list, ROOT)
            .ref_is_null()
            .if_(BlockType::Value(ValType::I32))
            .local_get(root)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .struct_get(layout.list, MODIFICATIONS)
            .else_()
            .local_get(list)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .struct_get(layout.list, EXPECTED)
            .end()
            .local_get(list)
            .struct_new(layout.list);
        module.define(
            self.ranges.sub_list,
            Function {
                ty,
                locals: vec![REFERENCE],
                code,
            },
        );
        Ok(())
    }

    /// Give the range operations bodies a module that never records the collection members gets:
    /// it holds no list, so none is reached.
    pub(super) fn define_unreachable_ranges(&self, module: &mut Module) {
        let void = module.func_type(vec![REFERENCE], Vec::new());
        let sub_list =
            module.func_type(vec![REFERENCE, ValType::I32, ValType::I32], vec![REFERENCE]);
        for (function, ty) in [
            (self.ranges.validate, void),
            (self.ranges.reject_write, void),
            (self.ranges.check_walk, void),
            (self.ranges.sub_list, sub_list),
        ] {
            let mut code = Code::default();
            code.unreachable();
            module.define(
                function,
                Function {
                    ty,
                    locals: Vec::new(),
                    code,
                },
            );
        }
    }

    fn define_has_previous(&self, module: &mut Module, layout: Layout) {
        let ty = module.func_type(vec![REFERENCE], vec![ValType::I32]);
        let mut code = Code::default();
        code.i32_const(0);
        iterator_index(&mut code, layout, 0);
        code.op(LT_S32);
        module.define(
            self.has_previous,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    fn define_previous(&self, module: &mut Module, layout: Layout) {
        let ty = module.func_type(vec![REFERENCE], vec![REFERENCE]);
        let (iter, index) = (0, 1);
        let mut code = Code::default();
        code.local_get(iter).call(self.ranges.check_walk);
        iterator_index(&mut code, layout, iter);
        code.i32_const(-1).op(ADD32).local_set(index);
        code.local_get(iter)
            .ref_cast(false, HeapType::Concrete(layout.iterator))
            .local_get(index)
            .struct_set(layout.iterator, ITER_INDEX);
        code.local_get(iter)
            .ref_cast(false, HeapType::Concrete(layout.iterator))
            .struct_get(layout.iterator, ITER_LIST)
            .local_get(index)
            .call(self.get);
        module.define(
            self.previous,
            Function {
                ty,
                locals: vec![ValType::I32],
                code,
            },
        );
    }

    /// `nextIndex()`, the iterator's position, and `previousIndex()`, the one before it.
    fn define_indices(&self, module: &mut Module, layout: Layout) {
        let ty = module.func_type(vec![REFERENCE], vec![ValType::I32]);
        for (function, offset) in [(self.next_index, 0), (self.previous_index, -1)] {
            let mut code = Code::default();
            iterator_index(&mut code, layout, 0);
            if offset != 0 {
                code.i32_const(offset).op(ADD32);
            }
            module.define(
                function,
                Function {
                    ty,
                    locals: Vec::new(),
                    code,
                },
            );
        }
    }
}

/// Push the position of the iterator in local `iter`.
fn iterator_index(code: &mut Code, layout: Layout, iter: u32) {
    code.local_get(iter)
        .ref_cast(false, HeapType::Concrete(layout.iterator))
        .struct_get(layout.iterator, ITER_INDEX);
}

#[derive(Clone, Copy)]
struct Layout {
    list: u32,
    iterator: u32,
    elements: u32,
}

impl Layout {
    fn elements(self, code: &mut Code, list: u32) {
        code.local_get(list)
            .ref_cast(false, HeapType::Concrete(self.list))
            .struct_get(self.list, ELEMENTS);
    }

    fn length(self, code: &mut Code, list: u32) {
        code.local_get(list)
            .ref_cast(false, HeapType::Concrete(self.list))
            .struct_get(self.list, LENGTH);
    }

    /// Where the range in local `list` starts in its elements.
    fn offset(self, code: &mut Code, list: u32) {
        code.local_get(list)
            .ref_cast(false, HeapType::Concrete(self.list))
            .struct_get(self.list, OFFSET);
    }

    /// Push the list in local `list`, or the list a range in it was taken from.
    fn root_of(self, code: &mut Code, list: u32) {
        code.local_get(list)
            .ref_cast(false, HeapType::Concrete(self.list))
            .struct_get(self.list, ROOT)
            .ref_is_null()
            .if_(BlockType::Value(REFERENCE))
            .local_get(list)
            .else_()
            .local_get(list)
            .ref_cast(false, HeapType::Concrete(self.list))
            .struct_get(self.list, ROOT)
            .end();
    }

    /// Push the shape of the list or range in local `list`: how many times a list's shape
    /// changed, or the one of its root a range answers for.
    fn shape(self, code: &mut Code, list: u32) {
        code.local_get(list)
            .ref_cast(false, HeapType::Concrete(self.list))
            .struct_get(self.list, ROOT)
            .ref_is_null()
            .if_(BlockType::Value(ValType::I32))
            .local_get(list)
            .ref_cast(false, HeapType::Concrete(self.list))
            .struct_get(self.list, MODIFICATIONS)
            .else_()
            .local_get(list)
            .ref_cast(false, HeapType::Concrete(self.list))
            .struct_get(self.list, EXPECTED)
            .end();
    }

    /// Count a change of the shape of the list in local `list`.
    fn bump(self, code: &mut Code, list: u32) {
        code.local_get(list)
            .ref_cast(false, HeapType::Concrete(self.list))
            .local_get(list)
            .ref_cast(false, HeapType::Concrete(self.list))
            .struct_get(self.list, MODIFICATIONS)
            .i32_const(1)
            .op(ADD32)
            .struct_set(self.list, MODIFICATIONS);
    }
}

fn dispatch_table(
    module: &mut Module,
    objects: &mut ObjectRoot,
    structure: u32,
    slots: [u32; 3],
) -> (ClassRef, u32) {
    let (class, itable) = objects
        .interfaces
        .class(module, objects.itable, vec![structure]);
    let mut init = Code::default();
    for function in slots {
        module.reference_function(function);
        init.ref_func(function);
    }
    init.global_get(itable);
    objects.push_type_info(&mut init, None);
    init.struct_new(objects.any_table);
    let table = module.constant_global(ValType::reference(objects.any_table), init);
    (class, table)
}

/// A mutable struct field holding `value`.
fn mutable(value: ValType) -> FieldType {
    FieldType {
        storage: StorageType::Val(value),
        mutable: true,
    }
}

/// `() -> String` of no units, for a runtime object's `toString` slot the program does not call.
fn empty_string_function(
    module: &mut Module,
    objects: &ObjectRoot,
    chars: u32,
    string: u32,
) -> u32 {
    let function = module.declare();
    let ty = objects.any_slot_types[2];
    let mut code = Code::default();
    code.i32_const(0)
        .array_new_default(chars)
        .struct_new(string);
    module.define(
        function,
        Function {
            ty,
            locals: Vec::new(),
            code,
        },
    );
    function
}

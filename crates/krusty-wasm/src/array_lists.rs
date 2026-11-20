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
const ITER_LIST: u32 = PREFIX_FIELDS;
const ITER_INDEX: u32 = ITER_LIST + 1;

/// Whether `classifier` is `kotlin.collections.ArrayList`, which the runtime lays out.
pub(super) fn lays_out(classifier: TypeName) -> bool {
    classifier.matches("kotlin/collections/ArrayList")
}

/// A collection operation the runtime realizes on its `ArrayList`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ListOperation {
    Add,
    AddAll,
    Size,
    IsEmpty,
    Get,
    Contains,
    ContainsAll,
    Remove,
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
            Self::Get | Self::Set => Some(IndexBound::BelowSize),
            Self::ListIteratorAt => Some(IndexBound::AtMostSize),
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
        ("add", [Ty::Int, _]) => return Err("an ArrayList insertion at an index".into()),
        ("addAll", [_]) => AddAll,
        ("<get-size>" | "getSize" | "size", []) => Size,
        ("isEmpty", []) => IsEmpty,
        ("get", [Ty::Int]) => Get,
        ("set", [Ty::Int, _]) => Set,
        ("subList", [Ty::Int, Ty::Int]) => SubList,
        ("contains", [_]) => Contains,
        ("containsAll", [_]) => ContainsAll,
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
    /// `(list) -> ()`: throws `UnsupportedOperationException` for a range, which cannot be written
    /// through yet.
    pub(super) reject_write: u32,
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
                immutable(ValType::I32),
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
                sub_list: module.declare(),
            },
            table: list_table,
            allocate: identity.allocate,
            set: module.declare(),
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
        lists.define_add(module, layout, reserve);
        lists.define_contains(module, layout, equals);
        lists.define_clear(module, layout);
        lists.define_remove(module, layout, equals);
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

    fn define_add(&self, module: &mut Module, layout: Layout, reserve: u32) {
        let ty = module.func_type(vec![REFERENCE, REFERENCE], vec![ValType::I32]);
        let (list, element, at) = (0, 1, 2);
        let mut code = Code::default();
        code.local_get(list).call(self.ranges.reject_write);
        code.local_get(list).call(reserve);
        layout.length(&mut code, list);
        code.local_set(at);
        layout.elements(&mut code, list);
        code.local_get(at)
            .local_get(element)
            .array_set(layout.elements);
        code.local_get(list)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .local_get(at)
            .i32_const(1)
            .op(ADD32)
            .struct_set(layout.list, LENGTH);
        layout.bump(&mut code, list);
        code.i32_const(1);
        module.define(
            self.add,
            Function {
                ty,
                locals: vec![ValType::I32],
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

    fn define_clear(&self, module: &mut Module, layout: Layout) {
        let ty = module.func_type(vec![REFERENCE], Vec::new());
        let mut code = Code::default();
        code.local_get(0).call(self.ranges.reject_write);
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(layout.list))
            .i32_const(0)
            .struct_set(layout.list, LENGTH);
        layout.bump(&mut code, 0);
        module.define(
            self.clear,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    fn define_remove(&self, module: &mut Module, layout: Layout, equals: u32) {
        let ty = module.func_type(vec![REFERENCE, REFERENCE], vec![ValType::I32]);
        let (list, element, index) = (0, 1, 2);
        let mut code = Code::default();
        code.local_get(list).call(self.ranges.reject_write);
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(index);
        layout.length(&mut code, list);
        code.op(GE_S32).br_if(1);
        code.local_get(element);
        layout.elements(&mut code, list);
        code.local_get(index)
            .array_get(layout.elements)
            .call(equals)
            .if_(BlockType::Empty);
        self.shift_down(&mut code, layout, list, index, 3);
        code.i32_const(1).return_().end();
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
                locals: vec![ValType::I32, ValType::I32],
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
            .local_get(1)
            .struct_new(layout.iterator);
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
    /// The bodies of the operations that answer a range of a list: `validate`, `reject_write` and
    /// `sub_list`. A range shares the list's elements, starting at an offset, and stays valid while
    /// its root list keeps the shape it had. Writing through a range is not supported: it would
    /// have to resize every range between it and the root.
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

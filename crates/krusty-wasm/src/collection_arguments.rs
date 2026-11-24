//! A collection argument of the runtime's `ArrayList`: the `elements` of `addAll`, `containsAll`,
//! `removeAll` and `retainAll`, and of the `ArrayList(Collection)` constructor.
//!
//! The argument may be any `Collection`: another runtime list, a list built from an array
//! (`listOf(x)` wraps its array as `ArrayAsCollection`), a set, or a class of the program. The
//! stdlib walks it with its `iterator()` and asks it `contains`, so that collection's own members
//! run. Those are the checked members the handoff froze
//! (`krusty::backend::CheckedBackendCallables::collection_protocol`), called through their
//! interface stubs, which the runtime list and every class implementing them answer. A runtime
//! list is read directly. The operations reading an argument exist once those members are
//! recorded, which the code generator does before anything reaches a runtime list's member.

use super::array_lists::{BulkOperations, RangeExceptions};
use super::encode::ValType;
use super::encode::{BlockType, Code, Function, HeapType, Module};
use super::interface_dispatch::MemberRef;
use super::objects::REFERENCE;
use super::runtime::{Runtime, ADD32, AND32, EQZ32, GE_S32, MUL32, NE32};
use krusty::backend::BackendClassifierSource;
use krusty::libraries::RuntimeExceptionKind;

/// The checked members a collection argument is read through.
#[derive(Clone, Copy)]
pub(super) struct CollectionMembers {
    pub(super) iterator: MemberRef,
    pub(super) has_next: MemberRef,
    pub(super) next: MemberRef,
    pub(super) contains: MemberRef,
}

/// The members the module records once something reaches a runtime list.
#[derive(Default)]
pub(super) struct CollectionArguments {
    members: Option<CollectionMembers>,
}

impl CollectionArguments {
    /// Whether the members an argument is read through are recorded.
    pub(super) fn has_members(&self) -> bool {
        self.members.is_some()
    }
}

impl Runtime {
    /// Record the checked members a collection argument is read through, and define the list
    /// operations reading one (`crate::array_lists::BulkOperations`) over them.
    pub(super) fn record_collection_members(
        &mut self,
        module: &mut Module,
        classifiers: &dyn BackendClassifierSource,
        members: CollectionMembers,
    ) -> Result<(), String> {
        if self.collection_arguments.members.is_some() {
            return Ok(());
        }
        self.define_list_ranges(module, classifiers)?;
        self.collection_arguments.members = Some(members);
        let bulk = BulkOperations {
            add_all: module.declare(),
            contains_all: module.declare(),
            remove_all: module.declare(),
            retain_all: module.declare(),
            elements_of: module.declare(),
            argument_contains: module.declare(),
        };
        self.define_list_value_members(module, members);
        self.define_elements_of(module, members, bulk.elements_of);
        self.define_argument_contains(module, members, bulk.argument_contains);
        self.define_contains_all(module, members, bulk.contains_all);
        let lists = &self.array_lists;
        lists.define_add_all(module, &bulk);
        lists.define_remove_where(module, &bulk, true);
        lists.define_remove_where(module, &bulk, false);
        self.array_lists.bulk = Some(bulk);
        Ok(())
    }

    /// The operations answering a range of a list, which throw the runtime exceptions the checked
    /// classes give.
    fn define_list_ranges(
        &mut self,
        module: &mut Module,
        classifiers: &dyn BackendClassifierSource,
    ) -> Result<(), String> {
        let mut class = |kind: RuntimeExceptionKind| {
            self.exception_class(module, classifiers, kind.classifier())
        };
        let exceptions = RangeExceptions {
            concurrent_modification: class(RuntimeExceptionKind::ConcurrentModification)?,
            unsupported_operation: class(RuntimeExceptionKind::UnsupportedOperation)?,
            index_out_of_bounds: class(RuntimeExceptionKind::IndexOutOfBounds)?,
            illegal_argument: class(RuntimeExceptionKind::IllegalArgument)?,
        };
        let (runtime_exceptions, string) = (&self.exceptions, self.string);
        let throw = |code: &mut Code, class: u32| -> Result<(), String> {
            runtime_exceptions.construct_class(code, class, None, string)?;
            code.throw(runtime_exceptions.tag);
            Ok(())
        };
        self.array_lists.define_ranges(module, &throw, exceptions)
    }

    /// `elements_of(collection)`: a runtime list as it is, any other collection copied into a new
    /// list in the order its iterator answers.
    fn define_elements_of(
        &mut self,
        module: &mut Module,
        members: CollectionMembers,
        function: u32,
    ) {
        let interfaces = &mut self.objects.interfaces;
        let iterate = interfaces.stub(module, members.iterator);
        let has_next = interfaces.stub(module, members.has_next);
        let next = interfaces.stub(module, members.next);
        let lists = &self.array_lists;
        let (collection, copy, iterator) = (0, 1, 2);
        let mut code = Code::default();
        code.local_get(collection)
            .ref_test(false, HeapType::Concrete(lists.list))
            .if_(BlockType::Empty)
            .local_get(collection)
            .call(lists.ranges.validate)
            .local_get(collection)
            .return_()
            .end()
            .i32_const(10)
            .call(lists.new)
            .local_set(copy)
            .local_get(collection)
            .call(iterate)
            .local_set(iterator)
            .block(BlockType::Empty)
            .loop_(BlockType::Empty)
            .local_get(iterator)
            .call(has_next)
            .op(EQZ32)
            .br_if(1)
            .local_get(copy)
            .local_get(iterator)
            .call(next)
            .call(lists.add)
            .drop_()
            .br(0)
            .end()
            .end()
            .local_get(copy);
        let ty = module.func_type(vec![REFERENCE], vec![REFERENCE]);
        module.define(
            function,
            Function {
                ty,
                locals: vec![REFERENCE, REFERENCE],
                code,
            },
        );
    }

    /// `contains_all(list, collection)`: whether the list contains every element the collection's
    /// iterator answers. The argument is read one element at a time and the first element the list
    /// lacks answers false, so no later advance of that iterator is observed. A runtime list
    /// argument is read by index.
    fn define_contains_all(
        &mut self,
        module: &mut Module,
        members: CollectionMembers,
        function: u32,
    ) {
        let interfaces = &mut self.objects.interfaces;
        let iterate = interfaces.stub(module, members.iterator);
        let has_next = interfaces.stub(module, members.has_next);
        let next = interfaces.stub(module, members.next);
        let lists = &self.array_lists;
        let (list, collection, iterator, index, count) = (0, 1, 2, 3, 4);
        let mut code = Code::default();
        code.local_get(collection)
            .ref_test(false, HeapType::Concrete(lists.list))
            .if_(BlockType::Empty)
            .local_get(collection)
            .call(lists.size)
            .local_set(count)
            .block(BlockType::Empty)
            .loop_(BlockType::Empty)
            .local_get(index)
            .local_get(count)
            .op(GE_S32)
            .br_if(1)
            .local_get(list)
            .local_get(collection)
            .local_get(index)
            .call(lists.get)
            .call(lists.contains)
            .op(EQZ32)
            .if_(BlockType::Empty)
            .i32_const(0)
            .return_()
            .end()
            .local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index)
            .br(0)
            .end()
            .end()
            .i32_const(1)
            .return_()
            .end()
            .local_get(collection)
            .call(iterate)
            .local_set(iterator)
            .block(BlockType::Empty)
            .loop_(BlockType::Empty)
            .local_get(iterator)
            .call(has_next)
            .op(EQZ32)
            .br_if(1)
            .local_get(list)
            .local_get(iterator)
            .call(next)
            .call(lists.contains)
            .op(EQZ32)
            .if_(BlockType::Empty)
            .i32_const(0)
            .return_()
            .end()
            .br(0)
            .end()
            .end()
            .i32_const(1);
        let ty = module.func_type(vec![REFERENCE, REFERENCE], vec![ValType::I32]);
        module.define(
            function,
            Function {
                ty,
                locals: vec![REFERENCE, ValType::I32, ValType::I32],
                code,
            },
        );
    }

    /// `argument_contains(collection, element)`: the collection's own `contains`.
    fn define_argument_contains(
        &mut self,
        module: &mut Module,
        members: CollectionMembers,
        function: u32,
    ) {
        let contains = self.objects.interfaces.stub(module, members.contains);
        let lists = &self.array_lists;
        let (collection, element) = (0, 1);
        let mut code = Code::default();
        code.local_get(collection)
            .ref_test(false, HeapType::Concrete(lists.list))
            .if_(BlockType::Empty)
            .local_get(collection)
            .local_get(element)
            .call(lists.contains)
            .return_()
            .end()
            .local_get(collection)
            .local_get(element)
            .call(contains);
        let ty = module.func_type(vec![REFERENCE, REFERENCE], vec![ValType::I32]);
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

impl Runtime {
    /// The `equals`, `hashCode` and `toString` of a runtime list: `java.util.AbstractList`'s
    /// contract, over the elements the collection's own iterator answers. A list equals another
    /// `List` with equal elements in the same order, hashes as `31 * hash + element hash` from
    /// `1`, and prints its elements between brackets, a list holding itself as `(this Collection)`.
    fn define_list_value_members(&mut self, module: &mut Module, members: CollectionMembers) {
        let interfaces = &mut self.objects.interfaces;
        let iterate = interfaces.stub(module, members.iterator);
        let has_next = interfaces.stub(module, members.has_next);
        let next = interfaces.stub(module, members.next);
        let list_class = krusty::types::type_name("kotlin/collections/List");
        let is_list = interfaces.test(module, list_class);
        let [equals_slot, hash_slot, string_slot] = self.array_lists.value_slots;
        let slot_types = self.objects.any_slot_types;

        // equals(this, other)
        let (this, other, walk, against) = (0, 1, 2, 3);
        let mut code = Code::default();
        code.local_get(this)
            .local_get(other)
            .ref_eq()
            .if_(BlockType::Empty)
            .i32_const(1)
            .return_()
            .end()
            .local_get(other)
            .ref_test(false, HeapType::Concrete(self.array_lists.list))
            .op(EQZ32)
            .local_get(other)
            .call(is_list)
            .op(EQZ32)
            .op(AND32)
            .if_(BlockType::Empty)
            .i32_const(0)
            .return_()
            .end()
            .local_get(this)
            .call(iterate)
            .local_set(walk)
            .local_get(other)
            .call(iterate)
            .local_set(against)
            .block(BlockType::Empty)
            .loop_(BlockType::Empty)
            .local_get(walk)
            .call(has_next)
            .local_get(against)
            .call(has_next)
            .op(NE32)
            .if_(BlockType::Empty)
            .i32_const(0)
            .return_()
            .end()
            .local_get(walk)
            .call(has_next)
            .op(EQZ32)
            .br_if(1)
            .local_get(walk)
            .call(next)
            .local_get(against)
            .call(next)
            .call(self.object_equals)
            .op(EQZ32)
            .if_(BlockType::Empty)
            .i32_const(0)
            .return_()
            .end()
            .br(0)
            .end()
            .end()
            .i32_const(1);
        module.define(
            equals_slot,
            Function {
                ty: slot_types[0],
                locals: vec![REFERENCE, REFERENCE],
                code,
            },
        );

        // hashCode(this)
        let (this, walk, hash) = (0, 1, 2);
        let mut code = Code::default();
        code.i32_const(1).local_set(hash);
        code.local_get(this)
            .call(iterate)
            .local_set(walk)
            .block(BlockType::Empty)
            .loop_(BlockType::Empty)
            .local_get(walk)
            .call(has_next)
            .op(EQZ32)
            .br_if(1)
            .local_get(hash)
            .i32_const(31)
            .op(MUL32)
            .local_get(walk)
            .call(next)
            .call(self.hashing.hash_code)
            .op(ADD32)
            .local_set(hash)
            .br(0)
            .end()
            .end()
            .local_get(hash);
        module.define(
            hash_slot,
            Function {
                ty: slot_types[1],
                locals: vec![REFERENCE, ValType::I32],
                code,
            },
        );

        // toString(this)
        let string = self.string_type();
        let (this, walk, text, first, element) = (0, 1, 2, 3, 4);
        let mut code = Code::default();
        self.ascii_constant(module, &mut code, "[");
        code.local_set(text).i32_const(1).local_set(first);
        code.local_get(this)
            .call(iterate)
            .local_set(walk)
            .block(BlockType::Empty)
            .loop_(BlockType::Empty)
            .local_get(walk)
            .call(has_next)
            .op(EQZ32)
            .br_if(1)
            .local_get(first)
            .op(EQZ32)
            .if_(BlockType::Empty)
            .local_get(text);
        self.ascii_constant(module, &mut code, ", ");
        code.call(self.string_concat)
            .local_set(text)
            .end()
            .i32_const(0)
            .local_set(first)
            .local_get(walk)
            .call(next)
            .local_set(element)
            .local_get(text)
            .local_get(element)
            .local_get(this)
            .ref_eq()
            .if_(BlockType::Value(string));
        self.ascii_constant(module, &mut code, "(this Collection)");
        code.else_()
            .local_get(element)
            .call(self.object_strings.any_to_string)
            .end()
            .call(self.string_concat)
            .local_set(text)
            .br(0)
            .end()
            .end()
            .local_get(text);
        self.ascii_constant(module, &mut code, "]");
        code.call(self.string_concat);
        module.define(
            string_slot,
            Function {
                ty: slot_types[2],
                locals: vec![REFERENCE, string, ValType::I32, REFERENCE],
                code,
            },
        );
    }

    /// Give the list's table slots bodies when the program never records the collection members:
    /// no runtime list exists then, so no call reaches them.
    pub(super) fn define_unreachable_list_value_members(&self, module: &mut Module) {
        if self.collection_arguments.has_members() {
            return;
        }
        self.array_lists.define_unreachable_ranges(module);
        for (slot, ty) in self
            .array_lists
            .value_slots
            .into_iter()
            .zip(self.objects.any_slot_types)
        {
            let mut code = Code::default();
            code.unreachable();
            module.define(
                slot,
                Function {
                    ty,
                    locals: Vec::new(),
                    code,
                },
            );
        }
    }
}

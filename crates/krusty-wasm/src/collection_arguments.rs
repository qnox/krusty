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

use super::array_lists::BulkOperations;
use super::encode::ValType;
use super::encode::{BlockType, Code, Function, HeapType, Module};
use super::interface_dispatch::MemberRef;
use super::objects::REFERENCE;
use super::runtime::{Runtime, ADD32, EQZ32, GE_S32};

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
        members: CollectionMembers,
    ) {
        if self.collection_arguments.members.is_some() {
            return;
        }
        self.collection_arguments.members = Some(members);
        let bulk = BulkOperations {
            add_all: module.declare(),
            contains_all: module.declare(),
            remove_all: module.declare(),
            retain_all: module.declare(),
            elements_of: module.declare(),
            argument_contains: module.declare(),
        };
        self.define_elements_of(module, members, bulk.elements_of);
        self.define_argument_contains(module, members, bulk.argument_contains);
        self.define_contains_all(module, members, bulk.contains_all);
        let lists = &self.array_lists;
        lists.define_add_all(module, &bulk);
        lists.define_remove_where(module, &bulk, true);
        lists.define_remove_where(module, &bulk, false);
        self.array_lists.bulk = Some(bulk);
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

//! A collection argument of the runtime's `ArrayList`: the `elements` of `addAll`, `containsAll`,
//! `removeAll` and `retainAll`, and of the `ArrayList(Collection)` constructor.
//!
//! The argument may be any `Collection`: another runtime list, a list built from an array
//! (`listOf(x)` wraps its array as `ArrayAsCollection`), a set, or a class of the program. The
//! stdlib walks it with its `iterator()` and asks it `contains`, so that collection's own members
//! run. Those are the checked members the handoff froze
//! (`krusty::backend::CheckedBackendCallables::collection_protocol`), called through their
//! interface stubs, which the runtime list and every class implementing them answer. A runtime
//! list is read directly.

use super::encode::ValType;
use super::encode::{BlockType, Code, Function, HeapType, Module};
use super::interface_dispatch::MemberRef;
use super::objects::REFERENCE;
use super::runtime::{Runtime, EQZ32};

/// The checked members a collection argument is read through.
#[derive(Clone, Copy)]
pub(super) struct CollectionMembers {
    pub(super) iterator: MemberRef,
    pub(super) has_next: MemberRef,
    pub(super) next: MemberRef,
    pub(super) contains: MemberRef,
}

/// The members the module records once something constructs a runtime list.
#[derive(Default)]
pub(super) struct CollectionArguments {
    members: Option<CollectionMembers>,
}

impl CollectionArguments {
    /// Whether the members an argument is read through are recorded.
    pub(super) fn has_members(&self) -> bool {
        self.members.is_some()
    }

    pub(super) fn record(&mut self, members: CollectionMembers) {
        self.members.get_or_insert(members);
    }
}

impl Runtime {
    /// Define the list's `elements_of` and `argument_contains`. Every runtime list comes from a
    /// construction, which records the members first; a module constructing none reads no
    /// argument, and both functions trap.
    pub(super) fn define_collection_arguments(&mut self, module: &mut Module) {
        let lists = &self.array_lists;
        let list = HeapType::Concrete(lists.list);
        let (new, add, contains) = (lists.new, lists.add, lists.contains);
        let (elements_of, argument_contains) = (lists.elements_of, lists.argument_contains);
        let members = self.collection_arguments.members;
        let stubs = members.map(|members| {
            let interfaces = &mut self.objects.interfaces;
            (
                interfaces.stub(module, members.iterator),
                interfaces.stub(module, members.has_next),
                interfaces.stub(module, members.next),
                interfaces.stub(module, members.contains),
            )
        });

        // `elements_of(collection)`: a runtime list as it is, any other collection copied into a
        // new list in the order its iterator answers.
        let (collection, copy, iterator) = (0, 1, 2);
        let mut code = Code::default();
        code.local_get(collection)
            .ref_test(false, list)
            .if_(BlockType::Empty)
            .local_get(collection)
            .return_()
            .end();
        match stubs {
            Some((iterate, has_next, next, _)) => {
                code.i32_const(10)
                    .call(new)
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
                    .call(add)
                    .drop_()
                    .br(0)
                    .end()
                    .end()
                    .local_get(copy);
            }
            None => {
                code.unreachable();
            }
        }
        let ty = module.func_type(vec![REFERENCE], vec![REFERENCE]);
        module.define(
            elements_of,
            Function {
                ty,
                locals: vec![REFERENCE, REFERENCE],
                code,
            },
        );

        // `argument_contains(collection, element)`: the collection's own `contains`.
        let (collection, element) = (0, 1);
        let mut code = Code::default();
        code.local_get(collection)
            .ref_test(false, list)
            .if_(BlockType::Empty)
            .local_get(collection)
            .local_get(element)
            .call(contains)
            .return_()
            .end();
        match stubs {
            Some((_, _, _, contains)) => {
                code.local_get(collection).local_get(element).call(contains);
            }
            None => {
                code.unreachable();
            }
        }
        let ty = module.func_type(vec![REFERENCE, REFERENCE], vec![ValType::I32]);
        module.define(
            argument_contains,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }
}

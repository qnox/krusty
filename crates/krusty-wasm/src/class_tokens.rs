//! Class tokens: what a class literal evaluates to, and the type information every object's
//! dispatch table points at.
//!
//! Kotlin/Wasm's standard library models `KClass` as `KClassImpl` over the type-information record
//! its compiler gives every class, and this module follows it. A **type information** record
//! (`$TypeInfo`) holds the names Kotlin reflection publishes for one classifier: `simpleName`,
//! `qualifiedName`, and the name `toString` renders. There is one per classifier, an immutable
//! global built by a constant expression, so two records are the same classifier exactly when
//! they are the same reference. A record a runtime table reserved but no classifier facts named
//! stays null: nothing is rendered from a guess, and only reading that object's class traps.
//!
//! Every dispatch table carries the record of the class its instances have
//! ([`super::objects::TYPE_INFO_FIELD`]), which is what `x::class` reads. A table of a runtime
//! object that no Kotlin classifier describes (a function value, a class token itself) carries
//! none, and reading one traps rather than naming a guess.
//!
//! A **class token** (`$KClass`) is an ordinary object wrapping one record. Kotlin's `KClass` is
//! equal by the class it stands for and not by identity, so a literal allocates a fresh token,
//! and its table answers `equals` by the record, `hashCode` by the rendered name's hash and
//! `toString` as `class <name>` — Kotlin/Wasm's `KClassImpl` renderings.

use krusty::backend::class_names::{ReflectionNameKind, ReflectionNames, ANONYMOUS_OBJECT_NAME};
use krusty::fx_hash::HashMap;
use krusty::types::TypeName;

use super::encode::{BlockType, Code, CompositeType, Function, HeapType, Module, SubType, ValType};
use super::objects::{immutable, ObjectRoot};

/// The `$TypeInfo` field holding `simpleName`.
pub(super) const SIMPLE_NAME_FIELD: u32 = 0;
/// The `$TypeInfo` field holding `qualifiedName`.
pub(super) const QUALIFIED_NAME_FIELD: u32 = 1;
/// The `$TypeInfo` field holding the name `toString` renders and `hashCode` hashes.
const RENDERED_NAME_FIELD: u32 = 2;

/// The `$KClass` field holding its record, after the dispatch table.
const INFO_FIELD: u32 = 1;

/// The type information records of one module, one per classifier.
pub(super) struct TypeInfos {
    /// `$TypeInfo`.
    pub(super) ty: u32,
    chars: u32,
    string: u32,
    globals: HashMap<TypeName, u32>,
    /// Records reserved before their names were known, in reservation order.
    unnamed: Vec<TypeName>,
}

impl TypeInfos {
    /// Declare `$TypeInfo` over the runtime's `$chars` and `$String` types.
    pub(super) fn declare(module: &mut Module, chars: u32, string: u32) -> Self {
        let ty = module.ty(CompositeType::Struct(vec![
            immutable(ValType::nullable(string)),
            immutable(ValType::nullable(string)),
            immutable(ValType::reference(string)),
        ]));
        Self {
            ty,
            chars,
            string,
            globals: HashMap::default(),
            unnamed: Vec::new(),
        }
    }

    /// The global holding `classifier`'s record, `(ref null $TypeInfo)`. A record reserved here
    /// before its names are known stays null until a file names it ([`Self::name`]).
    pub(super) fn global(&mut self, module: &mut Module, classifier: TypeName) -> u32 {
        if let Some(global) = self.globals.get(&classifier) {
            return *global;
        }
        let mut unnamed = Code::default();
        unnamed.ref_null(HeapType::Concrete(self.ty));
        let global = module.constant_global(ValType::nullable(self.ty), unnamed);
        self.globals.insert(classifier, global);
        self.unnamed.push(classifier);
        global
    }

    /// Give `classifier`'s record its names, unless it already has them. Names are declaration
    /// facts, so whichever file names a classifier first names it as every other file would.
    pub(super) fn name(
        &mut self,
        module: &mut Module,
        classifier: TypeName,
        names: &ReflectionNames,
    ) {
        let global = self.global(module, classifier);
        let Some(position) = self.unnamed.iter().position(|known| *known == classifier) else {
            return;
        };
        self.unnamed.remove(position);
        let mut init = Code::default();
        for name in [&names.simple, &names.qualified] {
            match name {
                Some(name) => self.constant_string(&mut init, name),
                None => {
                    init.ref_null(HeapType::Concrete(self.string));
                }
            }
        }
        self.constant_string(&mut init, &rendered_name(names));
        init.struct_new(self.ty);
        module.set_constant_init(global, init);
    }

    /// The classifiers whose records are still unnamed.
    pub(super) fn unnamed(&self) -> &[TypeName] {
        &self.unnamed
    }

    /// A `$String` of `text` as a constant expression: `array.new_fixed` of its code units.
    fn constant_string(&self, code: &mut Code, text: &str) {
        let units = text.encode_utf16().collect::<Vec<_>>();
        for unit in &units {
            code.i32_const(i32::from(*unit));
        }
        code.array_new_fixed(self.chars, units.len() as u32)
            .struct_new(self.string);
    }
}

/// The name a token renders: its qualified name, or the package-qualified simple name of a local
/// class, or the package-qualified placeholder of an anonymous object.
fn rendered_name(names: &ReflectionNames) -> String {
    let own = match names.kind() {
        ReflectionNameKind::Member => {
            return names
                .qualified
                .clone()
                .expect("a member classifier has a qualified name")
        }
        ReflectionNameKind::Local => names.simple.as_deref().expect("a local class has a name"),
        ReflectionNameKind::Anonymous => ANONYMOUS_OBJECT_NAME,
    };
    if names.package.is_empty() {
        own.to_string()
    } else {
        format!("{}.{own}", names.package)
    }
}

/// The class token type, its table, and the runtime functions over it.
pub(super) struct ClassTokens {
    /// `$KClass`: the dispatch table, then the record.
    pub(super) structure: u32,
    /// The global holding every token's dispatch table.
    table: u32,
}

/// The runtime functions a class token's table calls.
pub(super) struct TokenRuntime {
    pub(super) string: u32,
    pub(super) string_concat: u32,
    pub(super) string_hash: u32,
}

impl ClassTokens {
    /// Declare `$KClass` and define its `kotlin.Any` members.
    pub(super) fn declare(
        module: &mut Module,
        objects: &mut ObjectRoot,
        runtime: TokenRuntime,
        prefix: &[u16],
    ) -> Self {
        let info = objects.type_infos.ty;
        let structure = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(objects.any_table)),
                immutable(ValType::reference(info)),
            ]),
            supertype: Some(objects.object),
            open: false,
        });
        let token = HeapType::Concrete(structure);
        let [equals_ty, hash_code_ty, to_string_ty] = objects.any_slot_types;

        // `equals`: the other is a token of the same record.
        let mut code = Code::default();
        code.local_get(1)
            .ref_test(false, token)
            .if_(BlockType::Value(ValType::I32));
        for value in [0, 1] {
            code.local_get(value)
                .ref_cast(false, token)
                .struct_get(structure, INFO_FIELD);
        }
        code.ref_eq().else_().i32_const(0).end();
        let equals = define(module, equals_ty, code);

        // `hashCode`: the rendered name's hash, as `KClassImpl` hashes it.
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, token)
            .struct_get(structure, INFO_FIELD)
            .struct_get(info, RENDERED_NAME_FIELD)
            .call(runtime.string_hash);
        let hash_code = define(module, hash_code_ty, code);

        // `toString`: `class <rendered name>`.
        let mut code = Code::default();
        for unit in prefix {
            code.i32_const(i32::from(*unit));
        }
        code.array_new_fixed(objects.type_infos.chars, prefix.len() as u32)
            .struct_new(runtime.string);
        code.local_get(0)
            .ref_cast(false, token)
            .struct_get(structure, INFO_FIELD)
            .struct_get(info, RENDERED_NAME_FIELD)
            .call(runtime.string_concat);
        let to_string = define(module, to_string_ty, code);

        let mut init = Code::default();
        for function in [equals, hash_code, to_string] {
            module.reference_function(function);
            init.ref_func(function);
        }
        init.ref_null(HeapType::Concrete(objects.itable));
        objects.push_type_info(&mut init, None);
        init.struct_new(objects.any_table);
        let table = module.constant_global(ValType::reference(objects.any_table), init);
        Self { structure, table }
    }

    /// Replace the record on the stack with a new token for it.
    pub(super) fn emit_token(&self, code: &mut Code, scratch: u32) {
        code.local_set(scratch)
            .global_get(self.table)
            .local_get(scratch)
            .struct_new(self.structure);
    }

    /// Replace the token on the stack, which the frontend checked is a `KClass`, with its
    /// `simpleName` (`field` [`SIMPLE_NAME_FIELD`]) or `qualifiedName`.
    pub(super) fn emit_name(&self, code: &mut Code, info: u32, field: u32) {
        code.ref_cast(false, HeapType::Concrete(self.structure))
            .struct_get(self.structure, INFO_FIELD)
            .struct_get(info, field);
    }
}

/// The record of the object on the stack: a `String` has no table and is the `kotlin.String`
/// record `string_info`, which the caller named; every other object reads its table's, and one
/// without a named record traps.
pub(super) fn emit_runtime_info(
    code: &mut Code,
    objects: &ObjectRoot,
    string: u32,
    string_info: u32,
    scratch: u32,
) {
    let info = ValType::reference(objects.type_infos.ty);
    code.local_tee(scratch)
        .ref_test(false, HeapType::Concrete(string))
        .if_(BlockType::Value(info))
        .global_get(string_info)
        .ref_as_non_null()
        .else_()
        .local_get(scratch)
        .ref_cast(false, HeapType::Concrete(objects.object))
        .struct_get(objects.object, 0)
        .struct_get(objects.any_table, super::objects::TYPE_INFO_FIELD)
        .ref_as_non_null()
        .end();
}

fn define(module: &mut Module, ty: u32, code: Code) -> u32 {
    let function = module.declare();
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

#[cfg(test)]
mod tests;

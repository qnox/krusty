//! The member declarations of one class file's `@Metadata`, decoded on first use.
//!
//! A class's header (kind, flags, type parameters, supertypes, type aliases) is decoded when the
//! class file is read. Its functions, properties, and constructors are by far the largest part of
//! the protobuf, and most classes a compilation reads are consulted only for their header or their
//! bytecode methods: a package's member index walks every facade of every default-imported package.
//! So the member messages stay packed until a lookup asks for them, and a member message that does
//! not decode is reported to the lookup that asked, which records it as the classpath's terminal
//! load error.

use std::sync::{Arc, OnceLock};

use super::enclosing_type_parameters::{self, ScopedTypeParameter};
use super::member_names::{package_member_names, PackageMemberNames};
use super::realization_facts::Realizations;
use super::string_table::{parse_string_table, split_d1};
use super::{
    ctor_params, decode_functions, decode_properties, MetaConstructor, MetaCtx, MetaFn, MetaProp,
    MetadataResult,
};
use crate::jvm::classreader::MethodSig;
use crate::metadata::decode::Pb;
use crate::types::Ty;

/// `Package.function`, `Package.property`, `Class.constructor`, `Class.function`, and
/// `Class.property`: the member messages [`KotlinMembers`] decodes.
const MEMBER_FIELDS: [u64; 5] = [3, 4, 8, 9, 10];

/// The decoded member declarations of one class file.
#[derive(Clone, Debug, Default)]
pub struct KotlinMembers {
    /// `Class.function` (field 9) — member/extension functions of a class kind. `Arc` slices so a
    /// consumer cache shares the decode instead of copying it.
    pub class_functions: Arc<[MetaFn]>,
    /// `Package.function` (field 3) — top-level/extension functions of a file-facade/part kind.
    pub package_functions: Arc<[MetaFn]>,
    /// `Class.property` (field 10).
    pub class_properties: Arc<[MetaProp]>,
    /// `Package.property` (field 4).
    pub package_properties: Arc<[MetaProp]>,
    /// `Class.constructor` (field 8): named-parameter lists in declaration order.
    pub constructors: Arc<[MetaConstructor]>,
}

impl KotlinMembers {
    /// No members: a class without member metadata, or one whose members did not decode.
    pub fn none() -> &'static KotlinMembers {
        static NONE: OnceLock<KotlinMembers> = OnceLock::new();
        NONE.get_or_init(KotlinMembers::default)
    }
}

/// One class file's member messages, packed until [`Self::decode`] first runs.
#[derive(Clone, Debug, Default)]
pub struct LazyMembers(Option<Arc<PackedMembers>>);

/// `Debug` shows the packed messages, which determine the members, and never whether they have
/// been decoded yet: a declaration digest of a class must not depend on which lookups ran.
struct PackedMembers {
    /// The decoded `d1` bytes: string table, then the Class or Package message.
    d1: Vec<u8>,
    d2: Vec<String>,
    is_class: bool,
    /// Type parameters of the outer classes an inner class addresses by id.
    enclosing: Vec<ScopedTypeParameter>,
    /// A `data` class's own type, which bounds its generated `equals` parameter.
    data_equality_bound: Option<Ty>,
    declares_package_members: bool,
    decoded: OnceLock<MetadataResult<KotlinMembers>>,
    package_member_names: OnceLock<MetadataResult<PackageMemberNames>>,
}

impl std::fmt::Debug for PackedMembers {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PackedMembers")
            .field("d1", &self.d1)
            .field("d2", &self.d2)
            .field("is_class", &self.is_class)
            .field("enclosing", &self.enclosing)
            .field("data_equality_bound", &self.data_equality_bound)
            .finish()
    }
}

impl LazyMembers {
    pub(super) fn new(
        d1: Vec<u8>,
        d2: Vec<String>,
        is_class: bool,
        enclosing: &[ScopedTypeParameter],
        data_equality_bound: Option<Ty>,
    ) -> Self {
        let declared = declared_member_fields(&d1);
        if declared.iter().all(|declared| !declared) {
            return Self(None);
        }
        Self(Some(Arc::new(PackedMembers {
            d1,
            d2,
            is_class,
            enclosing: enclosing.to_vec(),
            data_equality_bound,
            declares_package_members: declared[0] || declared[1],
            decoded: OnceLock::new(),
            package_member_names: OnceLock::new(),
        })))
    }

    /// Whether the metadata carries any member message.
    pub fn declares_any(&self) -> bool {
        self.0.is_some()
    }

    /// Whether the metadata carries a `Package.function` or `Package.property` message.
    pub fn declares_package_members(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|packed| packed.declares_package_members)
    }

    /// The member declarations, decoded once. `methods` and `scope` are the owning class file's
    /// bytecode methods and complete type-parameter scope, which the class header already holds.
    pub fn decode(
        &self,
        methods: &[MethodSig],
        scope: &[ScopedTypeParameter],
    ) -> MetadataResult<&KotlinMembers> {
        let Some(packed) = &self.0 else {
            return Ok(KotlinMembers::none());
        };
        packed
            .decoded
            .get_or_init(|| packed.decode(methods, scope))
            .as_ref()
            .map_err(|error| *error)
    }

    /// The names of the `Package.function` and `Package.property` messages, read without decoding
    /// their signatures (see [`PackageMemberNames`]). A package member index and a lookup by name
    /// need only these; the signatures stay packed until [`Self::decode`].
    pub fn package_member_names(&self) -> MetadataResult<&PackageMemberNames> {
        static NONE: PackageMemberNames = PackageMemberNames {
            functions: Vec::new(),
            properties: Vec::new(),
        };
        let Some(packed) = &self.0 else {
            return Ok(&NONE);
        };
        packed
            .package_member_names
            .get_or_init(|| packed.with_ctx(package_member_names))
            .as_ref()
            .map_err(|error| *error)
    }
}

impl PackedMembers {
    fn with_ctx<T>(&self, read: impl FnOnce(&MetaCtx) -> T) -> T {
        let (string_table, msg) = split_d1(&self.d1);
        let records = parse_string_table(string_table);
        read(&MetaCtx {
            msg,
            records: &records,
            d2: &self.d2,
            enclosing: &self.enclosing,
        })
    }

    fn decode(
        &self,
        methods: &[MethodSig],
        scope: &[ScopedTypeParameter],
    ) -> MetadataResult<KotlinMembers> {
        self.with_ctx(|ctx| self.decode_in(ctx, methods, scope))
    }

    fn decode_in(
        &self,
        ctx: &MetaCtx,
        methods: &[MethodSig],
        scope: &[ScopedTypeParameter],
    ) -> MetadataResult<KotlinMembers> {
        let (scope_tparams, scope_bounds) = enclosing_type_parameters::split(scope);
        let realizations = Realizations::new(methods);
        let class_functions = realizations.stamp_functions(decode_functions(
            ctx,
            9,
            &scope_tparams,
            &scope_bounds,
            self.data_equality_bound,
        )?);
        // A package's field 5 is `Package.type_alias`, while a class's field 5 is
        // `Class.type_parameter`. Constructor decoding owns only the Class schema; running it over
        // a package would feed each complete TypeAlias message to the strict type-parameter
        // decoder.
        let mut constructors = if self.is_class {
            ctor_params(ctx)?
        } else {
            Vec::new()
        };
        realizations.stamp_constructors(&mut constructors);
        let class_properties = realizations.stamp_properties(decode_properties(
            ctx,
            10,
            &scope_tparams,
            &scope_bounds,
        )?);
        let package_functions =
            realizations.stamp_functions(decode_functions(ctx, 3, &[], &[], None)?);
        let package_properties =
            realizations.stamp_properties(decode_properties(ctx, 4, &[], &[])?);
        Ok(KotlinMembers {
            class_functions: class_functions.into(),
            package_functions: package_functions.into(),
            class_properties: class_properties.into(),
            package_properties: package_properties.into(),
            constructors: constructors.into(),
        })
    }
}

/// Which of [`MEMBER_FIELDS`] occur as length-delimited fields of the top-level message, found
/// without decoding them. A message that does not parse is reported as declaring members, so its
/// decode reports the error to the lookup that asks for them.
fn declared_member_fields(d1: &[u8]) -> [bool; 5] {
    let (_, msg) = split_d1(d1);
    let mut declared = [false; 5];
    let mut pb = Pb::new(msg);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else {
            return [true; 5];
        };
        let (field, wire) = (tag >> 3, tag & 7);
        if wire == 2 {
            if let Some(slot) = MEMBER_FIELDS.iter().position(|&member| member == field) {
                declared[slot] = true;
            }
        }
        if pb.skip(wire).is_none() {
            return [true; 5];
        }
    }
    declared
}

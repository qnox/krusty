//! `Type.abbreviated_type` (`@Metadata` field 13) — a typealias spelled in a DECLARED type.
//!
//! kotlinc records the SOURCE SPELLING of a declared type next to its expanded form: `fun make(c:
//! Cargo): Cargo` writes `Type{class_name=Payload, abbreviated_type=Type{type_alias_name=Cargo}}`.
//! krusty's metadata pipeline is `Ty`-driven and `Ty` is fully expanded, so the spelling was lost
//! and every alias-spelled declaration differed from kotlinc in `d1`/`d2` (bytecode, descriptors and
//! the constant pool already matched — the metadata was the only delta).
//!
//! Field numbers here are not recalled, they are read off kotlinc 2.4.10 output: `abbreviated_type`
//! is field 13 and the alias reference inside it is `type_alias_name` = field **12** (NOT field 10,
//! which several third-party writeups claim). See `docs/METADATA_NOTES.md`.

use super::common;

/// The producer half, isolated: resolving a declaration that spells an alias must record that
/// spelling against the declaration, whatever the encoder later does with it.
#[test]
fn resolution_records_the_alias_a_declared_type_spelled() {
    use krusty::ast::Decl;
    const SRC: &str = "package app\n\
        \n\
        class Payload(val v: Int)\n\
        typealias Cargo = Payload\n\
        \n\
        fun make(c: Cargo): Cargo = c\n";
    let mut diags = krusty::diag::DiagSink::new();
    let features = krusty::features::LangFeatures::from_source(SRC);
    let tokens = krusty::lexer::lex(SRC, &mut diags);
    let files = vec![krusty::parser::parse_with_features(
        SRC, &tokens, &mut diags, &features,
    )];
    let symbols = krusty::frontend::collect_signatures(&files, &mut diags);
    let make = files[0]
        .decls
        .iter()
        .copied()
        .find(|&d| matches!(files[0].decl(d), Decl::Fun(f) if f.name == "make"))
        .expect("the fixture declares `make`");
    let spellings = symbols
        .declared_spellings
        .get(&(0, make))
        .expect("a declaration spelling a typealias must be recorded");
    let cargo = Some(krusty::types::type_name("app/Cargo"));
    assert_eq!(spellings.ret.alias, cargo, "return type");
    assert_eq!(spellings.param(0).alias, cargo, "value parameter");
}

/// A same-module `typealias` spelled in a top-level function's parameter and return type. The
/// smallest shape that exercises the channel: no classpath, no generics, no nullability.
#[test]
fn same_module_typealias_in_declared_type_is_byte_identical() {
    const SRC: &str = "package app\n\
        \n\
        class Payload(val v: Int)\n\
        typealias Cargo = Payload\n\
        \n\
        fun make(c: Cargo): Cargo = c\n";
    assert_identical("Use", SRC, "app/UseKt");
}

/// Assert one same-module fixture is byte-identical to kotlinc's output for `class_internal`.
fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    // The stdlib is on the classpath because kotlinc's always is: without it krusty resolves
    // `open class`, generic bounds, and every stdlib type differently, and the comparison measures
    // the missing classpath rather than the metadata.
    let classpath = [common::stdlib_jar()];
    let Some(result) =
        common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
    else {
        eprintln!("skip ({stem}: provisioned kotlinc unavailable)");
        return;
    };
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

/// Declarations that prepend the shared fixture types every case below draws on.
const PRELUDE: &str = "package app\n\
    \n\
    open class Payload(val v: Int)\n\
    class PBox<A, B>(val a: A, val b: B)\n\
    typealias Cargo = Payload\n";

/// Controls: the SAME shapes with the alias spelled out. These must be byte-identical whatever the
/// abbreviation work does — a failure here is a pre-existing gap (or a harness limit), not an
/// abbreviation bug, and it tells the alias-spelled cases below apart from background noise.
#[test]
fn the_same_shapes_without_an_alias_are_already_byte_identical() {
    assert_identical(
        "BoundBase",
        "package app\n\nopen class Payload(val v: Int)\n\nfun <T : Payload> bound(t: T): T = t\n",
        "app/BoundBaseKt",
    );
    assert_identical(
        "PropsBase",
        "package app\n\nopen class Payload(val v: Int)\n\nval top: Payload? = null\n\nval Payload.ext: Payload get() = this\n",
        "app/PropsBaseKt",
    );
    assert_identical(
        "BoundClassBase",
        "package app\n\nopen class Payload(val v: Int)\n\nclass Bounded<T : Payload>(val t: T)\n",
        "app/Bounded",
    );
}

/// The abbreviation is recorded PER TYPE NODE, not per declaration: in `List<Cargo>` it belongs to
/// the argument's `Type`, and the enclosing `List` carries none.
#[test]
fn an_alias_inside_a_type_argument_abbreviates_that_argument_only() {
    let src = format!("{PRELUDE}\nfun nested(x: List<Cargo>): List<Cargo> = x\n");
    assert_identical("Nested", &src, "app/NestedKt");
}

/// A nullable alias repeats the `nullable` flag on the abbreviated `Type` as well as the expanded one.
#[test]
fn a_nullable_alias_marks_both_the_expanded_and_the_abbreviated_type() {
    let src = format!("{PRELUDE}\nfun maybe(x: Cargo?): Cargo? = x\n");
    assert_identical("Maybe", &src, "app/MaybeKt");
}

/// Only the OUTERMOST alias of a chain is recorded — `Chain` writes `Chain`, never `Cargo`.
#[test]
fn an_alias_chain_records_only_the_spelling_source_used() {
    let src = format!("{PRELUDE}typealias Chain = Cargo\n\nfun chained(x: Chain): Chain = x\n");
    assert_identical("Chained", &src, "app/ChainedKt");
}

/// A generic alias whose expansion has a DIFFERENT arity: the expanded `Type` takes two arguments
/// and the abbreviated one takes the single argument source actually wrote.
#[test]
fn a_generic_alias_abbreviates_with_its_as_spelled_arity() {
    let src = format!(
        "{PRELUDE}typealias Boxed<T> = PBox<T, T>\n\nfun boxed(x: Boxed<Int>): Boxed<Int> = x\n"
    );
    assert_identical("Boxed", &src, "app/BoxedKt");
}

/// A formal nested below another alias on the right-hand side must be replaced in both the semantic
/// expansion and its parallel abbreviation tree. Leaving the declaration's `T` in the spelling
/// sidecar makes a concrete non-generic function try to emit an out-of-scope type parameter.
#[test]
fn a_nested_alias_expansion_substitutes_its_formal_in_abbreviations() {
    let src = "package app\n\
        \n\
        class Inv<K>\n\
        typealias Inner<V> = Inv<V>\n\
        typealias Outer<T> = Inv<Inner<T>>\n\
        \n\
        fun concrete(): Outer<String>? = null\n";
    assert_identical("NestedFormal", src, "app/NestedFormalKt");
}

/// An alias for a FUNCTION type, whose expansion is a synthesized `FunctionN` classifier.
#[test]
fn an_alias_for_a_function_type_abbreviates_the_function_classifier() {
    let src =
        format!("{PRELUDE}typealias Handler<T> = (T) -> String\n\nfun handle(x: Handler<Int>): Handler<Int> = x\n");
    assert_identical("Handled", &src, "app/HandledKt");
}

/// A `typealias` whose RIGHT-HAND SIDE spells another alias propagates that spelling into the
/// EXPANSION: every expanded argument is abbreviated even though the use site named only the outer
/// alias. Verified against kotlinc 2.4.10.
#[test]
fn an_alias_right_hand_side_propagates_its_own_spellings_into_the_expansion() {
    let src = format!(
        "{PRELUDE}typealias CargoBox = PBox<Cargo, Cargo>\n\nfun carry(x: CargoBox): CargoBox = x\n"
    );
    assert_identical("Carry", &src, "app/CarryKt");
}

/// A declared type parameter's upper bound is a declared type like any other.
#[test]
fn a_type_parameter_bound_spelled_as_an_alias_is_abbreviated() {
    let src = format!("{PRELUDE}\nfun <T : Cargo> bound(t: T): T = t\n");
    assert_identical("Bound", &src, "app/BoundKt");
}

/// A CLASS type parameter's bound is a declared type too — the class counterpart of the row above.
/// This one had to be left out while a bounded CLASS parameter still erased to `Object`: the
/// constructor, backing field and getter all spelled `Ljava/lang/Object;`, so the fixture diverged on
/// erasure long before its abbreviation could be compared. It erases to the bound now.
#[test]
fn a_class_type_parameter_bound_spelled_as_an_alias_is_abbreviated() {
    let src = format!("{PRELUDE}\nclass Bounded<T : Cargo>(val t: T)\n");
    assert_identical("BoundedClass", &src, "app/Bounded");
}

/// A top-level property's declared type, and an extension property's receiver.
#[test]
fn property_types_and_extension_receivers_are_abbreviated() {
    let src = format!("{PRELUDE}\nval top: Cargo? = null\n\nval Cargo.ext: Cargo get() = this\n");
    assert_identical("Props", &src, "app/PropsKt");
}

/// A use-site projection wraps the argument; the abbreviation belongs to the type inside it.
#[test]
fn an_alias_under_a_use_site_projection_is_abbreviated() {
    let src = format!("{PRELUDE}\nfun projected(x: List<out Cargo>): List<out Cargo> = x\n");
    assert_identical("Projected", &src, "app/ProjectedKt");
}

/// `import kotlin.collections.List as L` is a RENAME, not a `typealias`: it binds a name in one
/// file rather than declaring a type, and kotlinc writes NO `abbreviated_type` for it. The
/// abbreviation must key off real alias declarations, not off any name that redirects to a type.
///
#[test]
fn an_import_rename_is_not_a_typealias_and_carries_no_abbreviation() {
    const SRC: &str = "package app\n\
        \n\
        import kotlin.collections.List as L\n\
        \n\
        fun renamed(x: L<Int>): L<Int> = x\n";
    assert_identical("Renamed", SRC, "app/RenamedKt");
}

/// Class metadata is built from the IR alone — no AST, no `FrontendSymbols` — so the spelling
/// reaches it over a different channel than a top-level declaration's (an IR side table filled at
/// lowering). These rows exercise that channel: a primary-constructor property, a body property,
/// a member function's parameter and return, and a member extension's receiver.
#[test]
fn class_members_spelled_as_an_alias_are_abbreviated() {
    let src = format!(
        "{PRELUDE}\n\
        class Holder(val p: Cargo) {{\n\
        \x20   var r: Cargo? = null\n\
        \x20   fun mix(a: Cargo): Cargo = a\n\
        \x20   fun Cargo.member(): Cargo = this\n\
        }}\n"
    );
    assert_identical("Holder", &src, "app/Holder");
}

/// A SUPERTYPE spelled as an alias (`class Sub : Super()`). Metadata lists the declared superclass
/// before the interfaces, and the superclass is the one declared type parked as a bare name rather
/// than a `TypeRef`, so it reaches the spelling channel by its own route.
///
/// (A CLASS type-parameter bound spelled as an alias — `class Bounded<T : Cargo>` — is not asserted
/// here: krusty erases a class type parameter to `java/lang/Object` where kotlinc erases to the
/// declared bound, so those descriptors differ with or without an alias. That is a pre-existing
/// erasure gap, tracked separately; a FUNCTION type-parameter bound is covered above and passes.)
#[test]
fn a_supertype_spelled_as_an_alias_is_abbreviated() {
    let src = format!(
        "{PRELUDE}typealias Super = Payload\n\
        \n\
        class Sub : Super(1)\n"
    );
    assert_identical("SubKt", &src, "app/Sub");
}

/// A CLASSPATH `typealias` — declared in a dependency, not in this file. It reaches the spelling
/// channel by the other route: the parse seam never rewrites it (it knows only this file's
/// aliases), so `TypeRef::name` still spells the alias and name resolution identifies it.
#[test]
fn a_classpath_typealias_in_a_declared_type_is_abbreviated() {
    const LIB: &str = "package dep\n\
        \n\
        class Payload(val v: Int)\n\
        class PBox<A, B>(val a: A, val b: B)\n\
        typealias Cargo = Payload\n\
        typealias Boxed<T> = PBox<T, T>\n";
    const SRC: &str = "package app\n\
        \n\
        import dep.Boxed\n\
        import dep.Cargo\n\
        \n\
        fun plain(x: Cargo): Cargo = x\n\
        \n\
        fun nullable(x: Cargo?): Cargo? = x\n\
        \n\
        fun generic(x: Boxed<Int>): Boxed<Int> = x\n";
    let Some(result) =
        common::metadata_diff_against_kotlinc_lib("Dep", &[("Lib.kt", LIB)], SRC, "app/DepKt")
    else {
        eprintln!("skip (provisioned kotlinc unavailable)");
        return;
    };
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

/// An alias ARGUMENT that is itself an alias — `Boxed<Cargo>`. The abbreviation's own arguments are
/// EXPANDED, each carrying its own abbreviation (`{Payload, abbreviated={Cargo}}`), NOT bare alias
/// references. This is the one place the two alias-reference forms differ: `TypeAlias.underlying_type`
/// keeps the spelling all the way down, an `abbreviated_type` does not.
#[test]
fn an_alias_argument_that_is_itself_an_alias_expands_inside_the_abbreviation() {
    let src = format!(
        "{PRELUDE}typealias Boxed<T> = PBox<T, T>\n\
        \n\
        fun nested(x: Boxed<Cargo>): Boxed<Cargo> = x\n"
    );
    assert_identical("Nestalias", &src, "app/NestaliasKt");
}

/// A FULLY QUALIFIED spelling of an alias (`app.Cargo`). kotlinc abbreviates it exactly like the
/// bare spelling, and — the part that actually broke — resolves it to the alias's TARGET.
///
/// The dotted form takes a different resolution path from the bare one: the parse seam expands only
/// simple spellings, so `app.Cargo` reaches name resolution intact and is resolved as a qualified
/// declaration. Resolving it AS A CLASSIFIER made the alias its own type, and the emitted descriptor
/// then named `app/Cargo` — a class nothing declares or emits, so the class file would fail to load.
#[test]
fn a_qualified_alias_resolves_to_its_target_and_is_abbreviated() {
    const SRC: &str = "package app\n\
        \n\
        class Payload(val v: Int)\n\
        typealias Cargo = Payload\n\
        \n\
        fun qualified(x: app.Cargo): app.Cargo = x\n";
    assert_identical("Qualified", SRC, "app/QualifiedKt");
}

/// The same declaration must also RUN: a qualified alias that resolved to itself emitted a
/// descriptor and a `checkcast` naming a class that is never emitted, which the metadata comparison
/// above cannot see — it would only surface as a `NoClassDefFoundError` at load time.
#[test]
fn a_qualified_alias_emits_a_loadable_class() {
    const LIB: &str = "package app\n\
        \n\
        class Payload(val v: Int)\n\
        typealias Cargo = Payload\n\
        \n\
        fun qualified(x: app.Cargo): app.Cargo = x\n";
    common::Fixture::new().lib("Lib.kt", LIB).assert_box_ok(
        "import app.Payload\n\
         import app.qualified\n\
         fun box(): String = if (qualified(Payload(7)).v == 7) \"OK\" else \"fail\"\n",
    );
}

/// A qualified spelling of a FUNCTION-TYPE alias (`app.Handler` where `typealias Handler = (Int) ->
/// String`). Such an alias has no classifier at all, so resolving the dotted spelling as a
/// qualified classifier could only fail — it was reported `unresolved reference 'app.Handler'`,
/// rejecting valid Kotlin. Expanding it at the parse seam, where every alias is known by shape
/// rather than by target class, is what makes both alias kinds behave alike.
#[test]
fn a_qualified_function_type_alias_resolves() {
    const SRC: &str = "package app\n\
        \n\
        typealias Handler = (Int) -> String\n\
        \n\
        fun useQualified(h: app.Handler): app.Handler = h\n";
    assert_identical("Qfun", SRC, "app/QfunKt");
}

/// The qualified spelling has three independent routes to a target, and each resolves it in a
/// different place: a SAME-FILE alias is expanded at the parse seam, a SAME-MODULE alias in another
/// file is expanded through the module's alias map, and a CLASSPATH alias goes through the
/// dependency's own index. This locks the third — a consumer naming a dependency's alias by its
/// fully qualified spelling — and does it at runtime, because a descriptor naming a class that is
/// never emitted only fails at load time.
#[test]
fn a_qualified_classpath_alias_resolves_to_its_target() {
    const LIB: &str = "package dep\n\
        \n\
        class Payload(val v: Int)\n\
        typealias Cargo = Payload\n";
    common::Fixture::new().lib("Lib.kt", LIB).assert_box_ok(
        "import dep.Payload\n\
         fun take(x: dep.Cargo): dep.Cargo = x\n\
         fun box(): String = if (take(Payload(7)).v == 7) \"OK\" else \"fail\"\n",
    );
}

/// A GENERIC class takes its supertype list from its recorded signature, which is shaped
/// differently from a non-generic class's — it always leads with the superclass position. This
/// covers the generic path end to end; the ALIGNMENT itself, including the case where that leading
/// position holds an undeclared `kotlin/Any`, is pinned by the unit tests on
/// `DeclaredSpellings::supertype_spellings` (a generic class with no declared superclass cannot be
/// asserted byte-for-byte yet: krusty emits that implicit `kotlin/Any` as an explicit supertype
/// where kotlinc omits it, with or without an alias).
#[test]
fn a_generic_class_abbreviates_the_supertype_that_was_spelled() {
    const SRC: &str = "package app\n\
        \n\
        open class Base\n\
        typealias Super = Base\n\
        \n\
        class Holder<T>(val t: T) : Super()\n";
    assert_identical("GSuper", SRC, "app/Holder");
}

/// A GENERIC class with NO declared superclass, implementing an alias-spelled interface. The
/// emitted supertype list must hold the interface alone — the implicit `kotlin/Any` that fills the
/// signature's superclass position is a signature artifact, not a metadata supertype — and the
/// interface's abbreviation must land on it rather than one slot over.
#[test]
fn a_generic_class_with_no_superclass_lists_only_its_interfaces() {
    const SRC: &str = "package app\n\
        \n\
        interface Iface\n\
        typealias Marker = Iface\n\
        \n\
        class Holder<T>(val t: T) : Marker\n";
    assert_identical("GAny", SRC, "app/Holder");
}

/// An alias spelled INSIDE an inline function type. The arrow node names no alias itself, but its
/// components do, and a function type's metadata arguments are synthesized (`params… + ret`) rather
/// than taken from the spelling tree — so the component spellings have to be laid out in that same
/// synthesized order.
#[test]
fn aliases_inside_an_inline_function_type_are_abbreviated() {
    let src = format!("{PRELUDE}\nfun hof(f: (Cargo) -> Cargo): Int = 1\n");
    assert_identical("Arrow", &src, "app/ArrowKt");
}

/// A `vararg` parameter is SPELLED as its element (`vararg xs: Cargo`) but RECORDED as
/// `Array<out Cargo>`, so the element's spelling is lifted under the array rather than applied to
/// it — and the array argument carries an `out` projection the source never wrote.
#[test]
fn a_vararg_of_an_aliased_element_abbreviates_its_element() {
    let src = format!("{PRELUDE}\nfun many(vararg xs: Cargo): Int = xs.size\n");
    assert_identical("Many", &src, "app/ManyKt");
}

/// The same declaration must also RUN.
#[test]
fn a_vararg_of_an_aliased_element_still_compiles_and_runs() {
    let src = format!("{PRELUDE}\nfun many(vararg xs: Cargo): Int = xs.size\n");
    common::Fixture::new().lib("Lib.kt", &src).assert_box_ok(
        "import app.Payload\n\
         import app.many\n\
         fun box(): String = if (many(Payload(1), Payload(2)) == 2) \"OK\" else \"fail\"\n",
    );
}

/// A CLASSPATH alias's own right-hand-side spellings, inherited by a consumer.
///
/// `typealias CargoBox = PBox<Cargo, Cargo>` declared in a DEPENDENCY abbreviates both expanded
/// arguments as `Cargo` at every use site, and `typealias Boxed<T> = PBox<T, T>` used as
/// `Boxed<Cargo>` does too — the first because the right-hand side spelled the alias, the second
/// because the use site did and the spelling reaches the expansion through the alias's parameters.
/// Neither is recoverable from the expansion `Ty` alone: it is fully expanded, so the spellings
/// come from `Type.abbreviated_type` read back out of the dependency's own metadata.
#[test]
fn a_classpath_alias_propagates_its_right_hand_side_spellings() {
    const LIB: &str = "package dep\n\
        \n\
        class Payload(val v: Int)\n\
        class PBox<A, B>(val a: A, val b: B)\n\
        typealias Cargo = Payload\n\
        typealias CargoBox = PBox<Cargo, Cargo>\n\
        typealias Boxed<T> = PBox<T, T>\n";
    const SRC: &str = "package app\n\
        \n\
        import dep.Boxed\n\
        import dep.Cargo\n\
        import dep.CargoBox\n\
        \n\
        fun carry(x: CargoBox): CargoBox = x\n\
        \n\
        fun boxed(x: Boxed<Cargo>): Boxed<Cargo> = x\n";
    let Some(result) = common::metadata_diff_against_kotlinc_lib(
        "DepRhs",
        &[("Lib.kt", LIB)],
        SRC,
        "app/DepRhsKt",
    ) else {
        eprintln!("skip (provisioned kotlinc unavailable)");
        return;
    };
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

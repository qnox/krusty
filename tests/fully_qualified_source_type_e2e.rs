mod common;

#[test]
fn fully_qualified_cross_file_supertype_resolves() {
    let current = "\
package sample.first

interface Contract {
    fun accept(value: Int)
}
";
    let compatibility = "\
package sample.compat

interface Contract : sample.first.Contract {
    val marker: String
    override fun accept(value: Int)
}

fun widen(value: Contract): sample.first.Contract = value
fun read(value: Contract): String = value.marker
";

    common::expect_front_end_ok_files_with_stdlib(
        &[compatibility, current],
        "FullyQualifiedCrossFileSupertype",
    );
}

#[test]
fn cross_package_homonyms_lower_with_their_own_signatures() {
    let compatibility = "\
package sample.compat

class Record(val text: String)

fun make(): Record = Record(\"legacy\")
fun read(value: Any): String = if (value is Record) value.text else \"missing\"
fun factory(): (String) -> Record = ::Record
";
    let current = "\
package sample.first

class Record(val number: Int)
";
    let entry = "\
import sample.compat.make
import sample.compat.read
import sample.compat.factory

fun box(): String =
    if (read(make()) == \"legacy\" && factory()(\"legacy\").text == \"legacy\") \"OK\" else \"FAIL\"
";

    common::expect_front_end_ok_files_with_stdlib(
        &[compatibility, current, entry],
        "CrossPackageHomonymFrontend",
    );
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Compatibility", compatibility),
            ("Current", current),
            ("Entry", entry),
        ],
        "CrossPackageHomonymLowering",
    );
}

#[test]
fn cross_package_static_names_use_visible_identity() {
    let first = "\
package sample.first

enum class State { READY }

object Holder {
    val text: String = \"first\"
    fun value(): String = text
}

class Service {
    companion object {
        fun value(): String = \"first\"
    }
}

fun verify(): String =
    if (State.values()[0] == State.READY &&
        Holder.text == \"first\" &&
        Holder.value() == \"first\" &&
        Service.value() == \"first\"
    ) \"OK\" else \"FAIL\"
";
    let second = "\
package sample.second

enum class State { OTHER }

object Holder {
    val number: Int = 2
    fun value(number: Int): Int = number
}

class Service {
    companion object {
        fun value(number: Int): Int = number
    }
}
";
    let entry = "\
import sample.first.verify

fun box(): String = verify()
";

    common::expect_front_end_ok_files_with_stdlib(
        &[first, second, entry],
        "CrossPackageStaticIdentityFrontend",
    );
    common::expect_box_ok_files_with_stdlib(
        &[("First", first), ("Second", second), ("Entry", entry)],
        "CrossPackageStaticIdentityLowering",
    );
}

#[test]
fn bare_source_type_does_not_leak_from_another_package() {
    let first = "\
package sample.first

class Record(val number: Int)
";
    let second = "\
package sample.second

class Record(val text: String)
";
    let alias = "\
package sample.alias

typealias Label = String
";
    let unrelated = "\
package sample.unrelated

fun create(): Any = Record(1)
fun createLabel(): Any = Label()
";

    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let diagnostics = common::front_end_diagnostics_files(
        &[first, second, alias, unrelated],
        std::slice::from_ref(&stdlib),
        Some(&jdk),
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("unresolved function 'Record'")),
        "expected the unrelated bare constructor to remain unresolved: {diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("unresolved function 'Label'")),
        "expected the unimported source alias to remain unresolved: {diagnostics:?}"
    );
}

#[test]
fn cross_file_source_alias_follows_package_and_import_scope() {
    let model = "\
package sample.model

class Payload(val text: String)
class OtherPayload(val number: Int)
";
    let alias = "\
package sample.alias

import sample.model.Payload

typealias Entry = Payload
";
    let homonymous_alias = "\
package sample.other

import sample.model.OtherPayload

typealias Entry = OtherPayload
";
    let same_package_use = "\
package sample.alias

fun make(): Entry = Entry(\"legacy\")
";
    let imported_use = "\
package sample.consumer

import sample.alias.Entry

fun read(value: Entry): String = value.text
";
    let entry = "\
import sample.alias.make
import sample.consumer.read

fun box(): String = if (read(make()) == \"legacy\") \"OK\" else \"FAIL\"
";

    common::expect_front_end_ok_files_with_stdlib(
        &[
            model,
            alias,
            homonymous_alias,
            same_package_use,
            imported_use,
            entry,
        ],
        "CrossFileSourceAliasFrontend",
    );
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Model", model),
            ("Alias", alias),
            ("HomonymousAlias", homonymous_alias),
            ("SamePackageUse", same_package_use),
            ("ImportedUse", imported_use),
            ("Entry", entry),
        ],
        "CrossFileSourceAliasLowering",
    );
}

#[test]
fn inner_class_uses_its_own_homonymous_outer() {
    let first = "\
package sample.first

class Container<T>(val value: T) {
    inner class Nested {
        fun read(): T = value
    }

    fun nested(): Nested = Nested()
}

fun verify(): String = Container(\"OK\").nested().read()
";
    let second = "\
package sample.second

class Container(val number: Int) {
    inner class Nested {
        fun read(): Int = number
    }
}
";
    let entry = "\
import sample.first.verify

fun box(): String = verify()
";

    common::expect_front_end_ok_files_with_stdlib(
        &[first, second, entry],
        "HomonymousInnerOuterFrontend",
    );
    common::expect_box_ok_files_with_stdlib(
        &[("First", first), ("Second", second), ("Entry", entry)],
        "HomonymousInnerOuterLowering",
    );
}

#[test]
fn inherited_member_extension_uses_declaration_scope() {
    let declarations = "\
package sample.declarations

class Payload(val text: String)
private typealias Entry = Payload

open class Host {
    fun Entry.combine(other: Entry): String = text + other.text
}
";
    let use_site = "\
package sample.consumer

import sample.declarations.Host
import sample.declarations.Payload

class Client : Host() {
    fun verify(): String = Payload(\"O\").combine(Payload(\"K\"))
}
";
    let entry = "\
import sample.consumer.Client

fun box(): String = Client().verify()
";

    common::expect_front_end_ok_files_with_stdlib(
        &[declarations, use_site, entry],
        "MemberExtensionDeclarationScopeFrontend",
    );
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Declarations", declarations),
            ("UseSite", use_site),
            ("Entry", entry),
        ],
        "MemberExtensionDeclarationScopeLowering",
    );
}

#[test]
fn explicit_import_wins_inside_same_package_class() {
    let local = "\
package sample.local

class Peer(val number: Int)
";
    let imported = "\
package sample.imported

class Peer(val text: String)
";
    let use_site = "\
package sample.local

import sample.imported.Peer

class Consumer(val value: Peer)

fun verify(): String = Consumer(Peer(\"OK\")).value.text
";
    let entry = "\
import sample.local.verify

fun box(): String = verify()
";

    common::expect_front_end_ok_files_with_stdlib(
        &[local, imported, use_site, entry],
        "ExplicitImportClassScopeFrontend",
    );
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Local", local),
            ("Imported", imported),
            ("UseSite", use_site),
            ("Entry", entry),
        ],
        "ExplicitImportClassScopeLowering",
    );
}

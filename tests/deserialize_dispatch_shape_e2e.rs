//! How a generated `deserialize` dispatches on the element index.
//!
//! krusty compiled the dispatch to a chain of `if (index == k)` comparisons with no final case, so
//! an index naming no element of the descriptor fell through every arm and the loop simply went
//! round again — forever, if the decoder kept returning it. kotlinc emits one `tableswitch` whose
//! default throws `UnknownFieldException`.
//!
//! What a format does with an unknown field is the format's decision, and `Json` screens unknown
//! KEYS before the serializer sees an index. These tests pin that both compilers make the same
//! decision; the switch's own default arm is pinned structurally, with the rest of the dispatch.
use super::serialization_test_support::both_compilers_box;

/// A JSON object with a field the class does not declare. Both compilers must agree on rejecting it.
///
/// The `Json` decoder screens unknown KEYS before the generated `deserialize` ever sees an index,
/// so this does not reach the switch's default arm — it pins the behaviour a reader cares about
/// while the arm itself is pinned structurally beside the rest of the dispatch.
#[test]
fn an_undeclared_field_is_rejected_by_both_compilers() {
    let src = "import kotlinx.serialization.Serializable\n\
               import kotlinx.serialization.json.Json\n\
               \n\
               @Serializable\n\
               data class Point(val x: Int, val y: Int)\n\
               \n\
               fun box(): String {\n\
               \x20   return try {\n\
               \x20       Json.decodeFromString(Point.serializer(), \"\"\"{\"x\":1,\"y\":2,\"z\":3}\"\"\").toString()\n\
               \x20   } catch (e: Throwable) {\n\
               \x20       e.toString()\n\
               \x20   }\n\
               }\n";
    let outcome = both_compilers_box(src, "unknown_field");
    assert!(
        outcome.contains("unknown key"),
        "an undeclared field must be reported, not ignored: {outcome}"
    );
}

/// The declared fields still decode, in either order, so the dispatch is a switch and not a switch
/// that happens to reject everything.
#[test]
fn declared_fields_decode_in_any_order() {
    let src = "import kotlinx.serialization.Serializable\n\
               import kotlinx.serialization.json.Json\n\
               \n\
               @Serializable\n\
               data class Point(val x: Int, val y: Int)\n\
               \n\
               fun box(): String {\n\
               \x20   val forwards = Json.decodeFromString(Point.serializer(), \"\"\"{\"x\":1,\"y\":2}\"\"\")\n\
               \x20   val backwards = Json.decodeFromString(Point.serializer(), \"\"\"{\"y\":2,\"x\":1}\"\"\")\n\
               \x20   return \"$forwards $backwards\"\n\
               }\n";
    both_compilers_box(src, "either_order");
}

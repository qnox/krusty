fun format(
    label: String,
    value: Int,
) = when {
    value < 0 -> "$label: negative"
    else -> "$label: $value"
}

fun tight(): Int? = 1

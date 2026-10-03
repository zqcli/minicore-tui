//! Tiny UTF-8 byte-cursor operations shared by the single-line panel fields.
//! The owning panel keeps its focus, limits, request generation and errors.

pub(crate) fn boundary(text: &str, mut cursor: usize) -> usize {
    cursor = cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    cursor
}

pub(crate) fn move_cursor(text: &str, cursor: &mut usize, delta: i32) {
    *cursor = boundary(text, *cursor);
    match delta.cmp(&0) {
        std::cmp::Ordering::Less => {
            *cursor = text[..*cursor]
                .char_indices()
                .next_back()
                .map_or(0, |(i, _)| i);
        }
        std::cmp::Ordering::Greater => {
            *cursor += text[*cursor..].chars().next().map_or(0, char::len_utf8);
        }
        std::cmp::Ordering::Equal => {}
    }
}

pub(crate) fn insert(
    text: &mut String,
    cursor: &mut usize,
    value: &str,
    limit: usize,
) -> Result<bool, &'static str> {
    // Tabs remain literal (including JSON whitespace in Grep paths). Never
    // silently join pasted lines or inject terminal control sequences.
    if value.chars().any(|c| c.is_control() && c != '\t') {
        return Err("single-line input: paste rejected; remove line breaks/control characters");
    }
    if text.len().saturating_add(value.len()) > limit {
        return Err("single-line input: insertion exceeds this field's byte limit");
    }
    if value.is_empty() {
        return Ok(false);
    }
    *cursor = boundary(text, *cursor);
    text.insert_str(*cursor, value);
    *cursor += value.len();
    Ok(true)
}

pub(crate) fn backspace(text: &mut String, cursor: &mut usize) -> bool {
    *cursor = boundary(text, *cursor);
    let end = *cursor;
    move_cursor(text, cursor, -1);
    if end == *cursor {
        return false;
    }
    text.replace_range(*cursor..end, "");
    true
}

pub(crate) fn delete(text: &mut String, cursor: &mut usize) -> bool {
    *cursor = boundary(text, *cursor);
    let Some(c) = text[*cursor..].chars().next() else {
        return false;
    };
    text.replace_range(*cursor..*cursor + c.len_utf8(), "");
    true
}

//! Conversion between UTF-8 byte offsets (Typst) and character offsets (Emacs).

/// Return the number of chars in TEXT before byte offset BYTE.
///
/// BYTE is clamped to the text, and rounded down to a char boundary.
pub fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..floor_char_boundary(text, byte)].chars().count()
}

fn floor_char_boundary(text: &str, byte: usize) -> usize {
    let mut byte = byte.min(text.len());
    while !text.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_multibyte_text() {
        let text = "aé😀b";
        assert_eq!(byte_to_char(text, 0), 0);
        assert_eq!(byte_to_char(text, 1), 1);
        assert_eq!(byte_to_char(text, 3), 2);
        assert_eq!(byte_to_char(text, 7), 3);
        assert_eq!(byte_to_char(text, 8), 4);
    }

    #[test]
    fn clamps_offsets() {
        let text = "aé";
        // Byte 2 is inside "é".
        assert_eq!(byte_to_char(text, 2), 1);
        assert_eq!(byte_to_char(text, 100), 2);
    }
}

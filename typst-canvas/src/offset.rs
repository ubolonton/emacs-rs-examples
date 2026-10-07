//! Conversion between UTF-8 byte offsets (Typst) and character offsets (Emacs).

/// Return the number of chars in TEXT before byte offset BYTE.
///
/// BYTE is clamped to the text, and rounded down to a char boundary.
pub fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..floor_char_boundary(text, byte)].chars().count()
}

/// Return the byte offset of char offset CHAR in TEXT, clamped to the end of TEXT.
pub fn char_to_byte(text: &str, char: usize) -> usize {
    text.char_indices()
        .nth(char)
        .map_or(text.len(), |(byte, _)| byte)
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
    fn converts_chars_to_bytes() {
        let text = "aé😀b";
        assert_eq!(char_to_byte(text, 0), 0);
        assert_eq!(char_to_byte(text, 2), 3);
        assert_eq!(char_to_byte(text, 3), 7);
        for char in 0..=4 {
            assert_eq!(byte_to_char(text, char_to_byte(text, char)), char);
        }
        assert_eq!(char_to_byte(text, 100), text.len());
    }

    #[test]
    fn clamps_offsets() {
        let text = "aé";
        // Byte 2 is inside "é".
        assert_eq!(byte_to_char(text, 2), 1);
        assert_eq!(byte_to_char(text, 100), 2);
    }
}

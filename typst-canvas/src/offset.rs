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

/// Return BYTE, clamped to TEXT, and rounded down to a char boundary.
pub fn floor_char_boundary(text: &str, byte: usize) -> usize {
    let mut byte = byte.min(text.len());
    while !text.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

/// Maps byte offsets between two versions of a text, an old one and a new one, from their common
/// prefix and suffix. Offsets in the prefix stay. Offsets in the suffix move by the length
/// difference. Offsets in the changed middle are clamped to the changed middle of the other text.
///
/// Positions in a document come from the text that it was compiled from, which is older than the
/// buffer text after a failed compile. Map them before they meet buffer positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextMap {
    /// Length of the common prefix.
    prefix: usize,
    /// End of the changed middle in the old text, and in the new text. The common suffix follows.
    old_end: usize,
    new_end: usize,
}

impl TextMap {
    pub fn new(old: &str, new: &str) -> Self {
        let (old_bytes, new_bytes) = (old.as_bytes(), new.as_bytes());
        let mut prefix = old_bytes
            .iter()
            .zip(new_bytes)
            .take_while(|(old, new)| old == new)
            .count();
        while !(old.is_char_boundary(prefix) && new.is_char_boundary(prefix)) {
            prefix -= 1;
        }
        // The suffix must not overlap the prefix in the shorter text.
        let mut suffix = old_bytes
            .iter()
            .rev()
            .zip(new_bytes.iter().rev())
            .take(old.len().min(new.len()) - prefix)
            .take_while(|(old, new)| old == new)
            .count();
        while !(old.is_char_boundary(old.len() - suffix)
            && new.is_char_boundary(new.len() - suffix))
        {
            suffix -= 1;
        }
        Self {
            prefix,
            old_end: old.len() - suffix,
            new_end: new.len() - suffix,
        }
    }

    /// Map byte offset BYTE of the old text to the new text. The result can be inside a char of
    /// the new text if BYTE is in the changed middle.
    pub fn forward(&self, byte: usize) -> usize {
        shift(byte, self.prefix, self.old_end, self.new_end)
    }

    /// Map byte offset BYTE of the new text to the old text. See `forward`.
    pub fn backward(&self, byte: usize) -> usize {
        shift(byte, self.prefix, self.new_end, self.old_end)
    }
}

/// Map BYTE from a text whose changed middle is PREFIX..FROM_END to one whose changed middle is
/// PREFIX..TO_END. An offset stands for the char after it, so at an insertion point (an empty
/// middle), it goes to the char after the inserted text.
fn shift(byte: usize, prefix: usize, from_end: usize, to_end: usize) -> usize {
    if byte >= from_end {
        byte - from_end + to_end
    } else if byte <= prefix {
        byte
    } else {
        byte.min(to_end)
    }
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
    fn text_map_keeps_prefix_and_shifts_suffix() {
        let old = "Hello world";
        let new = "Hello big world";
        let map = TextMap::new(old, new);
        assert_eq!(map.forward(2), 2);
        // "world" moved by the length of "big ".
        assert_eq!(map.forward(6), 10);
        assert_eq!(map.forward(old.len()), new.len());
        assert_eq!(map.backward(10), 6);
        // In the inserted text, the old text has no position but the insertion point.
        assert_eq!(map.backward(8), 6);
        assert_eq!(TextMap::new(old, old).forward(7), 7);
    }

    #[test]
    fn text_map_clamps_changed_middle() {
        // "abcdef" became "aXf": the middle "bcde" became "X".
        let map = TextMap::new("abcdef", "aXf");
        assert_eq!(map.forward(1), 1);
        assert_eq!(map.forward(2), 2);
        assert_eq!(map.forward(4), 2);
        assert_eq!(map.forward(5), 2);
        assert_eq!(map.backward(2), 5);
    }

    #[test]
    fn text_map_respects_char_boundaries() {
        let middle = |old, new| TextMap::new(old, new);
        let expected = TextMap {
            prefix: 1,
            old_end: 3,
            new_end: 3,
        };
        // "é" (C3 A9) and "è" (C3 A8) share their first byte: the prefix must not take it.
        assert_eq!(middle("aéb", "aèb"), expected);
        // "é" (C3 A9) and "©" (C2 A9) share their last byte: the suffix must not take it.
        assert_eq!(middle("aé", "a©"), expected);
    }

    #[test]
    fn clamps_offsets() {
        let text = "aé";
        // Byte 2 is inside "é".
        assert_eq!(byte_to_char(text, 2), 1);
        assert_eq!(byte_to_char(text, 100), 2);
    }
}

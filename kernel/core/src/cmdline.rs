//! The kernel command line: words separated by spaces.

/// Whether `word` is one of the words of `cmdline`.
pub fn has_word(cmdline: &[u8], word: &[u8]) -> bool {
    cmdline.split(|&b| b == b' ').any(|w| w == word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_whole_words() {
        assert!(has_word(b"quiet dma=strict", b"dma=strict"));
        assert!(has_word(b"dma=strict", b"dma=strict"));
        assert!(!has_word(b"dma=strictly", b"dma=strict"));
        assert!(!has_word(b"", b"dma=strict"));
    }
}

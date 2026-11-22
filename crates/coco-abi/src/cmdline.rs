//! Kernel command-line parsing: whitespace-separated `key` or `key=value` tokens.

/// Borrowed view over a validated command line.
#[derive(Clone, Copy, Debug)]
pub struct Cmdline<'a> {
    text: &'a str,
}

impl<'a> Cmdline<'a> {
    /// Wraps already-validated command-line text.
    #[must_use]
    pub const fn new(text: &'a str) -> Self {
        Self { text }
    }

    /// The raw text.
    #[must_use]
    pub const fn as_str(&self) -> &'a str {
        self.text
    }

    /// Iterates over `(key, value)` pairs; bare flags have value `None`.
    pub fn options(&self) -> impl Iterator<Item = (&'a str, Option<&'a str>)> + 'a {
        self.text.split_ascii_whitespace().map(|tok| match tok.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (tok, None),
        })
    }

    /// Value of the last occurrence of `key=value`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&'a str> {
        self.options().filter(|(k, _)| *k == key).filter_map(|(_, v)| v).last()
    }

    /// True if `key` appears as a bare flag or as `key=1|on|true|yes`.
    #[must_use]
    pub fn flag(&self, key: &str) -> bool {
        self.options().filter(|(k, _)| *k == key).last().is_some_and(|(_, v)| match v {
            None => true,
            Some(v) => matches!(v, "1" | "on" | "true" | "yes"),
        })
    }
}

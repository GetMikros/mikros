//! Reading JSON into values, for `from json`.

use super::Value;

/// The value of the JSON text `text`.
pub fn json(text: &str) -> Result<Value, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut reader = Reader {
        chars: &chars,
        pos: 0,
    };
    reader.blank();
    let value = reader.value()?;
    reader.blank();
    match reader.pos == chars.len() {
        true => Ok(value),
        false => Err(reader.error("text after the JSON value")),
    }
}

struct Reader<'a> {
    chars: &'a [char],
    pos: usize,
}

impl Reader<'_> {
    fn error(&self, what: &str) -> String {
        format!("not JSON: {what} at character {}", self.pos + 1)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn blank(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn word(&mut self, word: &str, value: Value) -> Result<Value, String> {
        let end = self.pos + word.len();
        match self.chars.get(self.pos..end) {
            Some(chars) if chars.iter().copied().eq(word.chars()) => {
                self.pos = end;
                Ok(value)
            }
            _ => Err(self.error("an unknown word")),
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some('{') => {
                self.pos += 1;
                let mut fields = Vec::new();
                self.blank();
                if self.peek() == Some('}') {
                    self.pos += 1;
                    return Ok(Value::Record(fields));
                }
                loop {
                    self.blank();
                    let key = self.string()?;
                    self.blank();
                    if self.peek() != Some(':') {
                        return Err(self.error("`:` expected"));
                    }
                    self.pos += 1;
                    self.blank();
                    fields.push((key, self.value()?));
                    self.blank();
                    match self.peek() {
                        Some(',') => self.pos += 1,
                        Some('}') => {
                            self.pos += 1;
                            return Ok(Value::Record(fields));
                        }
                        _ => return Err(self.error("`,` or `}` expected")),
                    }
                }
            }
            Some('[') => {
                self.pos += 1;
                let mut items = Vec::new();
                self.blank();
                if self.peek() == Some(']') {
                    self.pos += 1;
                    return Ok(Value::List(items));
                }
                loop {
                    self.blank();
                    items.push(self.value()?);
                    self.blank();
                    match self.peek() {
                        Some(',') => self.pos += 1,
                        Some(']') => {
                            self.pos += 1;
                            return Ok(Value::List(items));
                        }
                        _ => return Err(self.error("`,` or `]` expected")),
                    }
                }
            }
            Some('"') => self.string().map(Value::String),
            Some('t') => self.word("true", Value::Bool(true)),
            Some('f') => self.word("false", Value::Bool(false)),
            Some('n') => self.word("null", Value::Null),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            _ => Err(self.error("a value expected")),
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_digit() || "+-.eE".contains(c))
        {
            self.pos += 1;
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        if let Ok(n) = text.parse::<i64>() {
            return Ok(Value::Int(n));
        }
        text.parse::<f64>()
            .map(Value::Float)
            .map_err(|_| self.error("a bad number"))
    }

    fn string(&mut self) -> Result<String, String> {
        if self.peek() != Some('"') {
            return Err(self.error("a string expected"));
        }
        self.pos += 1;
        let mut text = String::new();
        loop {
            match self.peek() {
                None => return Err(self.error("an unterminated string")),
                Some('"') => {
                    self.pos += 1;
                    return Ok(text);
                }
                Some('\\') => {
                    self.pos += 1;
                    let c = match self.peek() {
                        Some('n') => '\n',
                        Some('t') => '\t',
                        Some('r') => '\r',
                        Some('b') => '\u{8}',
                        Some('f') => '\u{c}',
                        Some(c @ ('"' | '\\' | '/')) => c,
                        Some('u') => {
                            let hex: String = self
                                .chars
                                .get(self.pos + 1..self.pos + 5)
                                .unwrap_or_default()
                                .iter()
                                .collect();
                            let code = u32::from_str_radix(&hex, 16)
                                .map_err(|_| self.error("a bad escape"))?;
                            self.pos += 4;
                            char::from_u32(code).unwrap_or('\u{fffd}')
                        }
                        _ => return Err(self.error("a bad escape")),
                    };
                    text.push(c);
                    self.pos += 1;
                }
                Some(c) => {
                    text.push(c);
                    self.pos += 1;
                }
            }
        }
    }
}

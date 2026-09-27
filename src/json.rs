//! A JSON document as a value tree: there is no JSON library among the
//! dependencies, and both a `--config` file and the GitHub API that lists a
//! remote boot partition ([`crate::remote`]) are JSON.

use anyhow::{bail, Context, Result};

/// A parsed JSON or TOML value. Numbers keep their text: nothing here does
/// arithmetic on them.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<Value>),
    /// Keys in file order.
    Obj(Vec<(String, Value)>),
}

/// Parse a JSON document (RFC 8259) into a [`Value`]; there is no JSON library among the dependencies.
pub fn parse(text: &str) -> Result<Value> {
    let mut p = Json {
        s: text.as_bytes(),
        i: 0,
    };
    let v = p.value()?;
    p.ws();
    if p.i != p.s.len() {
        bail!("{}: unexpected text after the value", p.at());
    }
    Ok(v)
}

struct Json<'a> {
    s: &'a [u8],
    i: usize,
}

impl Json<'_> {
    fn at(&self) -> String {
        let line = self.s[..self.i].iter().filter(|&&c| c == b'\n').count() + 1;
        format!("line {line}")
    }

    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn eat(&mut self, lit: &str) -> bool {
        if self.s[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Result<Value> {
        self.ws();
        match self.s.get(self.i) {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ if self.eat("true") => Ok(Value::Bool(true)),
            _ if self.eat("false") => Ok(Value::Bool(false)),
            _ if self.eat("null") => Ok(Value::Null),
            _ => bail!("{}: expected a value", self.at()),
        }
    }

    fn object(&mut self) -> Result<Value> {
        self.i += 1;
        let mut entries = Vec::new();
        self.ws();
        if self.eat("}") {
            return Ok(Value::Obj(entries));
        }
        loop {
            self.ws();
            if self.s.get(self.i) != Some(&b'"') {
                bail!("{}: expected a key", self.at());
            }
            let key = self.string()?;
            self.ws();
            if !self.eat(":") {
                bail!("{}: expected ':' after \"{key}\"", self.at());
            }
            entries.push((key, self.value()?));
            self.ws();
            if self.eat("}") {
                return Ok(Value::Obj(entries));
            }
            if !self.eat(",") {
                bail!("{}: expected ',' or '}}'", self.at());
            }
        }
    }

    fn array(&mut self) -> Result<Value> {
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.eat("]") {
            return Ok(Value::Arr(items));
        }
        loop {
            items.push(self.value()?);
            self.ws();
            if self.eat("]") {
                return Ok(Value::Arr(items));
            }
            if !self.eat(",") {
                bail!("{}: expected ',' or ']'", self.at());
            }
        }
    }

    fn number(&mut self) -> Result<Value> {
        let start = self.i;
        while self
            .s
            .get(self.i)
            .is_some_and(|c| c.is_ascii_digit() || b"+-.eE".contains(c))
        {
            self.i += 1;
        }
        let text = std::str::from_utf8(&self.s[start..self.i])?;
        if text.parse::<f64>().is_err() {
            bail!("{}: bad number {text:?}", self.at());
        }
        Ok(Value::Num(text.to_string()))
    }

    fn hex4(&mut self) -> Result<u32> {
        let h = self
            .s
            .get(self.i..self.i + 4)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u32::from_str_radix(h, 16).ok())
            .with_context(|| format!("{}: bad \\u escape", self.at()))?;
        self.i += 4;
        Ok(h)
    }

    fn string(&mut self) -> Result<String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(&c) = self.s.get(self.i) else {
                bail!("{}: unterminated string", self.at());
            };
            self.i += 1;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let Some(&e) = self.s.get(self.i) else {
                        bail!("{}: unterminated string", self.at());
                    };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            // A surrogate pair is two escapes for one character.
                            if (0xD800..0xDC00).contains(&cp) && self.eat("\\u") {
                                let lo = self.hex4()?;
                                cp = 0x10000
                                    + ((cp - 0xD800) << 10)
                                    + (lo.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                        }
                        _ => bail!("{}: bad escape \\{}", self.at(), e as char),
                    }
                }
                _ => {
                    let start = self.i - 1;
                    while self.s.get(self.i).is_some_and(|&b| b & 0xC0 == 0x80) {
                        self.i += 1;
                    }
                    out.push_str(std::str::from_utf8(&self.s[start..self.i])?);
                }
            }
        }
    }
}

impl Value {
    /// The value of `key`, for an object; `None` for anything else.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_numbers_and_escapes() {
        let v = parse(r#"{"a": "x\né😀\"", "b": [-1.5e3, null], "c": {}}"#).unwrap();
        assert_eq!(
            v,
            Value::Obj(vec![
                ("a".into(), Value::Str("x\né😀\"".into())),
                (
                    "b".into(),
                    Value::Arr(vec![Value::Num("-1.5e3".into()), Value::Null])
                ),
                ("c".into(), Value::Obj(vec![])),
            ])
        );
        assert!(parse(r#"{"a": }"#).is_err());
        assert!(parse(r#"{"a": 1} x"#).is_err());
    }

    #[test]
    fn a_key_is_looked_up_on_an_object_only() {
        let v = parse(r#"{"name": "start4.elf", "size": 2}"#).unwrap();
        assert_eq!(v.get("name"), Some(&Value::Str("start4.elf".into())));
        assert_eq!(v.get("missing"), None);
        assert_eq!(Value::Num("1".into()).get("name"), None);
    }
}

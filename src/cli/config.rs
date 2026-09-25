//! Options from a file, and `--option=value`.
//!
//! `--config <file>` expands in place into the tokens the file stands for, so an
//! option after it on the command line wins and a repeatable one adds to it. The
//! file is a JSON object or a TOML table keyed by long option name without the
//! dashes (a one-letter key is the short form): `true` is a flag, a string or
//! number is the value, an array repeats the option, a nested array gives one
//! option several values, and `"file"` is the positional argument.

use anyhow::{bail, Context, Result};

/// A parsed JSON or TOML value. Numbers keep their text: they only ever become arguments again.
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

/// The command line with `--option=value` split in two and `--config <file>` expanded.
pub fn expand(args: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(args.len());
    let mut it = split_equals(args).into_iter();
    while let Some(a) = it.next() {
        if a != "--config" {
            out.push(a);
            continue;
        }
        let path = it.next().context("--config needs a file")?;
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("reading config {path}"))?;
        let value = parse(&text).with_context(|| format!("parsing config {path}"))?;
        out.extend(to_args(&value).with_context(|| format!("config {path}"))?);
    }
    Ok(out)
}

/// Long options only, split at the first `=`: `--bootconf=HTTP_HOST=x` keeps the second.
fn split_equals(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        match a.strip_prefix("--").and_then(|r| r.split_once('=')) {
            Some((name, value)) if !name.is_empty() => {
                out.push(format!("--{name}"));
                out.push(value.to_string());
            }
            _ => out.push(a.clone()),
        }
    }
    out
}

fn parse(text: &str) -> Result<Value> {
    match parse_json(text) {
        Ok(v) => Ok(v),
        Err(json) => match toml::from_str::<toml::Table>(text) {
            Ok(t) => Ok(from_toml(toml::Value::Table(t))),
            Err(_) => Err(json),
        },
    }
}

fn from_toml(v: toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::Str(s),
        toml::Value::Integer(i) => Value::Num(i.to_string()),
        toml::Value::Float(f) => Value::Num(f.to_string()),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(d) => Value::Str(d.to_string()),
        toml::Value::Array(a) => Value::Arr(a.into_iter().map(from_toml).collect()),
        toml::Value::Table(t) => {
            Value::Obj(t.into_iter().map(|(k, v)| (k, from_toml(v))).collect())
        }
    }
}

fn to_args(v: &Value) -> Result<Vec<String>> {
    let Value::Obj(entries) = v else {
        bail!("the file must hold one object of options");
    };
    let mut out = Vec::new();
    for (key, value) in entries {
        if key == "config" {
            bail!("a config file cannot name another one");
        }
        if key == "file" {
            out.push(scalar(key, value)?);
            continue;
        }
        let flag = if key.chars().count() == 1 {
            format!("-{key}")
        } else {
            format!("--{key}")
        };
        match value {
            Value::Arr(items) => {
                for item in items {
                    push_option(&mut out, &flag, key, item)?;
                }
            }
            other => push_option(&mut out, &flag, key, other)?,
        }
    }
    Ok(out)
}

fn push_option(out: &mut Vec<String>, flag: &str, key: &str, v: &Value) -> Result<()> {
    match v {
        Value::Null | Value::Bool(false) => {}
        Value::Bool(true) => out.push(flag.to_string()),
        Value::Arr(values) => {
            out.push(flag.to_string());
            for v in values {
                out.push(scalar(key, v)?);
            }
        }
        other => {
            out.push(flag.to_string());
            out.push(scalar(key, other)?);
        }
    }
    Ok(())
}

fn scalar(key: &str, v: &Value) -> Result<String> {
    match v {
        Value::Str(s) | Value::Num(s) => Ok(s.clone()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => bail!("\"{key}\": expected a string, a number or a boolean"),
    }
}

/// Parse a JSON document (RFC 8259) into a [`Value`]; there is no JSON library among the dependencies.
pub fn parse_json(text: &str) -> Result<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn expand_with(config: &str, cmdline: &[&str]) -> Vec<String> {
        let dir = std::env::temp_dir().join(format!("pimu-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("c{}", config.len()));
        std::fs::write(&path, config).unwrap();
        let mut v = args(cmdline);
        v.push(format!("--config={}", path.display()));
        expand(&v).unwrap()
    }

    #[test]
    fn equals_splits_at_the_first_one_only() {
        assert_eq!(
            expand(&args(&[
                "boot",
                "--max-wall=600",
                "--bootconf=HTTP_HOST=x",
                "-v"
            ]))
            .unwrap(),
            args(&[
                "boot",
                "--max-wall",
                "600",
                "--bootconf",
                "HTTP_HOST=x",
                "-v"
            ])
        );
    }

    #[test]
    fn a_json_config_expands_in_place() {
        let got = expand_with(
            r#"{"eeprom": "fw/pieeprom.bin", "max-wall": 600, "stdin": true, "verbose": false,
                "bootconf": ["A=1", "B=2"], "send-after": [["/ # ", "uname\n"]], "v": true}"#,
            &["boot"],
        );
        assert_eq!(
            got,
            args(&[
                "boot",
                "--eeprom",
                "fw/pieeprom.bin",
                "--max-wall",
                "600",
                "--stdin",
                "--bootconf",
                "A=1",
                "--bootconf",
                "B=2",
                "--send-after",
                "/ # ",
                "uname\n",
                "-v",
            ])
        );
    }

    #[test]
    fn a_toml_config_works_too() {
        let got = expand_with("file = \"s.toml\"\nupdate = true\n", &["run"]);
        assert_eq!(got, args(&["run", "s.toml", "--update"]));
    }

    #[test]
    fn json_strings_numbers_and_escapes() {
        let v = parse_json(r#"{"a": "x\né😀\"", "b": [-1.5e3, null], "c": {}}"#).unwrap();
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
        assert!(parse_json(r#"{"a": }"#).is_err());
        assert!(parse_json(r#"{"a": 1} x"#).is_err());
    }

    #[test]
    fn nested_objects_are_refused() {
        assert!(to_args(&parse_json(r#"{"a": {"b": 1}}"#).unwrap()).is_err());
        assert!(to_args(&parse_json(r#"[1]"#).unwrap()).is_err());
    }
}

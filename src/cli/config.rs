//! Options from a file, and `--option=value`.
//!
//! `--config <file>` expands in place into the tokens the file stands for, so an
//! option after it on the command line wins and a repeatable one adds to it. The
//! file is a JSON or YAML object keyed by long option name without the
//! dashes (a one-letter key is the short form): `true` is a flag, a string or
//! number is the value, an array repeats the option, a nested array gives one
//! option several values, and `"file"` is the positional argument.

use anyhow::{bail, Context, Result};

pub use pimu::json::{parse as parse_json, Value};

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
    parse_json(text).or_else(|json| match yaml_serde::from_str(text) {
        Ok(v) => Ok(from_yaml(v)),
        Err(_) => Err(json),
    })
}

fn from_yaml(v: yaml_serde::Value) -> Value {
    use yaml_serde::Value as Y;
    match v {
        Y::Null => Value::Null,
        Y::Bool(b) => Value::Bool(b),
        Y::Number(n) => Value::Num(n.to_string()),
        Y::String(s) => Value::Str(s),
        Y::Sequence(a) => Value::Arr(a.into_iter().map(from_yaml).collect()),
        Y::Mapping(m) => Value::Obj(
            m.into_iter()
                .map(|(k, v)| (key_name(k), from_yaml(v)))
                .collect(),
        ),
        Y::Tagged(t) => from_yaml(t.value),
    }
}

fn key_name(k: yaml_serde::Value) -> String {
    match k {
        yaml_serde::Value::String(s) => s,
        other => yaml_serde::to_string(&other)
            .unwrap_or_default()
            .trim()
            .to_string(),
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
    fn a_yaml_config_expands_like_the_json_one() {
        let got = expand_with(
            "# the firmware under test\neeprom: fw/pieeprom.bin\nmax-wall: 600\nstdin: true\n\
             verbose: false\nbootconf:\n  - A=1\n  - B=2\nsend-after:\n  - [\"/ # \", \"uname\\n\"]\nv: true\n",
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
    fn the_example_in_running_md_expands_as_documented() {
        let doc = include_str!("../../docs/running.md");
        let yaml = doc
            .split("# machine.yaml\n")
            .nth(1)
            .and_then(|r| r.split("```").next())
            .expect("the --config example");
        let got = expand_with(yaml, &["boot", "--max-wall", "900"]);
        assert_eq!(
            got,
            args(&[
                "boot",
                "--max-wall",
                "900",
                "--eeprom",
                "https://example.org/pieeprom.bin",
                "--usb",
                "https://example.org/disk.img",
                "--boot-order",
                "0x5",
                "--max-wall",
                "600",
                "--bootconf",
                "HTTP_HOST=boot.example.org",
                "--bootconf",
                "HTTP_PORT=80",
                "--send-after",
                "/ # ",
                "uname -a\n",
                "--stdin",
                "-v",
            ])
        );
    }

    #[test]
    fn nested_objects_are_refused() {
        assert!(to_args(&parse_json(r#"{"a": {"b": 1}}"#).unwrap()).is_err());
        assert!(to_args(&parse_json(r#"[1]"#).unwrap()).is_err());
    }
}

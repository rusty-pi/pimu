//! `--config <file>`: a command's options from a file.
//!
//! The file expands in place into the tokens it stands for, so an option after
//! it on the command line wins and a repeatable one adds to it. It is a JSON or
//! YAML object keyed by long option name without the dashes (a one-letter key
//! is the short form), checked against what the command declares: `true` is a
//! flag, a string or number is the value, an array repeats the option, a nested
//! array gives one option several values, and `"file"` is the positional
//! argument.

use anyhow::{bail, Context, Result};
use clap::{Arg, ArgAction, Command};

pub use pimu::json::{parse as parse_json, Value};

/// The command line of `cmd` with every `--config <file>` expanded.
pub fn expand(cmd: &mut Command, args: &[String]) -> Result<Vec<String>> {
    cmd.build();
    let mut out = Vec::with_capacity(args.len());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let path = match a.strip_prefix("--config") {
            Some("") => it.next().context("--config needs a file")?,
            Some(tail) if tail.starts_with('=') => &tail[1..],
            _ => {
                out.push(a.clone());
                continue;
            }
        };
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading config {path}"))?;
        let value = parse(&text).with_context(|| format!("parsing config {path}"))?;
        out.extend(to_args(cmd, &value).with_context(|| format!("config {path}"))?);
    }
    Ok(out)
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

fn to_args(cmd: &Command, v: &Value) -> Result<Vec<String>> {
    let Value::Obj(entries) = v else {
        bail!("the file must hold one object of options");
    };
    let mut out = Vec::new();
    for (key, value) in entries {
        if key == "config" {
            bail!("a config file cannot name another one");
        }
        let arg = find(cmd, key)?;
        match (arg.get_long(), value) {
            (None, _) => out.push(scalar(key, value)?),
            (Some(long), Value::Arr(items)) if takes_values(arg) => {
                if !matches!(arg.get_action(), ArgAction::Append) {
                    bail!("\"{key}\" takes one value, not a list");
                }
                for item in items {
                    push_option(&mut out, long, key, item)?;
                }
            }
            (Some(long), value) if takes_values(arg) => push_option(&mut out, long, key, value)?,
            (Some(long), Value::Bool(true)) => out.push(format!("--{long}")),
            (Some(_), Value::Bool(false) | Value::Null) => {}
            (Some(_), _) => bail!("\"{key}\" is a flag: true or false"),
        }
    }
    Ok(out)
}

/// The option `key` names: its long name, a one-letter short one, or `"file"`
/// for the positional argument.
fn find<'a>(cmd: &'a Command, key: &str) -> Result<&'a Arg> {
    let mut chars = key.chars();
    let short = chars.next().filter(|_| chars.next().is_none());
    cmd.get_arguments()
        .find(|a| match key {
            "file" => a.is_positional(),
            _ => a.get_long() == Some(key) || (short.is_some() && a.get_short() == short),
        })
        .with_context(|| format!("\"{key}\": `{}` has no such option", cmd.get_name()))
}

fn takes_values(arg: &Arg) -> bool {
    arg.get_num_args().is_some_and(|n| n.takes_values())
}

fn push_option(out: &mut Vec<String>, long: &str, key: &str, v: &Value) -> Result<()> {
    match v {
        Value::Null | Value::Bool(false) => {}
        Value::Arr(values) => {
            out.push(format!("--{long}"));
            for v in values {
                out.push(scalar(key, v)?);
            }
        }
        Value::Bool(true) => bail!("\"{key}\" needs a value, not true"),
        other => out.push(format!("--{long}={}", scalar(key, other)?)),
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
    use clap::CommandFactory;

    use super::*;
    use crate::boot_args::BootArgs;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn try_expand_with(config: &str, cmdline: &[&str]) -> Result<Vec<String>> {
        let dir = std::env::temp_dir().join(format!("pimu-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("c{:x}", config.len() ^ config.as_ptr() as usize));
        std::fs::write(&path, config).unwrap();
        let mut v = args(cmdline);
        v.push(format!("--config={}", path.display()));
        expand(&mut BootArgs::command(), &v)
    }

    fn expand_with(config: &str, cmdline: &[&str]) -> Vec<String> {
        try_expand_with(config, cmdline).unwrap()
    }

    const EXPANDED: &[&str] = &[
        "--eeprom=fw/pieeprom.bin",
        "--max-wall=600",
        "--stdin",
        "--bootconf=A=1",
        "--bootconf=B=2",
        "--send-after",
        "/ # ",
        "uname\n",
        "--verbose",
    ];

    #[test]
    fn a_json_config_expands_in_place() {
        let got = expand_with(
            r#"{"eeprom": "fw/pieeprom.bin", "max-wall": 600, "stdin": true, "verbose": false,
                "bootconf": ["A=1", "B=2"], "send-after": [["/ # ", "uname\n"]], "v": true}"#,
            &[],
        );
        assert_eq!(got, args(EXPANDED));
    }

    #[test]
    fn a_yaml_config_expands_like_the_json_one() {
        let got = expand_with(
            "# the firmware under test\neeprom: fw/pieeprom.bin\nmax-wall: 600\nstdin: true\n\
             verbose: false\nbootconf:\n  - A=1\n  - B=2\nsend-after:\n  - [\"/ # \", \"uname\\n\"]\nv: true\n",
            &[],
        );
        assert_eq!(got, args(EXPANDED));
    }

    #[test]
    fn the_example_in_running_md_expands_as_documented() {
        let doc = include_str!("../../docs/running.md");
        let yaml = doc
            .split("# machine.yaml\n")
            .nth(1)
            .and_then(|r| r.split("```").next())
            .expect("the --config example");
        let got = expand_with(yaml, &["--max-wall", "900"]);
        assert_eq!(
            got,
            args(&[
                "--max-wall",
                "900",
                "--eeprom=https://example.org/pieeprom.bin",
                "--usb=https://example.org/disk.img",
                "--boot-order=0x5",
                "--max-wall=600",
                "--bootconf=HTTP_HOST=boot.example.org",
                "--bootconf=HTTP_PORT=80",
                "--send-after",
                "/ # ",
                "uname -a\n",
                "--stdin",
                "--verbose",
            ])
        );
    }

    #[test]
    fn a_key_is_checked_against_what_the_command_declares() {
        for (config, says) in [
            (r#"{"no-such-option": 1}"#, "no such option"),
            (r#"{"quiet": "yes"}"#, "is a flag"),
            (r#"{"eeprom": true}"#, "needs a value"),
            (r#"{"eeprom": ["a.bin", "b.bin"]}"#, "not a list"),
            (r#"{"a": {"b": 1}}"#, "no such option"),
            (r#"[1]"#, "one object"),
        ] {
            let err = format!("{:#}", try_expand_with(config, &[]).unwrap_err());
            assert!(err.contains(says), "{config}: {err}");
        }
    }

    #[test]
    fn file_is_the_positional_argument() {
        assert_eq!(
            expand_with(r#"{"file": "fw/", "eeprom": "e.bin"}"#, &[]),
            args(&["fw/", "--eeprom=e.bin"])
        );
    }
}

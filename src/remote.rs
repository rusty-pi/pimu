//! A boot partition served over HTTP: the card's files come from a URL rather
//! than a host directory, so nothing has to be cloned or bind-mounted first.
//!
//! Only the listing — every name and length — is read up front, since that is
//! what the FAT32 volume around them is built from ([`crate::fat`]); a file's
//! bytes are fetched when the firmware first reads a block of it, and kept
//! under `$XDG_CACHE_HOME/pimu/remote` so the next run and the next container
//! start from the cache. `raspberrypi/firmware`'s `boot/` is 150 MB of which a
//! boot reads a tenth.
//!
//! A whole disk image over HTTP (`boot --sd <url>`) is read the same way, but
//! in 1 MiB chunks through `Range` requests, since an image is gigabytes and a
//! boot reads a fraction of one.
//!
//! The transfer itself is `curl`, or `wget` where there is none: a TLS stack is
//! a large dependency for one GET, and both are everywhere the binary runs. A
//! server that indexes the directory itself costs a HEAD per file to size it,
//! where the GitHub API gives every size with the listing.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use sha1::{Digest, Sha1};

use crate::fat::{Entry, Kind, Source};

/// An argument that names a URL rather than a path.
pub fn is_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

/// Where fetched files and the images `boot` downloads are kept.
pub fn cache_dir() -> Result<PathBuf> {
    Ok(std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .context("no XDG_CACHE_HOME and no HOME to cache downloads under")?
        .join("pimu"))
}

/// The card's tree under `base`, as [`crate::fat`] takes it. A GitHub URL is
/// listed through the API, since `raw.githubusercontent.com` serves files and
/// not directories; anything else has to serve an index of its own.
pub fn listing(base: &str) -> Result<Vec<Entry>> {
    match GitHub::parse(base) {
        Some(gh) => gh.listing(&gh.path),
        None => index_listing(&with_slash(base)),
    }
}

/// `url`'s body, `len` bytes of it, from the cache when it is already there.
///
/// The cache name carries the URL's digest, so two files of the same name on
/// different hosts are different files, and the length is checked: a moving
/// branch serves new bytes under the URL it served the old ones under.
pub fn fetch(url: &str, len: u64) -> Result<Vec<u8>> {
    let dir = cache_dir()?.join("remote");
    let path = dir.join(cache_name(url));
    if path.metadata().is_ok_and(|m| m.len() == len) {
        return std::fs::read(&path).with_context(|| format!("reading {}", path.display()));
    }
    let body = get(url)?;
    if body.len() as u64 != len {
        bail!("{url}: {} bytes, expected {len}", body.len());
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(&path, &body).with_context(|| format!("writing {}", path.display()))?;
    Ok(body)
}

/// What a HEAD says about a URL: how long the body is, and whether the server
/// will serve a part of it.
pub struct Probe {
    pub len: u64,
    pub ranges: bool,
}

/// A HEAD, for a caller that has to size the body before reading it.
pub fn probe(url: &str) -> Result<Probe> {
    let headers = headers(url)?;
    Ok(Probe {
        len: content_length_of(&headers).with_context(|| {
            format!("{url}: no Content-Length, so its size is unknown before reading it")
        })?,
        ranges: header_of(&headers, "accept-ranges").is_some_and(|v| v.contains("bytes")),
    })
}

/// `len` bytes of `url` from `at`, over a `Range` request: how a disk image is
/// read, block by block, without ever holding the whole of it.
pub fn fetch_range(url: &str, at: u64, len: usize) -> Result<Vec<u8>> {
    let last = at + len as u64 - 1;
    let out = run(Command::new("curl").args(["-fsSL", "-r", &format!("{at}-{last}"), url]))?
        .with_context(|| format!("reading part of {url} needs curl, which is not installed"))?;
    if out.stdout.len() != len {
        bail!(
            "{url}: asked for bytes {at}-{last} and got {} of {len}: \
             the server ignores Range",
            out.stdout.len()
        );
    }
    Ok(out.stdout)
}

/// `url` in the cache as a local file, for an option that takes a path — the
/// EEPROM image, a HAT EEPROM, an EDID blob.
pub fn fetch_to_cache(url: &str) -> Result<PathBuf> {
    let dir = cache_dir()?.join("remote");
    let path = dir.join(cache_name(url));
    let body = get(url)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(&path, &body).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// `url`'s body, or why it could not be read — for a caller that answers a
/// request of its own and has nowhere to return an error to.
pub fn fetch_optional(url: &str) -> std::result::Result<Vec<u8>, String> {
    get(url).map_err(|e| format!("{e:#}"))
}

/// `<digest>-<name>`: readable in the cache, and unique per URL.
fn cache_name(url: &str) -> String {
    let digest: String = Sha1::digest(url)[..5]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let name: String = url
        .rsplit('/')
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
        .collect();
    format!("{digest}-{name}")
}

/// A `raw.githubusercontent.com` or `github.com/<owner>/<repo>/tree` URL, which
/// only the API can list.
struct GitHub {
    owner: String,
    repo: String,
    reference: String,
    path: String,
}

impl GitHub {
    fn parse(url: &str) -> Option<GitHub> {
        let (host, rest) = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))?
            .split_once('/')?;
        let mut it = rest.trim_end_matches('/').split('/');
        let owner = it.next()?.to_string();
        let repo = it.next()?.to_string();
        let mut rest: Vec<&str> = it.collect();
        match host {
            // The blob path is the reference and then the file's path, and a
            // reference is one segment unless it is spelled out in full.
            "raw.githubusercontent.com" => {}
            "github.com" if rest.first() == Some(&"tree") => {
                rest.remove(0);
            }
            _ => return None,
        }
        let refs = if rest.first() == Some(&"refs") { 3 } else { 1 };
        if rest.len() < refs {
            return None;
        }
        let reference = rest.drain(..refs).collect::<Vec<_>>().join("/");
        Some(GitHub {
            owner,
            repo,
            // The API takes a bare branch or tag, not a fully qualified ref.
            reference: reference
                .strip_prefix("refs/heads/")
                .or_else(|| reference.strip_prefix("refs/tags/"))
                .unwrap_or(&reference)
                .to_string(),
            path: rest.join("/"),
        })
    }

    fn listing(&self, path: &str) -> Result<Vec<Entry>> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/contents/{path}?ref={}",
            self.owner, self.repo, self.reference
        );
        let body = get(&url)?;
        let text = String::from_utf8(body).with_context(|| format!("{url}: not text"))?;
        let items = match crate::json::parse(&text)? {
            crate::json::Value::Arr(items) => items,
            // A file, or the API's `{"message": ...}` for a path that is not there.
            _ => bail!("{url}: not a directory listing"),
        };
        let mut entries = Vec::new();
        for item in &items {
            let name = string(item, "name").context("a listing entry without a name")?;
            if name.starts_with('.') {
                continue;
            }
            let kind = match string(item, "type").as_deref() {
                Some("file") => Kind::File {
                    source: Source::Url(
                        string(item, "download_url")
                            .with_context(|| format!("{name}: no download_url"))?,
                    ),
                    len: number(item, "size").with_context(|| format!("{name}: no size"))?,
                },
                Some("dir") => {
                    let path = string(item, "path").unwrap_or_else(|| format!("{path}/{name}"));
                    Kind::Dir(self.listing(&path)?)
                }
                // A symlink or a submodule is not a file of the card.
                _ => continue,
            };
            entries.push(Entry { name, kind });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }
}

fn string(v: &crate::json::Value, key: &str) -> Option<String> {
    match v.get(key) {
        Some(crate::json::Value::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

fn number(v: &crate::json::Value, key: &str) -> Option<u64> {
    match v.get(key) {
        Some(crate::json::Value::Num(n)) => n.parse().ok(),
        _ => None,
    }
}

/// A server that indexes the directory itself (`autoindex`, `mod_autoindex`):
/// the links on the page are the entries, and a HEAD gives each length.
fn index_listing(base: &str) -> Result<Vec<Entry>> {
    let page = get(base)?;
    let page = String::from_utf8_lossy(&page).into_owned();
    let mut entries = Vec::new();
    for name in links(&page) {
        let url = format!("{base}{name}");
        let kind = match name.strip_suffix('/') {
            Some(_) => Kind::Dir(index_listing(&url)?),
            None => Kind::File {
                len: content_length(&url)?,
                source: Source::Url(url),
            },
        };
        entries.push(Entry {
            name: name.trim_end_matches('/').to_string(),
            kind,
        });
    }
    if entries.is_empty() {
        bail!(
            "{base} lists no files. A URL is the boot partition's directory, \
             and the server has to index it — a GitHub URL is listed through \
             the API instead"
        );
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// The `href`s of an index page that name an entry of the directory itself: not
/// a parent, a sort order, another host, or a dot file.
fn links(page: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = page;
    while let Some(at) = rest.find("href=\"") {
        rest = &rest[at + 6..];
        let Some(end) = rest.find('"') else { break };
        let href = &rest[..end];
        rest = &rest[end..];
        let plain = !href.is_empty()
            && !href.starts_with('.')
            && !href.starts_with('/')
            && !href.starts_with('?')
            && !href.starts_with('#')
            && !href.contains("://")
            && !href.trim_end_matches('/').contains('/');
        if plain && !names.iter().any(|n| n == href) {
            names.push(href.to_string());
        }
    }
    names
}

fn with_slash(base: &str) -> String {
    match base.ends_with('/') {
        true => base.to_string(),
        false => format!("{base}/"),
    }
}

/// The body at `url`, over whichever of `curl` and `wget` is installed.
fn get(url: &str) -> Result<Vec<u8>> {
    let out = match run(Command::new("curl").args(["-fsSL", url]))? {
        Some(out) => out,
        None => run(Command::new("wget").args(["-q", "-O-", url]))?.with_context(|| {
            format!("neither curl nor wget is installed, so {url} cannot be fetched")
        })?,
    };
    Ok(out.stdout)
}

/// `url`'s length from its headers, which is all the card needs of a file until
/// the firmware reads a block of it.
fn content_length(url: &str) -> Result<u64> {
    content_length_of(&headers(url)?)
        .with_context(|| format!("{url}: no Content-Length, so the card cannot be built around it"))
}

/// The headers of a HEAD request, whichever tool made it.
fn headers(url: &str) -> Result<String> {
    // `wget -S` writes the headers it read to stderr, `curl -I` to stdout.
    let out =
        match run(Command::new("curl").args(["-fsSLI", url]))? {
            Some(out) => out,
            None => run(Command::new("wget").args(["-q", "-S", "--spider", url]))?.with_context(
                || format!("neither curl nor wget is installed, so {url} cannot be sized"),
            )?,
        };
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// The last value of `name`: every redirect on the way contributes a header
/// block, and the last block describes the body that would arrive.
fn header_of(headers: &str, name: &str) -> Option<String> {
    headers
        .lines()
        .filter_map(|l| {
            let (key, value) = l.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
        .next_back()
}

fn content_length_of(headers: &str) -> Option<u64> {
    header_of(headers, "content-length")?.parse().ok()
}

/// `None` when the program is not installed; an error is the transfer's own.
fn run(cmd: &mut Command) -> Result<Option<std::process::Output>> {
    let out = match cmd.output() {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => bail!("running {:?}: {e}", cmd.get_program()),
    };
    if !out.status.success() {
        bail!(
            "{:?} {}: {}",
            cmd.get_program(),
            cmd.get_args().last().unwrap_or_default().to_string_lossy(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(url: &str) -> String {
        let gh = GitHub::parse(url).expect("a GitHub URL");
        format!("{}/{}@{}:{}", gh.owner, gh.repo, gh.reference, gh.path)
    }

    #[test]
    fn a_raw_url_names_the_repository_its_branch_and_the_path() {
        assert_eq!(
            api("https://raw.githubusercontent.com/raspberrypi/firmware/refs/heads/master/boot/"),
            "raspberrypi/firmware@master:boot"
        );
        assert_eq!(
            api("https://raw.githubusercontent.com/raspberrypi/firmware/1.20260907/boot"),
            "raspberrypi/firmware@1.20260907:boot"
        );
        assert_eq!(
            api("https://github.com/raspberrypi/firmware/tree/master/boot/overlays"),
            "raspberrypi/firmware@master:boot/overlays"
        );
        assert_eq!(
            api("https://raw.githubusercontent.com/raspberrypi/firmware/master/"),
            "raspberrypi/firmware@master:"
        );
    }

    #[test]
    fn another_host_is_listed_by_its_own_index() {
        assert!(GitHub::parse("https://example.org/pi/boot/").is_none());
        assert!(GitHub::parse("https://github.com/raspberrypi/firmware").is_none());
    }

    #[test]
    fn an_index_page_links_only_the_entries_of_the_directory() {
        let page = "<a href=\"../\">up</a> <a href=\"?C=N;O=D\">sort</a> \
             <a href=\"start4.elf\">start4.elf</a><a href=\"overlays/\">overlays/</a> \
             <a href=\"https://elsewhere/x\">x</a> <a href=\"/abs\">abs</a> \
             <a href=\"start4.elf\">again</a> <a href=\"sub/deep.txt\">deep</a>";
        assert_eq!(links(page), ["start4.elf", "overlays/"]);
    }

    #[test]
    fn the_last_content_length_of_a_redirect_chain_wins() {
        let headers = "HTTP/1.1 301\r\nContent-Length: 0\r\n\r\n\
             HTTP/1.1 200\r\ncontent-type: text/plain\r\nContent-Length: 2298912\r\n";
        assert_eq!(content_length_of(headers), Some(2298912));
        assert_eq!(content_length_of("HTTP/1.1 200\r\n"), None);
    }

    #[test]
    fn a_cache_name_is_the_urls_digest_and_the_files_name() {
        let name = cache_name("https://example.org/boot/start4.elf");
        assert!(name.ends_with("-start4.elf"), "{name}");
        assert_ne!(
            cache_name("https://elsewhere.org/boot/start4.elf"),
            name,
            "the same name on another host is another file"
        );
    }

    #[test]
    fn a_url_is_told_apart_from_a_path() {
        assert!(is_url("https://example.org/boot/"));
        assert!(is_url("http://example.org/boot/"));
        assert!(!is_url("firmware/boot"));
        assert!(!is_url("/srv/tftp/boot"));
    }
}

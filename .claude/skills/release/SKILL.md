---
name: release
description: Cut a pimu release - work out the version bump from what changed since the last tag, write the release notes, bump Cargo.toml, tag main and push so the release workflow builds it. Use when asked to "make a release", "cut a release", "release pimu" or "tag a version".
---

# Cutting a release

A release is an `X.Y.Z` tag on main — no `v` prefix. Pushing that tag runs
`.github/workflows/release.yml`, which builds the binaries, the packages and
the container image and publishes a GitHub release for the tag. Everything this
skill does happens locally until the final push, so every decision can be
changed before anything is public.

There is no bot and no API key in CI: you read the changes and decide the bump
yourself.

## 1. Check the preconditions

Stop and report if either of these fails rather than working around it:

- The working tree is clean and the branch is `main`.
- `git fetch origin` then `git status -sb` shows main level with
  `origin/main`. Release what is pushed, never a local-only commit.

Then look at what CI has already said about the commit being released:

```bash
gh run list --commit "$(git rev-parse HEAD)" \
  --json workflowName,status,conclusion --jq '.[] | "\(.workflowName) \(.status) \(.conclusion // "")"'
```

**Never wait for a run that is still going.** The release job boots the tagged
commit itself while it trains the PGO profile, and section 5 runs `fmt`,
`clippy` and the tests here before the tag is made, so an in-flight `boot-log`
run is not a reason to hold the release.

A run that has already **completed and failed** on that commit is worth
pausing for: name the workflow that failed and ask whether to release anyway.
Release on a yes — a red run is the user's call, not a veto.

The release job checks the same thing for itself: it refuses a commit
`simulated-boot-log` has already failed on, and only warns when that workflow has
no finished run for the commit. So a red run you decide to release anyway
needs the failing workflow re-run green on that commit first, or the release
job will stop on it.

## 2. Find what is being released

```bash
last=$(git describe --tags --match '[0-9]*' --abbrev=0 2>/dev/null)
```

With no version tag yet this is the first release: use the whole history, and see
the first-release note in section 4.

Read all three of these before deciding anything:

```bash
git log --format='%h %s%n%b' "$last..HEAD"
git diff --stat "$last..HEAD"
git diff "$last..HEAD" -- src/cli/ Cargo.toml packaging/ scripts/ docs/
```

The subjects alone are not enough. This repository writes prose commit
subjects with an area prefix (`vpu:`, `boot:`, `specs:`), not Conventional
Commits, so nothing in a subject says whether a change is breaking. Read the
diff of the compatibility surface.

## 3. Decide the bump

The compatibility surface of pimu is what someone running the released binary
depends on — not the Rust API. `publish = false` and nothing consumes
`src/lib.rs`, so a change to a `pub` item in the library is not by itself a
breaking change.

**Breaking** — a command line that worked against the previous release stops
working, or does something materially different:

- a subcommand, option or alias removed or renamed (`boot --help` is pinned by
  a test, so its diff is the place to look)
- a `--config` key removed or renamed, or its meaning changed
- a default changed such that the same command now boots differently
- a `PIMU_*` switch removed (see `docs/diagnostics.md`)
- the package layout changed — an installed path moving out of `/usr/bin/pimu`
- the MSRV raised (`rust-version` in `Cargo.toml`)

**Feature** — something new that an old command line does not notice: a new
subcommand or option, a newly modelled peripheral, a boot that reaches further
than it did, a new scenario.

**Fix** — everything else: a corrected model, a speed-up, docs, specs, tests,
CI, refactors. Modelling a register more accurately is a fix, not a feature,
unless it makes a previously impossible boot work.

Then map that to a version. **While the version is `0.x`, the minor field is
the incompatible slot**, so both breaking and feature bump it:

| Verdict  | `0.1.0` becomes |
|----------|-----------------|
| breaking | `0.2.0`         |
| feature  | `0.2.0`         |
| fix      | `0.1.1`         |

Say in the summary which of the two a minor bump came from — the notes carry
the detail that the number cannot. Cutting `1.0.0` is a deliberate decision:
never do it on your own reading, only when the user asks for it by name.

Two things override your reading, in this order:

1. **What the user said.** "make a patch release" settles it; classify only
   when they did not say.
2. **A trailer on any commit in the range.** `Breaking-change: <what>` forces
   breaking, `Feature: <what>` forces at least feature. Normal commits need no
   trailer — the default is a fix.

## 4. Write the notes

Match the repository's voice: plain, direct, no emoji, no praise, no
"🎉 What's New". Present tense, saying what the release does rather than what
was committed.

Structure:

- One or two sentences saying what this release is, when there is a theme.
- Grouped bullets under `### ` headings, using the commits' own area prefixes
  as the groups (`VPU`, `Boot`, `Specs`, `Docs`, ...). Skip a group with
  nothing in it.
- A `### Breaking` section first, whenever there is one, saying what to change.
- Leave out pure-CI, formatting and revert-of-an-unreleased-commit churn. A
  commit reverted inside the same range is not news.
- Mark register and field names, addresses, options, paths and commands as
  inline code.

For the first release, describe what pimu is and does today rather than
listing the whole history.

The workflow appends the install instructions and the "built against Debian
12's glibc" paragraph itself, so leave those out of the notes.

## 5. Show the user, then apply

Print the proposed version, one line on why that bump, and the full notes.
Wait for approval before touching anything.

On approval:

Edit the `version` field in `Cargo.toml` to the new version — an exact edit of
that one line, not a blind `sed` over the file, which would also hit a
dependency's version. Then:

```bash
version=X.Y.Z
cargo update --workspace --offline
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
git commit -am "Release $version"
git tag -a --cleanup=verbatim -F notes.md "$version"
```

`--cleanup=verbatim` is not optional: git's default cleanup strips every line
starting with `#` from a tag message, which would silently eat the notes'
Markdown headings.

`cargo update --workspace` rewrites `Cargo.lock`, which is tracked, so both
files belong in the release commit.

The tag must be **annotated**, with the notes as its message: the workflow
publishes the tag's message as the release body. A lightweight tag makes the
workflow fall back to generated notes.

## 6. Push

Pushing the tag publishes a release, and this repository is public — **always
ask before pushing, even when the user has already approved the version and
the notes**. Show exactly what will be pushed:

```bash
git push origin main "$version"
```

Main goes first in that command so the tag never arrives pointing at a commit
the remote does not have.

If the user declines, leave the commit and the tag in place and say how to
undo them (`git tag -d $version`, `git reset --hard HEAD~1`).

## 7. Watch the build

```bash
gh run watch "$(gh run list --workflow=release.yml --limit 1 --json databaseId -q '.[0].databaseId')"
```

The job takes around an hour: it trains a PGO profile by booting the firmware
before it builds either binary. When it finishes, check the release has the six
assets (two tarballs, two `.deb`, two `.rpm`) and report the release URL.

If it fails, the tag stays and the release is absent or incomplete. Fix the
cause on main and cut the next patch version rather than moving the tag — a
moved tag breaks anyone who already fetched it.

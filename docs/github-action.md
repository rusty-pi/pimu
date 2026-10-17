# The GitHub Action

`rusty-pi/pimu` is a composite action: it installs a released `pimu` binary on
a Linux runner and, if you give it arguments, runs it. Use it to boot your own
firmware and compare the serial transcript in CI, without building pimu.

```yaml
jobs:
  boot:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v7
      - uses: rusty-pi/pimu@0.6.0
        with:
          args: boot --scenario boot.yaml
```

`@0.6.0` pins the action and is also the default pimu version it installs
only when `version` says `latest`, so pin both when the result has to be
repeatable:

```yaml
      - uses: rusty-pi/pimu@0.6.0
        with:
          version: 0.6.0
          args: boot --scenario boot.yaml
```

| input | default | |
|---|---|---|
| `version` | `latest` | The release tag to install, without a `v`. `latest` is the newest release. |
| `args` | empty | Arguments for `pimu`, run through the shell in `working-directory`. Empty installs only, and later steps call `pimu` themselves. |
| `working-directory` | `.` | Where `args` runs. |
| `token` | `github.token` | What `gh` downloads the release with. |

The output `version` is the release that was installed.

## What it does not do

- It installs Linux binaries only, for x86-64 and aarch64 runners
  (`ubuntu-24.04` and `ubuntu-24.04-arm`).
- It brings no firmware. A boot needs your `pieeprom.bin` (`--eeprom`, a path
  or a URL) and a boot medium; see [`running.md`](running.md).
- `args` is evaluated by the shell, so quote what needs quoting, and only put
  text you wrote there.

The action is checked by `.github/workflows/action.yml`, which installs the
newest release on both architectures whenever `action.yml` changes.

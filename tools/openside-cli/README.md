<div align="center">

<img src="docs/brand/openside.svg" alt="openside" width="440"/>

**sideload .ipas from a beautiful terminal**

sign · install · renew · track expiry · apple id · anisette. one TUI, mouse and keyboard

[![Release](https://img.shields.io/github/v/release/schnnjuan/openside?style=for-the-badge&color=026D4F)](https://github.com/schnnjuan/openside/releases)
[![crates.io](https://img.shields.io/crates/v/openside?style=for-the-badge&color=24A067)](https://crates.io/crates/openside)
[![License](https://img.shields.io/badge/license-MIT-informational?style=for-the-badge)](LICENSE)

</div>

---

## what it does

- **sideload an .ipa**: sign + install straight from the file browser
- **expiry tracking**: every installed app, days left, expired / expiring counts
- **renew**: all apps expiring soon, or one at a time
- **apple id**: sign in, accounts, teams, cert / app id / device slots
- **device**: connected iPhones, developer mode, pairing file, installed apps
- **ipa tools**: inspect metadata, sign to a new file

## install

```bash
cargo install openside
openside
```

Python 3.12+ for the signing engine, from this repo:

```bash
pip install src/iPASide.Engine
```

## the interface

- full keyboard and mouse: click, click again or `⏎` to run
- command menu on the left, stylized output on the right (expiry badges, device chips, doctor checks)
- .ipa file browser with `ls`, `cd` and a mini command line: relative, `~` and absolute paths, quick `cd ..` with Backspace
- English / Português (BR) picker at first run, switchable from the version screen; the whole interface is translated

## engine

the terminal is the product; signing happens in a Python engine spawned as a child process over NDJSON stdio.

## provenance

**openside is a fork** of [iPASide](https://github.com/pwnapplehat/iPASide) (MIT, by pwnapplehat).

| inherited from iPASide | new in openside |
|---|---|
| the whole Python engine (`src/iPASide.Engine`), untouched | the entire TUI (`tools/openside-cli`, Rust) |
| pairing, anisette, app registry | menu, stylized views, mouse, .ipa browser, en↔pt-BR |

the engine stays untouched on purpose: it touches your Apple account and your apps, and stability there beats churn.

MIT.

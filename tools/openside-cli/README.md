<div align="center">

# openside

**Sideloading .ipas from a beautiful terminal.**

A keyboard-and-mouse driven terminal interface (ratatui) that signs, installs,
renews and tracks sideloaded apps on your iPhone — no jailbreak, no paid
developer account.

[![Release](https://img.shields.io/github/v/release/schnnjuan/openside?style=for-the-badge&color=026D4F)](https://github.com/schnnjuan/openside/releases)
[![crates.io](https://img.shields.io/crates/v/openside?style=for-the-badge&color=24A067)](https://crates.io/crates/openside)
[![License](https://img.shields.io/badge/license-MIT-informational?style=for-the-badge)](LICENSE)

</div>

---

## Install

```bash
cargo install openside
```

Then just run:

```bash
openside
```

Requires a Python ≥ 3.12 environment for the signing engine. The engine lives
in this repository (`src/iPASide.Engine`):

```bash
pip install src/iPASide.Engine
```

## What it does

| | |
|---|---|
| **Sideload an .ipa** | sign + install straight from the file browser |
| **Expiry tracking** | every installed app with days left, expired / expiring counts |
| **Renew** | all apps expiring soon, or one app at a time |
| **Apple ID** | sign in, list accounts, teams, cert / app-id / device slots |
| **Device** | connected iPhones, developer-mode state, pairing file, installed apps |
| **Anisette** | provisioning status |
| **Handy .ipa tools** | inspect metadata, sign to a new file |

## The interface

- **Full keyboard & mouse** — pick an item, click again (or `⏎`) to run.
- **Menu of commands** in the left column; live output panel on the right with
  stylized views per command (gauges, expiry badges, device chips) — never raw
  JSON.
- **File browser for .ipa** with `ls`, `cd` and a mini command line — type a
  relative/absolute path direct, or `cd ~/Downloads` and click.
- **First-run language picker** (English / Português-BR), switchable anytime
  from the *version* screen; everything (menu, dialogues, output, notes) is
  fully translated.
- **Nothing is typed at you**: every action is a wizard or a picker; only the
  .ipa path input accepts typing.

## Engine

`openside` is a terminal frontend; the heavy lifting (apple-id auth, signing,
device pairing) is done by the OpenSide engine — a Python service spawned as a
child process, communicating over NDJSON on stdio.

## License

MIT

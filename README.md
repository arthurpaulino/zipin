# zipin

`zipin` is a non-invasive, highly responsive terminal application for writing in
Chinese. The name comes from "Zì" (字 — "character") and "Pīn" (拼 — "to spell"),
and it's also a playful nod to the fact that `zipin` is for zipin' Hàn**zì** and
**Pīn**yīn.

`zipin` statically links [librime](https://github.com/rime/librime), so the
candidate quality matches what you get from a real desktop IME (Squirrel, Weasel,
fcitx-rime). No daemon, no system integration, no `.userdb` outside of zipin's
own data dir.

## Install

No precompiled binaries are published — build it yourself. Requires:

- A C++17 toolchain (GCC 11+, Clang 15+, or MSVC 2022)
- CMake 3.20+
- `make` and `git`
- Linux: `libboost-dev` and `libboost-regex-dev`
- The [Rust toolchain](https://www.rust-lang.org/tools/install)

Then, from the repo root:

```
cargo install --path crates/zipin
```

`zipin` lands in `~/.cargo/bin/`; make sure that's on your `PATH`.

The first install clones librime and its C++ submodules and compiles
them; this takes 5–15 minutes on a clean machine. By default the
checkout lives in cargo's temporary install dir and gets discarded after
the binary is copied out, so reinstalls re-pay the cost. To cache it
across reinstalls, point `ZIPIN_LIBRIME_DIR` at a stable location:

```
export ZIPIN_LIBRIME_DIR="$HOME/.cache/zipin/librime"
mkdir -p "$ZIPIN_LIBRIME_DIR"
cargo install --path crates/zipin
```

Cap cmake parallelism (default 8) with `RIME_BUILD_JOBS=N` if your box
runs short on RAM during the boost-regex compile.

For a development build (artifacts in `target/`, faster iteration):

```
cargo build --release
```

## Usage

| Key | Action |
| --- | --- |
| `a`–`z` | Open or extend composition (Chinese mode), or commit raw (ASCII mode) |
| `Shift`+letter | Commit one uppercase letter |
| `1`–`9`, `0` | Pick the Nth candidate from the current page |
| `Tab` / `Shift+Tab` | Cycle through candidates |
| `↑` / `↓` (composing) | Previous / next candidate (rank step) |
| `←` / `→` (composing) | Previous / next candidate page |
| `Space` (composing) | Commit the highlighted candidate |
| `Space` (idle) | Insert full-width blank `　` (Chinese mode) or ASCII space (English mode) |
| `Esc` | Cancel composition |
| `Backspace` | Shrink composition, then delete characters |
| `Delete` | Delete the segment under cursor |
| `←` / `→` (idle) | Move document cursor |
| `↑` / `↓`, `Home`, `End` | Move document cursor |
| `Ctrl+↑` / `Ctrl+↓` | Scroll document |
| `Enter` | New line |
| `,` `.` `?` `!` `:` `;` `'` `"` | Commit highlighted (if composing) + insert full-width punctuation |
| `Ctrl+C` | Copy document to clipboard |
| `Ctrl+V` | Paste clipboard into document |
| `Ctrl+Space` | Toggle Chinese ↔ English mode (cursor changes shape) |
| `Ctrl+N` | Clear document |
| `Ctrl+Q` | Quit |

The cursor signals the current language mode:

- **Chinese** — block cursor (`█`), matching the visual weight of a hanzi cell.
- **English** — bar cursor (`▏`), thin Latin caret.

`Ctrl+Space` toggles between them and clears any in-flight composition.

### Abbreviation expectations

zipin ships the stock `luna_pinyin` schema unchanged. That means
abbreviations follow the same rules as desktop Rime:

- `nh` → 你好, `bjdx` → 北京大学 — first-letter abbrev works.
- `zsh` → 这是 (two syllables `zh + sh`), not three. Three-syllable
  abbreviations like 早上好 require typing the third letter explicitly
  (`zshh`). Aggressive 1-letter-per-syllable schemas (e.g. rime-ice) are
  GPL-3 and excluded by zipin's MIT licensing.
- `wobuzhidao` → 我不知道 — full sentences segment via the bundled essay
  corpus.

### CLI flags

| Flag | Action |
| --- | --- |
| `-h`, `--help` | Print help and exit. |
| `-V`, `--version` | Print version and exit. |
| `--licenses` | Print zipin's MIT license + full third-party attribution + the `NOTICE` file. |
| `--forget` | Wipe Rime user-dict files (learned phrases) under `$DATA_DIR/zipin/rime/user/`. Bundled schemas live in `shared/` and are preserved; deployed `.bin` files re-build on next launch. |

## Customizing

Drop standard Rime patch files into `~/.local/share/zipin/rime/user/`.
Each `<name>.custom.yaml` overrides the matching `<name>.yaml`. Restart
zipin → Rime re-deploys and merges patches. Bundled defaults under
`shared/` are owned by zipin and overwritten on upgrades; never edit
those.

### Add cangjie5 to the schema list

```yaml
# user/default.custom.yaml
patch:
  schema_list:
    - schema: luna_pinyin
    - schema: cangjie5
```

### Default to traditional output

```yaml
# user/luna_pinyin.custom.yaml
patch:
  switches:
    - options: [ zh_trad, zh_simp, zh_hk, zh_tw ]
      reset: 0      # 0 = first option (zh_trad)
      states: [ 繁體, 简体, 香港, 臺灣 ]
```

### Aggressive 1-letter-per-syllable abbreviation

So `zsh` parses as `z + s + h` (→ 早上好) instead of `zh + sh` (→ 这是):

```yaml
# user/luna_pinyin.custom.yaml
patch:
  speller/algebra:
    - erase/^xx$/
    - abbrev/^([a-z]).+$/$1/    # any word → first letter only
    - derive/^([nl])ve$/$1ue/
    - derive/^([jqxy])u/$1v/
    - derive/un$/uen/
    - derive/ui$/uei/
```

### Disable sentence completion

```yaml
# user/luna_pinyin.custom.yaml
patch:
  translator/enable_sentence: false
```

### Swap to a tone-aware schema (terra_pinyin)

1. Drop `terra_pinyin.schema.yaml` + matching dict files into `user/`.
2. Patch the schema list:

```yaml
# user/default.custom.yaml
patch:
  schema_list:
    - schema: terra_pinyin
```

For the full patch DSL, see
[Rime's CustomizationGuide](https://github.com/rime/home/wiki/CustomizationGuide).

## Why

Typing Chinese on Linux/macOS/Windows usually requires installing
system-wide IME software that runs background services and integrates
deeply with the OS. Most are great. None of them feel terminal-native.

`zipin` is a single binary that runs in your terminal: no daemon, no DE
hooks, no `.so` to keep around. Open the binary, type pinyin, get the
same candidate quality you'd get from a desktop IME (because it's the
same engine — librime).

Ideal for notes, drills, quick snippets, and anywhere you want a
disposable IME for a single window.

## Third-party software

zipin statically links librime + its C++ deps (LevelDB, yaml-cpp,
marisa-trie, OpenCC, Boost.Regex on Linux) and embeds the BSD-3-Clause
Rime data shipped in `librime/data/minimal/`. All deps are permissively
licensed; no copyleft is included in shipped artifacts.

Full attribution + license texts: [`LICENSE-THIRD-PARTY.md`](LICENSE-THIRD-PARTY.md)
or run `zipin --licenses`.

## License

`zipin` is licensed under the [MIT License](LICENSE).

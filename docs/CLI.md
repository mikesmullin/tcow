# tcow CLI Reference

**Version** — 1.0
**Date** — February 2026

The `tcow` binary is a standalone host-side utility for inspecting, manipulating, and maintaining `.tcow` virtual filesystem files. It shares the `tcow` library crate with the Wasmtime agent host — no separate parser, no drift in format interpretation.

Build:
```
cargo build --bin tcow --release
```

The binary is at `target/release/tcow`.

---

## Top-level help

```
$ tcow --help
tcow 1.0.0
Copy-on-write virtual filesystem inspector and manager.
A .tcow file stores a layered tar-based filesystem with CoW semantics.

USAGE:
    tcow <SUBCOMMAND> [OPTIONS] <FILE>

SUBCOMMANDS:
    info        Show metadata and layer summary for a .tcow file
    list        List files visible in the current union view (or a specific layer)
    cat         Print the contents of a file from the virtual filesystem
    stat        Show metadata for a specific file path
    insert      Add or replace a file in a new writable delta layer
    delete      Mark a file as deleted (write a whiteout) in a new delta layer
    extract     Extract files from the virtual filesystem to the host
    snapshot    Seal the current state and start a new writable layer
    compact     Merge all layers into a single base layer (destructive, creates new file)
    verify      Check integrity of all layer digests
    layers      List all layers with byte offsets and sizes
    help        Print this message or the help of a given subcommand

GLOBAL OPTIONS:
    -f, --file <FILE>    Path to the .tcow file [env: TCOW_FILE]
    -v, --verbose        Enable verbose output
    --color <WHEN>       Color output: auto, always, never [default: auto]
    --version            Print version information
    -h, --help           Print help information
```

---

## Subcommands

---

### `info`

Print a human-readable summary of a `.tcow` file's metadata and layer structure.

```
$ tcow info --help
tcow-info
Show metadata and layer summary for a .tcow file.

Reads and displays the CBOR trailer index without scanning file contents.

USAGE:
    tcow info [OPTIONS] <FILE>

ARGS:
    <FILE>    Path to the .tcow file

OPTIONS:
    -h, --help    Print help information
```

**Example output:**

```
$ tcow info agent.tcow

File:          agent.tcow
Size:          87,412 bytes
Format:        TCOW v1
Last modified: 2026-02-28T14:32:00Z
Label:         run-abc123
Layers:        3

  #  Kind   Offset      Size        Digest (SHA-256)           Created
  ─  ─────  ──────────  ──────────  ─────────────────────────  ──────────────────────
  0  Base         16    8,192 B     a3f27b…c91e               2026-02-28T12:00:00Z
  1  Delta     8,208    4,096 B     55f10c…aa32               2026-02-28T13:15:00Z
  2  Delta    12,304   71,680 B     9d3e84…2b17               2026-02-28T14:32:00Z

Union view: 47 files visible
```

---

### `ls`

List files visible in the merged union view of the filesystem, or restrict to a single layer.

```
$ tcow ls --help
tcow-ls
List files visible in the current union view, or from a specific layer.

Union view applies the same top-to-bottom lookup as the live agent:
whiteouts hide files from lower layers, and the highest-layer version
of each file is shown.

USAGE:
    tcow ls [OPTIONS] <FILE> [PATH]

ARGS:
    <FILE>    Path to the .tcow file
    [PATH]    Optional directory path to list (default: / — list everything)

OPTIONS:
    -l, --layer <N>        Restrict listing to this layer index only (0-based)
    -a, --all-layers       Show every entry from every layer, including hidden/overwritten ones
    -L, --long             Long format: show size, mtime, layer index, and whiteout flag
    -d, --dirs-only        Show only directory entries
    --show-whiteouts       Include whiteout entries in output (prefixed with [DEL])
    -h, --help             Print help information
```

**Example: union view**

```
$ tcow ls agent.tcow
/config/settings.json
/data/records.db
/output/result.json
/thoughts/step2.md
```

**Example: long format**

```
$ tcow ls -L agent.tcow /
      SIZE  MTIME                LAYER  PATH
    ──────  ───────────────────  ─────  ────────────────────────
     4,096  2026-02-28T12:00:00      0  /config/settings.json
    12,288  2026-02-28T13:15:00      1  /data/records.db
     1,024  2026-02-28T14:32:00      2  /output/result.json
       512  2026-02-28T14:32:00      2  /thoughts/step2.md
```

**Example: show whiteouts**

```
$ tcow ls --show-whiteouts -a agent.tcow
[DEL]  /thoughts/step1.md  (whiteout in layer 2)
       /thoughts/step1.md  (original in layer 1, hidden)
       /thoughts/step2.md  (layer 2)
       /config/settings.json  (layer 0)
       ...
```

**Example: single layer**

```
$ tcow ls --layer 1 agent.tcow
/data/records.db
/thoughts/.wh.step1.md
```

---

### `cat`

Print the contents of a file from the virtual filesystem to stdout.

```
$ tcow cat --help
tcow-cat
Print the contents of a file from the virtual (union) filesystem.

Resolves the path using the standard top-to-bottom union view and writes
raw bytes to stdout. Safe to pipe into other tools.

USAGE:
    tcow cat [OPTIONS] <FILE> <PATH>

ARGS:
    <FILE>    Path to the .tcow file
    <PATH>    Virtual filesystem path to read (e.g. /config/settings.json)

OPTIONS:
    -l, --layer <N>    Read from a specific layer instead of the union view
    -h, --help         Print help information
```

**Examples:**

```sh
# Print to terminal
$ tcow cat agent.tcow /output/result.json
{"answer": "Tokyo is the capital of Japan.", "confidence": 0.99}

# Pipe to jq
$ tcow cat agent.tcow /output/result.json | jq .answer
"Tokyo is the capital of Japan."

# Write to a host file
$ tcow cat agent.tcow /data/records.db > records.db

# Read from a specific layer (ignores whiteouts above it)
$ tcow cat --layer 1 agent.tcow /thoughts/step1.md
Step 1: query the database...
```

---

### `stat`

Show detailed metadata for a specific path in the virtual filesystem.

```
$ tcow stat --help
tcow-stat
Show metadata for a specific virtual filesystem path.

Resolves the path using the union view and prints size, modification time,
the layer index it was found in, and whether it is a whiteout marker.

USAGE:
    tcow stat [OPTIONS] <FILE> <PATH>

ARGS:
    <FILE>    Path to the .tcow file
    <PATH>    Virtual filesystem path (e.g. /config/settings.json)

OPTIONS:
    --json    Output metadata as JSON instead of human-readable text
    -h, --help    Print help information
```

**Example:**

```
$ tcow stat agent.tcow /data/records.db

Path:     /data/records.db
Size:     12,288 bytes
Mtime:    2026-02-28T13:15:00Z
Layer:    1 (Delta)
Whiteout: false
```

**JSON output:**

```sh
$ tcow stat --json agent.tcow /data/records.db
{
  "path": "/data/records.db",
  "size": 12288,
  "mtime": "2026-02-28T13:15:00Z",
  "layer": 1,
  "whiteout": false
}
```

---

### `insert`

Add or replace a file in the `.tcow` filesystem by appending a new delta layer.

The original `.tcow` file is never rewritten in-place for existing layers; a new delta tar stream is appended and the CBOR trailer is updated.

```
$ tcow insert --help
tcow-insert
Add or replace a file inside a .tcow virtual filesystem.

Reads source bytes from a host file (or stdin), writes them into a new
delta layer appended to the .tcow file, and updates the trailer index.
If a file at <VPATH> already exists in any layer, it is effectively
shadowed (copy-up semantics: lower layers are not modified).

USAGE:
    tcow insert [OPTIONS] <FILE> <VPATH> [SOURCE]

ARGS:
    <FILE>      Path to the .tcow file
    <VPATH>     Destination path inside the virtual filesystem (e.g. /config/new.json)
    [SOURCE]    Source file to read from (default: stdin)

OPTIONS:
    --mtime <DATETIME>    Override modification time (RFC 3339). Default: now.
    --dry-run             Show what would be inserted without modifying the file
    -h, --help            Print help information
```

**Examples:**

```sh
# Insert a host file at a virtual path
$ tcow insert agent.tcow /config/settings.json ./local-settings.json
Inserted /config/settings.json (4,096 bytes) into new delta layer 3

# Insert from stdin
$ echo '{"mode":"debug"}' | tcow insert agent.tcow /config/flags.json
Inserted /config/flags.json (17 bytes) into new delta layer 3

# Insert multiple files by running insert once per file
$ for f in output/*.json; do
    tcow insert agent.tcow "/results/$(basename $f)" "$f"
  done

# Dry run
$ tcow insert --dry-run agent.tcow /config/settings.json ./local-settings.json
[DRY RUN] Would insert /config/settings.json (4,096 bytes) as new delta layer 3
```

---

### `delete`

Mark a virtual filesystem path as deleted by writing a whiteout entry in a new delta layer.

```
$ tcow delete --help
tcow-delete
Mark a file as deleted in the virtual filesystem.

Appends a whiteout tar entry (.wh.<basename>) to a new delta layer,
causing the path to appear non-existent in all future union views.
The original content in lower layers is preserved and can still be
accessed with --layer on the list/cat subcommands.

USAGE:
    tcow delete [OPTIONS] <FILE> <VPATH>

ARGS:
    <FILE>     Path to the .tcow file
    <VPATH>    Virtual filesystem path to delete (e.g. /data/records.db)

OPTIONS:
    --dry-run    Show what whiteout would be written without modifying the file
    -h, --help   Print help information
```

**Examples:**

```sh
$ tcow delete agent.tcow /data/records.db
Wrote whiteout for /data/records.db in new delta layer 3

$ tcow delete --dry-run agent.tcow /data/records.db
[DRY RUN] Would write whiteout data/.wh.records.db in new delta layer 3
```

---

### `extract`

Extract files from the virtual filesystem to the host filesystem.

```
$ tcow extract --help
tcow-extract
Extract files from the virtual filesystem to a host directory.

Resolves all paths using the union view (whiteouts are respected —
deleted files are not extracted). Optionally restrict to a single layer.

USAGE:
    tcow extract [OPTIONS] <FILE> [VPATH] <OUTDIR>

ARGS:
    <FILE>      Path to the .tcow file
    [VPATH]     Virtual path to extract (file or directory prefix). Default: / (all files)
    <OUTDIR>    Host directory to write extracted files into (created if absent)

OPTIONS:
    -l, --layer <N>      Extract from a specific layer only (bypasses union view)
    --strip-prefix <P>   Strip this prefix from virtual paths before writing to OUTDIR
    --dry-run            List what would be extracted without writing to disk
    -h, --help           Print help information
```

**Examples:**

```sh
# Extract everything to ./extracted/
$ tcow extract agent.tcow ./extracted/
Extracted 47 files to ./extracted/

# Extract a single subdirectory
$ tcow extract agent.tcow /config/ ./extracted-config/
Extracted 3 files to ./extracted-config/

# Extract a single file
$ tcow extract agent.tcow /output/result.json .
Extracted 1 file to .

# Extract only from layer 0 (base), ignoring all deltas
$ tcow extract --layer 0 agent.tcow ./base-snapshot/

# Strip a path prefix
$ tcow extract --strip-prefix /output agent.tcow /output ./out/
# Writes ./out/result.json  (not ./out/output/result.json)
```

---

### `snapshot`

Seal the current in-memory or pending writable state and record it as an immutable delta layer. For a `.tcow` file that was written by an agent and exists purely on disk (no live process), `snapshot` adds an empty marker delta layer to checkpoint the current state.

```
$ tcow snapshot --help
tcow-snapshot
Seal the current state and start a new writable layer.

For files with a pending (unflushed) writable layer written externally,
this ensures the trailer index is consistent. When called on a fully
flushed file, it appends an empty delta layer to mark a checkpoint.

USAGE:
    tcow snapshot [OPTIONS] <FILE>

ARGS:
    <FILE>    Path to the .tcow file

OPTIONS:
    --label <TEXT>    Attach a human-readable label to this snapshot
    -h, --help        Print help information
```

**Examples:**

```sh
$ tcow snapshot agent.tcow
Snapshot created: layer 3 (Delta) at offset 83,504

$ tcow snapshot --label "after-step-5" agent.tcow
Snapshot created: layer 3 (Delta) "after-step-5" at offset 83,504
```

---

### `compact`

Merge all layers into a single base layer containing only the current visible union-view contents. Eliminates unreachable bytes from copy-ups and overwrites. Writes to a **new file** by default — the original is never modified.

```
$ tcow compact --help
tcow-compact
Compact a .tcow file by merging all layers into a single base layer.

Resolves the full union view (respecting whiteouts), then writes a new
.tcow file containing exactly one Base layer with all currently visible
files. The original file is not modified unless --in-place is set.

WARNING: --in-place is irreversible. Always keep a backup of the original.

USAGE:
    tcow compact [OPTIONS] <FILE>

ARGS:
    <FILE>    Path to the .tcow file to compact

OPTIONS:
    -o, --output <FILE>    Output path for the compacted file [default: <FILE>.compacted.tcow]
    --in-place             Overwrite the original file (IRREVERSIBLE)
    --dry-run              Report how many bytes would be reclaimed without writing
    -h, --help             Print help information
```

**Examples:**

```sh
# Compact to a new file (safe)
$ tcow compact agent.tcow
Compacted agent.tcow → agent.tcow.compacted.tcow
  Before: 3 layers, 87,412 bytes
  After:  1 layer,  21,504 bytes  (75% reduction)

# In-place (irreversible)
$ tcow compact --in-place agent.tcow
WARNING: This will overwrite agent.tcow. Type YES to confirm: YES
Compacted in place: 87,412 → 21,504 bytes

# Dry run
$ tcow compact --dry-run agent.tcow
[DRY RUN] Would reclaim 65,908 bytes (75%) from agent.tcow
```

---

### `verify`

Check the integrity of a `.tcow` file by recomputing the SHA-256 digest for each layer and comparing against the values stored in the CBOR trailer.

```
$ tcow verify --help
tcow-verify
Check integrity of all layer digests in a .tcow file.

Re-reads each layer's raw tar bytes and computes their SHA-256 digest,
then compares the result against the digest stored in the CBOR trailer.
Layers without a stored digest are skipped with a warning.

USAGE:
    tcow verify [OPTIONS] <FILE>

ARGS:
    <FILE>    Path to the .tcow file

OPTIONS:
    --fix-missing    Compute and write digests for layers that have none (modifies trailer)
    -h, --help       Print help information
```

**Example output (all OK):**

```
$ tcow verify agent.tcow

Verifying agent.tcow (3 layers)...

  Layer 0  [Base ]  a3f27b…c91e  ✓
  Layer 1  [Delta]  55f10c…aa32  ✓
  Layer 2  [Delta]  9d3e84…2b17  ✓

All layers verified. File is intact.
```

**Example output (corruption detected):**

```
$ tcow verify agent.tcow

Verifying agent.tcow (3 layers)...

  Layer 0  [Base ]  a3f27b…c91e  ✓
  Layer 1  [Delta]  55f10c…aa32  ✗  MISMATCH (stored: 55f10c…aa32, computed: deadbe…ef01)
  Layer 2  [Delta]  (no digest)  -  SKIPPED

1 error(s) found.
```

---

### `layers`

Print a machine-readable or human-readable enumeration of all layers with their byte offsets, sizes, and metadata.

```
$ tcow layers --help
tcow-layers
List all layers in a .tcow file with byte offsets, sizes, and metadata.

USAGE:
    tcow layers [OPTIONS] <FILE>

ARGS:
    <FILE>    Path to the .tcow file

OPTIONS:
    --json    Emit output as a JSON array
    -h, --help    Print help information
```

**Example:**

```
$ tcow layers agent.tcow

#  Kind   Offset      Size        Digest                             Created
─  ─────  ──────────  ──────────  ─────────────────────────────────  ──────────────────────
0  Base        16 B   8,192 B     a3f27b4f9c8e1d2a3b4c5d6e7f8a9b0c  2026-02-28T12:00:00Z
1  Delta    8,208 B   4,096 B     55f10cab3d2e1f0a9b8c7d6e5f4a3b2c  2026-02-28T13:15:00Z
2  Delta   12,304 B  71,680 B     9d3e84c1b2a3f4e5d6c7b8a9f0e1d2c  2026-02-28T14:32:00Z
```

**JSON output:**

```sh
$ tcow layers --json agent.tcow
[
  { "index": 0, "kind": "Base",  "offset": 16,    "size": 8192,  "created_at": "2026-02-28T12:00:00Z", "digest": "a3f27b…" },
  { "index": 1, "kind": "Delta", "offset": 8208,  "size": 4096,  "created_at": "2026-02-28T13:15:00Z", "digest": "55f10c…" },
  { "index": 2, "kind": "Delta", "offset": 12304, "size": 71680, "created_at": "2026-02-28T14:32:00Z", "digest": "9d3e84…" }
]
```

---

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `TCOW_FILE` | _(none)_ | Default `.tcow` path; used when `-f` / `--file` is not given |
| `TCOW_COLOR` | `auto` | Color output: `auto`, `always`, `never` |
| `NO_COLOR` | _(unset)_ | Set to any value to disable color (standard convention) |
| `RUST_LOG` | `warn` | Log level for debug output (e.g. `tcow=debug`) |

---

## Exit Codes

| Code | Meaning |
|------|---------|
| `0`  | Success |
| `1`  | Generic error (I/O failure, bad arguments) |
| `2`  | File not a valid `.tcow` (bad magic, truncated trailer) |
| `3`  | Path not found in virtual filesystem |
| `4`  | Integrity check failed (`verify` subcommand) |

---

## Shell Completions

Generate shell completions using the built-in `completions` subcommand (implemented via `clap_complete`):

```sh
# bash
tcow completions bash >> ~/.bash_completion

# zsh
tcow completions zsh > ~/.zfunc/_tcow

# fish
tcow completions fish > ~/.config/fish/completions/tcow.fish
```

---

## Implementation Notes

- The CLI binary is defined at `src/bin/tcow.rs` and uses `clap` v4 with the derive API.
- It imports the `tcow` library crate (the same one linked into `main.rs`) so there is a single source of truth for format parsing.
- All subcommands that modify the file acquire an exclusive `flock` (Unix) or `LockFile` (Windows) on the `.tcow` path before writing.
- Color output uses the `termcolor` crate; `--color never` / `NO_COLOR` disables it.
- The `compact` subcommand streams layer-by-layer — it does not load the entire file into memory.

See [TCOW.md](TCOW.md) for the on-disk format specification.

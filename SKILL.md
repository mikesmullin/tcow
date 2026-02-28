# SKILL: tcow — Copy-on-Write Virtual Filesystem CLI

## Overview

`tcow` is a CLI tool that creates and inspects `.tcow` files — single-file virtual filesystems using a layered copy-on-write format. Files from lower layers are never modified; writes and deletions append new delta layers. A union view resolves the current visible state across all layers.

Binary location after `cargo build`: `target/debug/tcow` (or `target/release/tcow`).

---

## Subcommand Reference with Examples

### `insert` — Write a file into the virtual filesystem

```
# Create a new .tcow file with a single file (stdin)
echo "hello world" | tcow insert agent.tcow /hello.txt

# Insert from a host file
tcow insert agent.tcow /config/settings.json ./settings.json

# Dry run (no modification)
echo "test" | tcow insert --dry-run agent.tcow /test.txt
```

Output:
```
Created "agent.tcow" — inserted /hello.txt (12 bytes) into base layer 0
Inserted /config/settings.json (4096 bytes) into new delta layer 1
[DRY RUN] Would insert /test.txt (5 bytes) as new delta layer 2
```

---

### `ls` — List visible files

```
# Union view (default — respects whiteouts)
tcow ls agent.tcow

# Long format: size, mtime, layer index
tcow ls -L agent.tcow

# Single layer only (bypasses union view)
tcow ls --layer 0 agent.tcow

# All layers including hidden/deleted entries
tcow ls -a --show-whiteouts agent.tcow

# Filter by virtual path prefix
tcow ls agent.tcow /config/
```

Output (`tcow ls -L agent.tcow`):
```
        12 B  2026-02-28T12:00:00Z  layer  0  /hello.txt
      4096 B  2026-02-28T13:15:00Z  layer  1  /config/settings.json
```

Output (`tcow ls -a --show-whiteouts agent.tcow`):
```
  [hidden]  /hello.txt  (layer 0 — Base)
           /config/settings.json  (layer 1 — Delta)
  [DEL]  /hello.txt  (layer 2 — Delta)
```

---

### `cat` — Print file contents to stdout

```
# Union view (highest layer wins)
tcow cat agent.tcow /hello.txt

# Read from a specific layer (ignores whiteouts above it)
tcow cat --layer 0 agent.tcow /hello.txt

# Pipe into another tool
tcow cat agent.tcow /config/settings.json | jq .
```

---

### `stat` — File metadata

```
tcow stat agent.tcow /hello.txt
tcow stat --json agent.tcow /hello.txt
```

Output (human):
```
Path:     /hello.txt
Size:     12 bytes
Mtime:    2026-02-28T12:00:00Z
Layer:    0 (Base)
Whiteout: false
```

Output (`--json`):
```json
{"path":"/hello.txt","size":12,"mtime":"2026-02-28T12:00:00Z","layer":0,"whiteout":false}
```

---

### `delete` — Mark a file as deleted (whiteout)

```
# Appends a .wh. whiteout entry in a new delta layer
tcow delete agent.tcow /hello.txt

# Dry run
tcow delete --dry-run agent.tcow /hello.txt
```

Output:
```
Wrote whiteout for /hello.txt in new delta layer 2
[DRY RUN] Would write whiteout .wh.hello.txt in new delta layer 2
```

The original bytes remain in layer 0. The file disappears from the union view but can still be read with `cat --layer 0`.

---

### `info` — High-level summary

```
tcow info agent.tcow
```

Output:
```
File:          agent.tcow
Size:          9468 bytes
Format:        TCOW v1
Last modified: 2026-02-28T14:32:00Z
Layers:        5

  #    Kind    Offset        Size        Created             Digest
  ───  ──────  ────────────  ──────────  ──────────────────  ────────────────
  0    Base    16            2.0 KiB     2026-02-28T12:00:00Z  5d165eb6f10364c0…
  1    Delta   2064          2.0 KiB     2026-02-28T13:15:00Z  e388a693eae44263…
  ...

Union view: 2 file(s) visible
```

---

### `layers` — Layer table

```
tcow layers agent.tcow

# JSON output (for scripting)
tcow layers --json agent.tcow
```

Output (`--json`):
```json
[
  {"index":0,"kind":"Base","offset":16,"size":2048,"created_at":"2026-02-28T12:00:00Z","digest":"5d165eb6..."}
]
```

---

### `verify` — Integrity check

```
tcow verify agent.tcow

# Compute and write digests for any layer missing one
tcow verify --fix-missing agent.tcow
```

Output (all OK):
```
Verifying agent.tcow (3 layers)…

  Layer  0  [ Base]  5d165eb6f10364c0…  ✓
  Layer  1  [Delta]  e388a693eae44263…  ✓
  Layer  2  [Delta]  504e7b8ff81f0823…  ✓

All layers verified. File is intact.
```

Exit code `4` if any digest mismatch.

---

### `extract` — Write files to the host filesystem

```
# Extract all visible files to ./out/
tcow extract agent.tcow ./out/

# Only extract files under /config/
tcow extract --vpath /config/ agent.tcow ./out/

# Extract a single layer (bypasses union view)
tcow extract --layer 0 agent.tcow ./base-snapshot/

# Strip a path prefix from extracted names
tcow extract --strip-prefix /config agent.tcow --vpath /config/ ./out/
# Writes ./out/settings.json instead of ./out/config/settings.json

# Dry run
tcow extract --dry-run agent.tcow ./out/
```

Output:
```
Extracted 3 file(s) to ./out/
[DRY RUN] Would extract /hello.txt (12 bytes)
```

---

### `snapshot` — Seal current state as a checkpoint

```
tcow snapshot agent.tcow
tcow snapshot --label "after-step-3" agent.tcow
```

Output:
```
Snapshot created: layer 4 (Delta) at offset 7696
Snapshot created: layer 4 (Delta) "after-step-3" at offset 7696
```

Appends an empty delta layer. Subsequent writes land in a new layer 5.

---

### `compact` — Merge all layers into one

```
# Write to a new file (safe default)
tcow compact agent.tcow
# → creates agent.compacted.tcow

# Custom output path
tcow compact -o flat.tcow agent.tcow

# In-place (IRREVERSIBLE — keep a backup first)
tcow compact --in-place agent.tcow

# Dry run
tcow compact --dry-run agent.tcow
```

Output:
```
Compacted "agent.tcow" → "agent.compacted.tcow"
  Before: 5 layer(s), 9468 bytes
  After:  1 layer,  3296 bytes  (65% reduction)

[DRY RUN] Would compact 5 layers (9468 bytes) → ~1050 bytes
[DRY RUN] 2 file(s) would be preserved
```

Compaction resolves the full union view (whiteouts consumed), then writes a single Base layer with only the currently visible files.

---

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | Generic error (bad args, I/O failure) |
| `2` | Not a valid `.tcow` file |
| `3` | Virtual path not found |
| `4` | Integrity check failed (`verify`) |

---

## Common Patterns

```bash
# Round-trip: insert → read back
echo '{"key":"value"}' | tcow insert store.tcow /data.json
tcow cat store.tcow /data.json | jq .

# Overwrite a file (copy-up: lower layer unchanged)
echo "v2 content" | tcow insert store.tcow /data.json
tcow ls -a store.tcow   # shows v1 as [hidden], v2 as current

# Inspect a .tcow file produced by the wasm agent
tcow info agent.tcow
tcow ls -L agent.tcow
tcow cat agent.tcow /output/result.json

# Checkpoint mid-run, then continue writing
tcow snapshot agent.tcow
echo "step4 output" | tcow insert agent.tcow /steps/4.txt

# Flatten for distribution
tcow compact -o agent-flat.tcow agent.tcow
tcow verify agent-flat.tcow
```

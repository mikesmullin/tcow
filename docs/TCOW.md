# TCOW File Format

**Version** — 1.0
**Date** — February 2026

---

## 1. Overview

A `.tcow` file is a **single-file virtual filesystem** with **copy-on-write (CoW)** semantics, inspired by Docker's OverlayFS layer model. Unlike OverlayFS, which spans multiple directories on a real host filesystem, a `.tcow` file packs all layers into one portable binary artifact that can be inspected, diffed, and manipulated with the standalone `tcow` CLI.

### Design Principles

- **Single file** — one `.tcow` file = complete filesystem snapshot history.
- **Append-friendly** — new writes append data; nothing is overwritten in place.
- **Immutable lower layers** — only the topmost (writable) layer changes.
- **Tar-based layer storage** — each layer is a standard POSIX [ustar](https://pubs.opengroup.org/onlinepubs/9699919799/utilities/pax.html#tag_20_92_13_06) tar stream so it can be read by standard `tar` tooling.
- **CBOR trailer index** — a compact binary index at the end of the file lets readers locate all layer boundaries in O(1) without scanning.
- **Docker-style whiteouts** — file deletions recorded as `.wh.`-prefixed zero-byte tar entries.

---

## 2. High-Level File Layout

```
┌─────────────────────────────────────────────────────────────────┐
│  .tcow file                                                     │
│                                                                 │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  File Header  (16 bytes, fixed)                         │   │
│  │  magic[4]  version[2]  flags[2]  reserved[8]            │   │
│  └─────────────────────────────────────────────────────────┘   │
│                                                                 │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  Layer 0  (uncompressed ustar tar stream)               │   │
│  │  ── tar entry: fileA.txt ──────────────────────────     │   │
│  │  ── tar entry: dir/fileB.txt ──────────────────────     │   │
│  │  ── tar entry: config.json ────────────────────────     │   │
│  │  ── end-of-archive (two 512-byte zero blocks) ──────    │   │
│  └─────────────────────────────────────────────────────────┘   │
│                                                                 │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  Layer 1  (delta / writable layer)                      │   │
│  │  ── tar entry: fileA.txt (copy-up + modified) ──────    │   │
│  │  ── tar entry: .wh.dir/fileB.txt (whiteout) ────────    │   │
│  │  ── tar entry: newfile.txt ────────────────────────     │   │
│  │  ── end-of-archive ────────────────────────────────     │   │
│  └─────────────────────────────────────────────────────────┘   │
│                                                                 │
│  ┌─  ·  ·  ·  (additional layers)  ·  ·  ·  ─────────────┐   │
│  └─────────────────────────────────────────────────────────┘   │
│                                                                 │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  CBOR Trailer  (variable length)                        │   │
│  └─────────────────────────────────────────────────────────┘   │
│                                                                 │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  Trailer Footer  (16 bytes, fixed)                      │   │
│  │  trailer_offset[8]  trailer_len[4]  magic_tail[4]       │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

A reader can locate the index by seeking to the last 16 bytes of the file, reading `trailer_offset` and `trailer_len`, and then seeking to that offset to parse the CBOR blob.

---

## 3. File Header (16 bytes)

The file always begins with a fixed 16-byte header.

```
Offset  Size  Field          Value / Notes
──────  ────  ─────────────  ──────────────────────────────────────────
0       4     magic          b"TCOW"  (0x54 0x43 0x4F 0x57)
4       2     version        0x0001  (little-endian u16)
6       2     flags          bit 0: has_base_layer
                             bits 1-15: reserved, must be 0
8       8     reserved       all zeros
```

### Byte diagram

```
 0    1    2    3    4    5    6    7    8    9   10   11   12   13   14   15
┌────┬────┬────┬────┬────┬────┬────┬────┬────┬────┬────┬────┬────┬────┬────┬────┐
│ T  │ C  │ O  │ W  │ 01 │ 00 │ fl │ ags│    reserved (8 bytes)               │
└────┴────┴────┴────┴────┴────┴────┴────┴────┴────┴────┴────┴────┴────┴────┴────┘
```

---

## 4. Layers

### 4.1 What is a Layer?

A layer is a **complete, self-contained ustar tar stream** with the standard 512-byte block structure. Each layer holds zero or more file entries followed by the two 512-byte zero-filled end-of-archive blocks that POSIX tar requires.

Layers are stored **sequentially** in the file, starting immediately after the 16-byte header. There is no padding between layers.

### 4.2 Layer Roles

| Layer index | Role | Mutability |
|-------------|------|-----------|
| 0 | Base layer — initial filesystem state | Immutable after first write |
| 1…N-1 | Delta layers — accumulated snapshots | Immutable |
| N (highest) | Writable layer — active changes | Mutable (in-memory; flushed on close) |

The writable layer exists **only in memory** during an active process. On flush (`TcowFs::flush()`), the in-memory writable layer is serialised as a ustar tar stream and appended to the file, then the CBOR trailer is rewritten to reference the new layer.

### 4.3 tar Entry Structure

Each file within a layer is stored as a standard **ustar** tar entry:

```
┌───────────────────────────────────────────────────┐
│  ustar Header Block  (512 bytes)                  │
│                                                   │
│  name[100]    ← file path (NUL-terminated)        │
│  mode[8]      ← octal permissions                 │
│  uid[8]       ← user ID (octal)                   │
│  gid[8]       ← group ID (octal)                  │
│  size[12]     ← file size in bytes (octal)        │
│  mtime[12]    ← modification time (octal, unix)   │
│  checksum[8]                                      │
│  typeflag[1]  ← '0'=regular, '5'=directory,       │
│                  '0'=whiteout (size=0)             │
│  linkname[100]                                    │
│  magic[6]     ← "ustar"                           │
│  version[2]   ← "00"                              │
│  uname[32]                                        │
│  gname[32]                                        │
│  devmajor[8]                                      │
│  devminor[8]                                      │
│  prefix[155]  ← path prefix for long names        │
│  pad[12]                                          │
└───────────────────────────────────────────────────┘
┌───────────────────────────────────────────────────┐
│  File content (rounded up to 512-byte boundary)   │
└───────────────────────────────────────────────────┘
```

The Rust `tar` crate handles this encoding/decoding transparently via `tar::Builder` and `tar::Archive`.

---

## 5. Whiteout Entries

File deletions are encoded as **Docker-compatible whiteout entries**: a zero-byte regular tar file (`typeflag = '0'`, `size = 0`) whose basename is `.wh.<original-basename>` in the same parent directory.

### Examples

| Deleted path | Whiteout entry name |
|---|---|
| `config.json` | `.wh.config.json` |
| `data/records.db` | `data/.wh.records.db` |
| `app/bin/server` | `app/bin/.wh.server` |

### Union View Algorithm

When resolving a path `P` across layers (highest index first):

```
for layer in layers.iter().rev():
    if layer contains whiteout_for(P):
        return NotFound
    if layer contains entry for P:
        return that entry
return NotFound
```

### Opaque Whiteout (future)

A special entry named `.wh..wh..opq` in a directory causes the entire directory from lower layers to be treated as if it does not exist. This is reserved for a future version.

---

## 6. CBOR Trailer

The CBOR trailer is the **index** that allows a reader to locate every layer without scanning the entire file. It is parsed using the `ciborium` crate.

### 6.1 Trailer Schema (Rust)

```rust
/// Top-level CBOR document stored in the trailer.
#[derive(Serialize, Deserialize)]
struct TcowIndex {
    /// Format version for the index itself.
    version: u16,
    /// Ordered list of layers, from base (index 0) to most-recent delta.
    layers: Vec<LayerRecord>,
    /// Timestamp when this trailer was last written (RFC 3339).
    last_modified: String,
    /// Human-readable label (optional, e.g. agent run ID).
    label: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct LayerRecord {
    /// Byte offset from the start of the .tcow file where this layer begins.
    offset: u64,
    /// Uncompressed byte length of the complete tar stream for this layer.
    size: u64,
    /// Role of this layer.
    kind: LayerKind,
    /// SHA-256 digest of the raw tar bytes (hex string, optional).
    digest: Option<String>,
    /// Timestamp when this layer was created (RFC 3339).
    created_at: String,
}

#[derive(Serialize, Deserialize)]
enum LayerKind {
    Base,   // layer 0
    Delta,  // layers 1..N-1
}
```

### 6.2 CBOR Wire Layout (annotated)

```
Map(5)
  "version"       → Integer(1)
  "layers"        → Array(N)
    Map(5)                          ← LayerRecord for layer 0
      "offset"     → Integer(16)   ← right after file header
      "size"       → Integer(8192)
      "kind"       → Text("Base")
      "digest"     → Text("a3f2...") or Null
      "created_at" → Text("2026-02-28T12:00:00Z")
    Map(5)                          ← LayerRecord for layer 1
      "offset"     → Integer(8208)
      "size"       → Integer(4096)
      "kind"       → Text("Delta")
      ...
  "last_modified" → Text("2026-02-28T14:32:00Z")
  "label"         → Text("run-abc123") or Null
```

### 6.3 Trailer Footer (16 bytes)

The last 16 bytes of every valid `.tcow` file:

```
Offset from EOF  Size  Field            Notes
───────────────  ────  ───────────────  ──────────────────────────────────────
-16              8     trailer_offset   Byte offset of CBOR blob (little-endian u64)
-8               4     trailer_len      Byte length of CBOR blob (little-endian u32)
-4               4     magic_tail       b"W0CT"  (reverse of "TCW0" — "TCow end")
```

```
         ... CBOR bytes ...
┌──────────────────┬──────────────┬──────────┐
│ trailer_offset   │ trailer_len  │ magic    │
│ (8 bytes, LE u64)│ (4 bytes,LE) │ "W0CT"   │
└──────────────────┴──────────────┴──────────┘
         ▲ last 16 bytes of file
```

A reader opens the file, seeks to `-16` from the end, verifies `magic_tail == b"W0CT"`, then seeks to `trailer_offset` and reads exactly `trailer_len` bytes to obtain the CBOR document.

---

## 7. File Offset Map (example)

For a `.tcow` file with a base layer (8 KiB of tar) and one delta layer (2 KiB of tar), the byte layout looks like:

```
Offset        Size    Content
──────────    ──────  ──────────────────────────────────────────────────
0             16      File header (magic, version, flags, reserved)
16            8192    Layer 0 tar stream (base)
8208          2048    Layer 1 tar stream (delta)
10256         312     CBOR trailer (TcowIndex, variable)
10568         16      Trailer footer (offset=10256, len=312, magic)
──────────────────────────────────────────────────────────────────────
Total         10584 bytes
```

---

## 8. Read Path (Union View)

```
fs_read("/data/config.json")
         │
         ▼
  Iterate layers from highest index to 0
         │
  Layer N ──► look for "data/.wh.config.json"
         │         found?  → return NotFound
         │    look for "data/config.json"
         │         found?  → return bytes
         │         not found → continue
         ▼
  Layer N-1 ── same logic
         │
        ...
         ▼
  Layer 0 ──► same logic
         │
  None found → return NotFound (-1)
```

The in-memory writable layer (not yet flushed) is always checked first, before any on-disk layer.

---

## 9. Write Path (Copy-Up)

```
fs_write("/data/config.json", new_bytes)
         │
         ▼
  Does "data/config.json" exist in the writable layer (in memory)?
  ├── YES → overwrite in writable layer buffer (old entry shadowed)
  └── NO  → does it exist in any read-only layer?
                ├── NO  → create new entry in writable layer buffer
                └── YES → copy-up:
                      1. read original bytes from lower layer
                      2. create new tar entry in writable layer buffer
                         with new_bytes (lower layer is NOT modified)
```

The writable layer is an in-memory `Vec<TarEntry>`. On flush, entries are serialised in order into a ustar tar stream and appended to the `.tcow` file.

Because a single path can appear multiple times in the writable layer's in-memory buffer (written then overwritten), a deduplication pass during flush keeps only the **last** entry for each path before writing to disk.

---

## 10. Snapshot / Layer Sealing

Snapshots freeze the current writable layer into an immutable delta and open a new empty writable layer. This is done externally via the `tcow snapshot` CLI command (or will be available as an `fs_snapshot` host function in a future version).

```
Before snapshot:
  [Layer 0: base] [Layer 1: delta] [writable buffer in memory]

tcow snapshot agent.tcow:
  1. Flush writable buffer → append Layer 2 tar stream to file
  2. Append new (empty) CBOR trailer referencing Layers 0, 1, 2
  3. Rewrite trailer footer

After snapshot:
  [Layer 0: base] [Layer 1: delta] [Layer 2: sealed delta]
  writable buffer = empty (Layer 3, not yet on disk)
```

---

## 11. Compaction

Over time, copy-ups and overwrites accumulate unreachable bytes in lower layers. Compaction merges all layers into a single base layer containing only the current visible state.

```
Before compaction:
  Layer 0: fileA(v1), fileB(v1), fileC(v1)
  Layer 1: fileA(v2), .wh.fileB
  Layer 2: fileC(v2), fileD(v1)

After compaction:
  Layer 0: fileA(v2), fileC(v2), fileD(v1)
  (fileB is gone — whiteout consumed)
```

Compaction is a CLI-only operation (`tcow compact`) and never happens automatically during a live agent run. See [FS_CLI.md](FS_CLI.md).

---

## 12. Integrity

- **Per-layer digest** — each `LayerRecord` in the CBOR trailer optionally contains a SHA-256 hex digest of the raw tar bytes. The `tcow verify` command checks these digests.
- **Trailer magic** — both the file header magic (`TCOW`) and footer magic (`W0CT`) serve as sanity checks against truncation or corruption.
- **No encryption** — `.tcow` files are plaintext. Encryption is out of scope for v1.

---

## 13. Worked Example

### Starting state (empty agent.tcow)

```
agent.tcow
├── [Header]  magic=TCOW version=1
├── [Layer 0] (base, empty — just two 512-byte zero blocks)
├── [CBOR]    { layers: [{offset:16, size:1024, kind:"Base"}] }
└── [Footer]  trailer_offset=1040 trailer_len=64 magic=W0CT
```

### After guest writes `/thoughts/step1.md`

Writable layer (in memory):
```
entry: thoughts/step1.md  (512-byte header + content blocks)
```

On flush:
```
agent.tcow
├── [Header]
├── [Layer 0]  (base, empty)
├── [Layer 1]  thoughts/step1.md
├── [CBOR]     { layers: [{...base...}, {offset:1040, size:2048, kind:"Delta"}] }
└── [Footer]
```

### After guest deletes `/thoughts/step1.md` and writes `/output.json`

Writable layer (in memory):
```
entry: thoughts/.wh.step1.md  (whiteout)
entry: output.json
```

On flush:
```
agent.tcow
├── [Header]
├── [Layer 0]  (empty base)
├── [Layer 1]  thoughts/step1.md
├── [Layer 2]  thoughts/.wh.step1.md  output.json
├── [CBOR]     { layers: [{...}, {...}, {offset:3088, size:3072, kind:"Delta"}] }
└── [Footer]

Visible filesystem union:
  output.json  ← from Layer 2
  (thoughts/step1.md is hidden by whiteout in Layer 2)
```

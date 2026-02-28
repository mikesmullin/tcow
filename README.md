# tcow

A copy-on-write virtual filesystem in a single portable file.

A `.tcow` file stores a layered stack of tar streams, inspired by Docker's OverlayFS model. Each write appends a new delta layer instead of mutating earlier ones. A compact CBOR trailer at the end of the file indexes all layer boundaries so the format can be read in O(1) without scanning.

## Features

- **Single-file portability** — one `.tcow` file contains the entire filesystem history as a binary artifact.
- **Append-only writes** — existing layer bytes are never modified; new deltas are appended.
- **Docker-style whiteout deletion** — files are deleted by writing a `.wh.`-prefixed zero-byte entry in the next delta layer, leaving lower layers intact.
- **Copy-up semantics** — the first write to a file from a lower layer promotes a copy into the active writable layer.
- **Union view** — reads resolve from the highest (most recent) layer downward; whiteouts shadow lower entries transparently.
- **CBOR trailer index** — layer offsets and SHA-256 digests are stored in a compact binary index at the tail of the file.
- **Snapshot / compaction** — checkpoint the current state as a sealed delta, or merge all layers into a single flat base layer.

## Format at a glance

```
[ 16-byte header: magic "TCOW" + version + flags ]
[ Layer 0: ustar tar stream  (Base — immutable)   ]
[ Layer 1: ustar tar stream  (Delta — immutable)  ]
[ ...                                              ]
[ Layer N: ustar tar stream  (Delta — current)    ]
[ CBOR trailer: TcowIndex with layer records      ]
[ 16-byte footer: trailer_offset + trailer_len + magic "W0CT" ]
```

See [docs/TCOW.md](docs/TCOW.md) in the parent project for the full format specification.

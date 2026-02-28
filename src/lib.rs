use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ── File-format constants ─────────────────────────────────────────────────────

pub const MAGIC: &[u8; 4] = b"TCOW";
pub const MAGIC_TAIL: &[u8; 4] = b"W0CT";
pub const FORMAT_VERSION: u16 = 1;
pub const HEADER_SIZE: u64 = 16;
pub const FOOTER_SIZE: u64 = 16;
pub const FLAG_HAS_BASE: u16 = 0x0001;

// ── CBOR index structures ─────────────────────────────────────────────────────

/// Top-level CBOR document stored in the trailer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcowIndex {
    pub version: u16,
    /// Ordered list of layers, from base (index 0) to most-recent delta.
    pub layers: Vec<LayerRecord>,
    pub last_modified: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerRecord {
    pub offset: u64,
    pub size: u64,
    /// "Base" or "Delta"
    pub kind: String,
    pub digest: Option<String>,
    pub created_at: String,
}

// ── In-memory layer entry ─────────────────────────────────────────────────────

/// One entry as parsed from a tar layer.
/// All paths are stored without a leading `/`.
#[derive(Debug, Clone)]
pub struct RawEntry {
    pub data: Vec<u8>,
    pub mtime: u64,
    /// True when this entry is a whiteout marker (deletion).
    pub is_whiteout: bool,
    pub is_dir: bool,
}

/// An entry resolved through the full union view.
#[derive(Debug, Clone)]
pub struct ResolvedEntry {
    pub data: Vec<u8>,
    pub mtime: u64,
    pub layer_idx: usize,
    pub size: u64,
}

// ── TcowFile ──────────────────────────────────────────────────────────────────

/// An open .tcow file with all layers parsed into memory.
pub struct TcowFile {
    pub path: std::path::PathBuf,
    pub index: TcowIndex,
    /// Entries for each layer, keyed by canonical path (no leading `/`).
    /// Whiteout entries are stored under the *real* (non-`.wh.`) path with
    /// `is_whiteout = true`.
    pub layers: Vec<HashMap<String, RawEntry>>,
}

impl TcowFile {
    // ── Open ──────────────────────────────────────────────────────────────────

    /// Open and parse an existing `.tcow` file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut f = File::open(&path)
            .with_context(|| format!("cannot open {:?}", path))?;

        // Validate header
        let mut hdr = [0u8; 16];
        f.read_exact(&mut hdr).context("reading TCOW file header")?;
        if &hdr[0..4] != MAGIC {
            bail!("{:?} is not a .tcow file: bad magic bytes", path);
        }
        let version = u16::from_le_bytes([hdr[4], hdr[5]]);
        if version != 1 {
            bail!("unsupported TCOW version {}", version);
        }

        // Read footer (last 16 bytes)
        let file_len = f.seek(SeekFrom::End(0))?;
        if file_len < HEADER_SIZE + FOOTER_SIZE {
            bail!("file too small to be a valid .tcow");
        }
        f.seek(SeekFrom::End(-(FOOTER_SIZE as i64)))?;
        let mut footer = [0u8; 16];
        f.read_exact(&mut footer)?;
        if &footer[12..16] != MAGIC_TAIL {
            bail!("bad footer magic — file may be truncated or corrupt");
        }
        let trailer_offset = u64::from_le_bytes(footer[0..8].try_into().unwrap());
        let trailer_len = u32::from_le_bytes(footer[8..12].try_into().unwrap());

        // Parse CBOR trailer
        f.seek(SeekFrom::Start(trailer_offset))?;
        let mut cbor_bytes = vec![0u8; trailer_len as usize];
        f.read_exact(&mut cbor_bytes).context("reading CBOR trailer")?;
        let index: TcowIndex = ciborium::from_reader(Cursor::new(&cbor_bytes))
            .map_err(|e| anyhow!("invalid CBOR trailer: {e}"))?;

        // Parse each layer's tar stream
        let mut layers = Vec::with_capacity(index.layers.len());
        for record in &index.layers {
            f.seek(SeekFrom::Start(record.offset))?;
            let mut layer_bytes = vec![0u8; record.size as usize];
            f.read_exact(&mut layer_bytes)?;
            let entries = parse_tar_layer(&layer_bytes)
                .with_context(|| format!("parsing layer at offset {}", record.offset))?;
            layers.push(entries);
        }

        Ok(TcowFile { path, index, layers })
    }

    // ── Create ────────────────────────────────────────────────────────────────

    /// Create a brand-new `.tcow` file with a single Base layer.
    pub fn create(
        path: impl AsRef<Path>,
        entries: &[(String, Vec<u8>)],
        whiteouts: &[String],
        label: Option<String>,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut f = File::create(&path)
            .with_context(|| format!("cannot create {:?}", path))?;

        let now = now_rfc3339();
        let has_content = !entries.is_empty() || !whiteouts.is_empty();
        write_file_header(&mut f, has_content)?;

        // Build & write base tar layer
        let layer_bytes = build_tar_layer(entries, whiteouts)?;
        let digest = sha256_hex(&layer_bytes);
        let layer_offset = HEADER_SIZE;
        let layer_size = layer_bytes.len() as u64;
        f.write_all(&layer_bytes)?;

        let index = TcowIndex {
            version: 1,
            layers: vec![LayerRecord {
                offset: layer_offset,
                size: layer_size,
                kind: "Base".into(),
                digest: Some(digest),
                created_at: now.clone(),
            }],
            last_modified: now,
            label,
        };

        let trailer_offset = layer_offset + layer_size;
        let cbor_bytes = encode_cbor(&index)?;
        let trailer_len = cbor_bytes.len() as u32;
        f.write_all(&cbor_bytes)?;
        write_trailer_footer(&mut f, trailer_offset, trailer_len)?;
        f.flush()?;

        let layer_entries = parse_tar_layer(&layer_bytes)?;
        Ok(TcowFile { path, index, layers: vec![layer_entries] })
    }

    // ── Append delta ──────────────────────────────────────────────────────────

    /// Append a new Delta layer to an existing `.tcow` file.
    /// Truncates the old trailer+footer, writes new tar + trailer + footer.
    pub fn append_delta(
        path: impl AsRef<Path>,
        entries: &[(String, Vec<u8>)],
        whiteouts: &[String],
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        // Parse current state to get the index
        let existing = TcowFile::open(&path)?;

        // Locate the old trailer offset from the footer
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .context("opening file for writing delta")?;

        f.seek(SeekFrom::End(-(FOOTER_SIZE as i64)))?;
        let mut footer_buf = [0u8; 16];
        f.read_exact(&mut footer_buf)?;
        let old_trailer_offset = u64::from_le_bytes(footer_buf[0..8].try_into().unwrap());

        // Truncate at old trailer, overwrite from there
        f.set_len(old_trailer_offset)?;
        f.seek(SeekFrom::Start(old_trailer_offset))?;

        // Build and write new delta tar stream
        let layer_bytes = build_tar_layer(entries, whiteouts)?;
        let digest = sha256_hex(&layer_bytes);
        let delta_offset = old_trailer_offset;
        let delta_size = layer_bytes.len() as u64;
        f.write_all(&layer_bytes)?;

        // Build updated index
        let now = now_rfc3339();
        let mut index = existing.index.clone();
        index.layers.push(LayerRecord {
            offset: delta_offset,
            size: delta_size,
            kind: "Delta".into(),
            digest: Some(digest),
            created_at: now.clone(),
        });
        index.last_modified = now;

        // Write new CBOR trailer + footer
        let new_trailer_offset = delta_offset + delta_size;
        let cbor_bytes = encode_cbor(&index)?;
        let new_trailer_len = cbor_bytes.len() as u32;
        f.write_all(&cbor_bytes)?;
        write_trailer_footer(&mut f, new_trailer_offset, new_trailer_len)?;
        f.flush()?;

        let new_layer_entries = parse_tar_layer(&layer_bytes)?;
        let mut all_layers = existing.layers;
        all_layers.push(new_layer_entries);

        Ok(TcowFile { path, index, layers: all_layers })
    }

    // ── Union view ────────────────────────────────────────────────────────────

    /// Compute the union view: the set of currently visible files.
    /// Iterates layers from highest (most recent) to lowest; whiteouts shadow
    /// same-named entries in lower layers.
    pub fn union_view(&self) -> HashMap<String, ResolvedEntry> {
        let mut result: HashMap<String, ResolvedEntry> = HashMap::new();
        let mut deleted: HashSet<String> = HashSet::new();

        for (layer_idx, layer_entries) in self.layers.iter().enumerate().rev() {
            for (path, entry) in layer_entries {
                if entry.is_whiteout {
                    deleted.insert(path.clone());
                } else if !deleted.contains(path) && !result.contains_key(path) && !entry.is_dir {
                    result.insert(
                        path.clone(),
                        ResolvedEntry {
                            data: entry.data.clone(),
                            mtime: entry.mtime,
                            layer_idx,
                            size: entry.data.len() as u64,
                        },
                    );
                }
            }
        }
        result
    }

    /// Resolve a single virtual path through the union view.
    pub fn resolve(&self, vpath: &str) -> Option<(ResolvedEntry, usize)> {
        let canonical = normalize_path(vpath);
        let view = self.union_view();
        view.get(&canonical).map(|e| (e.clone(), e.layer_idx))
    }

    /// Count of visible files in the union view.
    pub fn visible_count(&self) -> usize {
        self.union_view().len()
    }
}

// ── Path helpers ──────────────────────────────────────────────────────────────

/// Strip leading `/` and ensure consistent internal representation.
pub fn normalize_path(p: &str) -> String {
    p.trim_start_matches('/').to_string()
}

/// `"data/records.db"` → `"data/.wh.records.db"` (tar entry name for whiteout)
pub fn to_whiteout_tar_path(canonical: &str) -> String {
    if let Some(pos) = canonical.rfind('/') {
        format!("{}/.wh.{}", &canonical[..pos], &canonical[pos + 1..])
    } else {
        format!(".wh.{}", canonical)
    }
}

/// `"data/.wh.records.db"` → `Some("data/records.db")`, or `None` if not a whiteout.
pub fn from_whiteout_tar_path(path: &str) -> Option<String> {
    let filename = path.split('/').next_back()?;
    if filename.starts_with(".wh.") && !filename.starts_with(".wh..wh.") {
        let real_name = &filename[4..];
        if let Some(slash_pos) = path.rfind('/') {
            Some(format!("{}/{}", &path[..slash_pos], real_name))
        } else {
            Some(real_name.to_string())
        }
    } else {
        None
    }
}

// ── Tar helpers ───────────────────────────────────────────────────────────────

/// Parse a raw ustar tar byte stream into a map of canonical_path → RawEntry.
pub fn parse_tar_layer(data: &[u8]) -> Result<HashMap<String, RawEntry>> {
    let mut entries: HashMap<String, RawEntry> = HashMap::new();
    let cursor = Cursor::new(data);
    let mut archive = tar::Archive::new(cursor);

    for entry_res in archive.entries()? {
        let mut entry = entry_res.context("reading tar entry")?;
        let raw_path = entry.path()?.to_string_lossy().to_string();
        let path = raw_path.trim_start_matches('/').to_string();

        let mtime = entry.header().mtime().unwrap_or(0);
        let is_dir = entry.header().entry_type().is_dir();

        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;

        if let Some(real_path) = from_whiteout_tar_path(&path) {
            // Whiteout: store under the real path with is_whiteout=true
            entries.insert(
                real_path,
                RawEntry { data: Vec::new(), mtime, is_whiteout: true, is_dir: false },
            );
        } else {
            entries.insert(path, RawEntry { data, mtime, is_whiteout: false, is_dir });
        }
    }
    Ok(entries)
}

/// Serialise a set of file entries + whiteout paths into a ustar tar byte stream.
pub fn build_tar_layer(entries: &[(String, Vec<u8>)], whiteouts: &[String]) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut buf);
        let ts = now_unix_ts();

        for (vpath, data) in entries {
            let path = normalize_path(vpath);
            let mut hdr = tar::Header::new_ustar();
            hdr.set_path(&path)?;
            hdr.set_size(data.len() as u64);
            hdr.set_mtime(ts);
            hdr.set_mode(0o644);
            hdr.set_cksum();
            builder.append(&hdr, Cursor::new(data))?;
        }

        for canonical in whiteouts {
            let canonical = normalize_path(canonical);
            let wh_path = to_whiteout_tar_path(&canonical);
            let mut hdr = tar::Header::new_ustar();
            hdr.set_path(&wh_path)?;
            hdr.set_size(0);
            hdr.set_mtime(ts);
            hdr.set_mode(0o644);
            hdr.set_cksum();
            builder.append(&hdr, Cursor::new(&[][..]))?;
        }

        builder.finish()?;
    }
    Ok(buf)
}

// ── CBOR helpers ──────────────────────────────────────────────────────────────

pub fn encode_cbor(index: &TcowIndex) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    ciborium::into_writer(index, &mut buf)
        .map_err(|e| anyhow!("CBOR encode error: {e}"))?;
    Ok(buf)
}

// ── Binary format helpers ─────────────────────────────────────────────────────

pub fn write_file_header(w: &mut impl Write, has_base: bool) -> Result<()> {
    let mut hdr = [0u8; 16];
    hdr[0..4].copy_from_slice(MAGIC);
    hdr[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    let flags: u16 = if has_base { FLAG_HAS_BASE } else { 0 };
    hdr[6..8].copy_from_slice(&flags.to_le_bytes());
    // bytes 8..16 are reserved zeros
    w.write_all(&hdr)?;
    Ok(())
}

pub fn write_trailer_footer(w: &mut impl Write, trailer_offset: u64, trailer_len: u32) -> Result<()> {
    let mut footer = [0u8; 16];
    footer[0..8].copy_from_slice(&trailer_offset.to_le_bytes());
    footer[8..12].copy_from_slice(&trailer_len.to_le_bytes());
    footer[12..16].copy_from_slice(MAGIC_TAIL);
    w.write_all(&footer)?;
    Ok(())
}

// ── Digest ────────────────────────────────────────────────────────────────────

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

// ── Timestamp utilities ───────────────────────────────────────────────────────

pub fn now_rfc3339() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

pub fn now_unix_ts() -> u64 {
    Utc::now().timestamp() as u64
}

pub fn unix_ts_to_rfc3339(ts: u64) -> String {
    Utc.timestamp_opt(ts as i64, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".into())
}

pub fn format_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KiB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0))
    }
}

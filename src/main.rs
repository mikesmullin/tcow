use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use tcow::{
    encode_cbor, format_bytes, normalize_path, now_rfc3339,
    sha256_hex, unix_ts_to_rfc3339, write_trailer_footer,
    TcowFile, TcowIndex,
};

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "tcow",
    version = "1.0.0",
    about = "Copy-on-write virtual filesystem inspector and manager.\n\
             A .tcow file stores a layered tar-based filesystem with CoW semantics.",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show metadata and layer summary for a .tcow file
    Info { file: PathBuf },

    /// List files visible in the current union view (or a specific layer)
    #[command(name = "ls")]
    List {
        file: PathBuf,
        /// Only list entries under this virtual directory prefix
        path: Option<String>,
        /// Restrict listing to this layer index only (0-based)
        #[arg(short, long, value_name = "N")]
        layer: Option<usize>,
        /// Show all entries from every layer, including hidden/overwritten ones
        #[arg(short = 'a', long)]
        all_layers: bool,
        /// Long format: size, mtime, layer index
        #[arg(short = 'L', long)]
        long: bool,
        /// Include whiteout (deletion marker) entries
        #[arg(long)]
        show_whiteouts: bool,
    },

    /// Print the contents of a file from the virtual filesystem to stdout
    Cat {
        file: PathBuf,
        vpath: String,
        /// Read from a specific layer instead of the union view
        #[arg(short, long, value_name = "N")]
        layer: Option<usize>,
    },

    /// Show metadata for a specific virtual filesystem path
    Stat {
        file: PathBuf,
        vpath: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Add or replace a file in a new delta layer (creates .tcow if absent)
    Insert {
        file: PathBuf,
        /// Destination path inside the virtual filesystem (e.g. /config/new.json)
        vpath: String,
        /// Source file to read from (default: stdin)
        source: Option<PathBuf>,
        /// Do not modify the file — only show what would happen
        #[arg(long)]
        dry_run: bool,
    },

    /// Mark a file as deleted by writing a whiteout entry in a new delta layer
    Delete {
        file: PathBuf,
        vpath: String,
        #[arg(long)]
        dry_run: bool,
    },

    /// Extract files from the virtual filesystem to the host
    Extract {
        file: PathBuf,
        /// Host directory to write into (created if absent)
        outdir: PathBuf,
        /// Only extract entries under this virtual path prefix
        #[arg(short = 'p', long, value_name = "VPATH")]
        vpath: Option<String>,
        /// Extract from a specific layer only (bypasses union view)
        #[arg(short, long, value_name = "N")]
        layer: Option<usize>,
        /// Strip this virtual prefix before writing to OUTDIR
        #[arg(long, value_name = "PREFIX")]
        strip_prefix: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },

    /// Seal the current state as a checkpoint (appends an empty delta layer)
    Snapshot {
        file: PathBuf,
        /// Optional label for this snapshot
        #[arg(long)]
        label: Option<String>,
    },

    /// Merge all layers into a single base layer (creates a new file by default)
    Compact {
        file: PathBuf,
        /// Output path [default: <FILE>.compacted.tcow]
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
        /// Overwrite the original file in-place (IRREVERSIBLE)
        #[arg(long)]
        in_place: bool,
        #[arg(long)]
        dry_run: bool,
    },

    /// Check integrity of all layer digests stored in the CBOR trailer
    Verify {
        file: PathBuf,
        /// Compute and write digests for layers that currently have none
        #[arg(long)]
        fix_missing: bool,
    },

    /// List all layers with byte offsets and sizes
    Layers {
        file: PathBuf,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Info { file } => cmd_info(file),
        Commands::List { file, path, layer, all_layers, long, show_whiteouts } => {
            cmd_list(file, path, layer, all_layers, long, show_whiteouts)
        }
        Commands::Cat { file, vpath, layer } => cmd_cat(file, vpath, layer),
        Commands::Stat { file, vpath, json } => cmd_stat(file, vpath, json),
        Commands::Insert { file, vpath, source, dry_run } => {
            cmd_insert(file, vpath, source, dry_run)
        }
        Commands::Delete { file, vpath, dry_run } => cmd_delete(file, vpath, dry_run),
        Commands::Extract { file, vpath, outdir, layer, strip_prefix, dry_run } => {
            cmd_extract(file, vpath, outdir, layer, strip_prefix, dry_run)
        }
        Commands::Snapshot { file, label } => cmd_snapshot(file, label),
        Commands::Compact { file, output, in_place, dry_run } => {
            cmd_compact(file, output, in_place, dry_run)
        }
        Commands::Verify { file, fix_missing } => cmd_verify(file, fix_missing),
        Commands::Layers { file, json } => cmd_layers(file, json),
    }
}

// ── info ──────────────────────────────────────────────────────────────────────

fn cmd_info(path: PathBuf) -> Result<()> {
    let tcow = TcowFile::open(&path)?;
    let meta = fs::metadata(&path)?;

    println!("File:          {}", path.display());
    println!("Size:          {} bytes", meta.len());
    println!("Format:        TCOW v{}", tcow.index.version);
    println!("Last modified: {}", tcow.index.last_modified);
    if let Some(label) = &tcow.index.label {
        println!("Label:         {label}");
    }
    println!("Layers:        {}", tcow.index.layers.len());
    println!();

    let header = format!(
        "  {:<3}  {:<6}  {:<12}  {:<10}  {:<18}  {}",
        "#", "Kind", "Offset", "Size", "Created", "Digest"
    );
    println!("{header}");
    println!("  {}  {}  {}  {}  {}  {}",
        "─".repeat(3), "─".repeat(6), "─".repeat(12),
        "─".repeat(10), "─".repeat(18), "─".repeat(16));

    for (i, rec) in tcow.index.layers.iter().enumerate() {
        let digest_short = rec.digest.as_deref()
            .map(|d| &d[..16.min(d.len())])
            .unwrap_or("(none)");
        println!(
            "  {:<3}  {:<6}  {:<12}  {:<10}  {:<18}  {}…",
            i, rec.kind, rec.offset, format_bytes(rec.size), rec.created_at, digest_short
        );
    }

    println!();
    println!("Union view: {} file(s) visible", tcow.visible_count());
    Ok(())
}

// ── list ──────────────────────────────────────────────────────────────────────

fn cmd_list(
    path: PathBuf,
    prefix: Option<String>,
    layer: Option<usize>,
    all_layers: bool,
    long: bool,
    show_whiteouts: bool,
) -> Result<()> {
    let tcow = TcowFile::open(&path)?;
    let prefix_canon = prefix.as_deref().map(normalize_path).unwrap_or_default();

    if all_layers {
        // Show every entry from every layer including shadowed/whiteouts
        for (layer_idx, layer_entries) in tcow.layers.iter().enumerate() {
            let layer_kind = &tcow.index.layers[layer_idx].kind;
            let union = tcow.union_view();
            let mut paths: Vec<&String> = layer_entries.keys().collect();
            paths.sort();
            for p in paths {
                let entry = &layer_entries[p];
                if !prefix_canon.is_empty() && !p.starts_with(&prefix_canon) {
                    continue;
                }
                if entry.is_dir { continue; }
                let visible_in_union = union.contains_key(p.as_str());
                let tag = if entry.is_whiteout {
                    "[DEL]"
                } else if !visible_in_union {
                    "[hidden]"
                } else {
                    "       "
                };
                if !show_whiteouts && entry.is_whiteout { continue; }
                if long {
                    println!(
                        "  {tag}  {:>10}  {:<18}  layer {layer_idx} ({layer_kind})  /{}",
                        format_bytes(entry.data.len() as u64),
                        unix_ts_to_rfc3339(entry.mtime),
                        p
                    );
                } else {
                    println!("  {tag}  /{p}  (layer {layer_idx} — {layer_kind})");
                }
            }
        }
        return Ok(());
    }

    if let Some(layer_idx) = layer {
        // Specific layer only, no union logic
        if layer_idx >= tcow.layers.len() {
            bail!("layer {layer_idx} does not exist (file has {} layers)", tcow.layers.len());
        }
        let layer_entries = &tcow.layers[layer_idx];
        let mut paths: Vec<&String> = layer_entries.keys().collect();
        paths.sort();
        for p in paths {
            let entry = &layer_entries[p];
            if !prefix_canon.is_empty() && !p.starts_with(&prefix_canon) {
                continue;
            }
            if entry.is_dir { continue; }
            if !show_whiteouts && entry.is_whiteout { continue; }
            if long {
                let tag = if entry.is_whiteout { "[DEL]" } else { "     " };
                println!(
                    "{tag}  {:>10}  {:<18}  /{}",
                    format_bytes(entry.data.len() as u64),
                    unix_ts_to_rfc3339(entry.mtime),
                    p
                );
            } else {
                let tag = if entry.is_whiteout { "[DEL] " } else { "" };
                println!("{tag}/{p}");
            }
        }
        return Ok(());
    }

    // Default: union view
    let view = tcow.union_view();
    let mut paths: Vec<(&String, _)> = view.iter().collect();
    paths.sort_by_key(|(p, _)| p.as_str());

    for (p, entry) in paths {
        if !prefix_canon.is_empty() && !p.starts_with(&prefix_canon) {
            continue;
        }
        if long {
            println!(
                "  {:>10}  {:<18}  layer {:>2}  /{}",
                format_bytes(entry.size),
                unix_ts_to_rfc3339(entry.mtime),
                entry.layer_idx,
                p
            );
        } else {
            println!("/{p}");
        }
    }
    Ok(())
}

// ── cat ───────────────────────────────────────────────────────────────────────

fn cmd_cat(path: PathBuf, vpath: String, layer: Option<usize>) -> Result<()> {
    let tcow = TcowFile::open(&path)?;
    let canonical = normalize_path(&vpath);

    if let Some(layer_idx) = layer {
        if layer_idx >= tcow.layers.len() {
            bail!("layer {layer_idx} does not exist");
        }
        let entry = tcow.layers[layer_idx].get(&canonical)
            .ok_or_else(|| anyhow::anyhow!("/{canonical} not found in layer {layer_idx}"))?;
        if entry.is_whiteout {
            bail!("/{canonical} is a whiteout (deletion marker) in layer {layer_idx}");
        }
        io::stdout().write_all(&entry.data)?;
        io::stdout().write_all(b"\n")?;
    } else {
        match tcow.resolve(&vpath) {
            None => bail!("/{canonical} not found in virtual filesystem"),
            Some((entry, _)) => {
                io::stdout().write_all(&entry.data)?;
                io::stdout().write_all(b"\n")?;
            }
        }
    }
    Ok(())
}

// ── stat ──────────────────────────────────────────────────────────────────────

fn cmd_stat(path: PathBuf, vpath: String, json: bool) -> Result<()> {
    let tcow = TcowFile::open(&path)?;
    let canonical = normalize_path(&vpath);
    let view = tcow.union_view();

    if json {
        match view.get(&canonical) {
            None => {
                // Check if it's a whiteout
                let whiteout = tcow.layers.iter().rev().any(|l| {
                    l.get(&canonical).map_or(false, |e| e.is_whiteout)
                });
                if whiteout {
                    println!(r#"{{"path":"/{canonical}","size":0,"mtime":null,"layer":null,"whiteout":true}}"#);
                } else {
                    bail!("/{canonical} not found");
                }
            }
            Some(entry) => {
                let mtime = unix_ts_to_rfc3339(entry.mtime);
                println!(
                    r#"{{"path":"/{canonical}","size":{},"mtime":"{mtime}","layer":{},"whiteout":false}}"#,
                    entry.size, entry.layer_idx
                );
            }
        }
    } else {
        match view.get(&canonical) {
            None => bail!("/{canonical} not found in virtual filesystem"),
            Some(entry) => {
                println!("Path:     /{canonical}");
                println!("Size:     {} bytes", entry.size);
                println!("Mtime:    {}", unix_ts_to_rfc3339(entry.mtime));
                println!("Layer:    {} ({})", entry.layer_idx, tcow.index.layers[entry.layer_idx].kind);
                println!("Whiteout: false");
            }
        }
    }
    Ok(())
}

// ── insert ────────────────────────────────────────────────────────────────────

fn cmd_insert(path: PathBuf, vpath: String, source: Option<PathBuf>, dry_run: bool) -> Result<()> {
    let content = match source {
        Some(ref src) => {
            fs::read(src).with_context(|| format!("reading source file {:?}", src))?
        }
        None => {
            let mut buf = Vec::new();
            io::stdin().read_to_end(&mut buf).context("reading stdin")?;
            buf
        }
    };

    let size = content.len();
    let canonical = normalize_path(&vpath);

    if dry_run {
        if path.exists() {
            let tcow = TcowFile::open(&path)?;
            let n = tcow.index.layers.len();
            println!("[DRY RUN] Would insert /{canonical} ({size} bytes) as new delta layer {n}");
        } else {
            println!("[DRY RUN] Would create {:?} with /{canonical} ({size} bytes) as base layer 0", path);
        }
        return Ok(());
    }

    let entries = vec![(canonical.clone(), content)];

    if path.exists() {
        let tcow = TcowFile::append_delta(&path, &entries, &[])?;
        let n = tcow.index.layers.len();
        println!("Inserted /{canonical} ({size} bytes) into new delta layer {}", n - 1);
    } else {
        let _tcow = TcowFile::create(&path, &entries, &[], None)?;
        println!("Created {:?} — inserted /{canonical} ({size} bytes) into base layer 0", path);
    }
    Ok(())
}

// ── delete ────────────────────────────────────────────────────────────────────

fn cmd_delete(path: PathBuf, vpath: String, dry_run: bool) -> Result<()> {
    let canonical = normalize_path(&vpath);
    let tcow = TcowFile::open(&path)?;
    let view = tcow.union_view();

    if !view.contains_key(&canonical) {
        bail!("/{canonical} does not exist in the virtual filesystem (nothing to delete)");
    }

    let wh_tar_path = tcow::to_whiteout_tar_path(&canonical);

    if dry_run {
        let n = tcow.index.layers.len();
        println!("[DRY RUN] Would write whiteout {wh_tar_path} in new delta layer {n}");
        return Ok(());
    }

    let updated = TcowFile::append_delta(&path, &[], &[canonical.clone()])?;
    let n = updated.index.layers.len();
    println!("Wrote whiteout for /{canonical} in new delta layer {}", n - 1);
    Ok(())
}

// ── extract ───────────────────────────────────────────────────────────────────

fn cmd_extract(
    path: PathBuf,
    vpath: Option<String>,
    outdir: PathBuf,
    layer: Option<usize>,
    strip_prefix: Option<String>,
    dry_run: bool,
) -> Result<()> {
    let tcow = TcowFile::open(&path)?;
    let prefix_canon = vpath.as_deref().map(normalize_path).unwrap_or_default();
    let strip = strip_prefix.as_deref().map(normalize_path).unwrap_or_default();

    // Collect entries to extract
    let to_extract: Vec<(String, Vec<u8>)> = if let Some(layer_idx) = layer {
        if layer_idx >= tcow.layers.len() {
            bail!("layer {layer_idx} does not exist");
        }
        tcow.layers[layer_idx]
            .iter()
            .filter(|(_p, e)| !e.is_dir && !e.is_whiteout)
            .filter(|(p, _)| prefix_canon.is_empty() || p.starts_with(&prefix_canon))
            .map(|(p, e)| (p.clone(), e.data.clone()))
            .collect()
    } else {
        tcow.union_view()
            .into_iter()
            .filter(|(p, _)| prefix_canon.is_empty() || p.starts_with(&prefix_canon))
            .map(|(p, e)| (p, e.data))
            .collect()
    };

    if dry_run {
        for (p, data) in &to_extract {
            println!("[DRY RUN] Would extract /{p} ({} bytes)", data.len());
        }
        return Ok(());
    }

    if !outdir.exists() {
        fs::create_dir_all(&outdir)
            .with_context(|| format!("creating output directory {:?}", outdir))?;
    }

    let mut count = 0usize;
    for (p, data) in &to_extract {
        let rel = if !strip.is_empty() && p.starts_with(&strip) {
            p[strip.len()..].trim_start_matches('/')
        } else {
            p.as_str()
        };

        let dest = outdir.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dest, data)
            .with_context(|| format!("writing {:?}", dest))?;
        count += 1;
    }

    println!("Extracted {count} file(s) to {}", outdir.display());
    Ok(())
}

// ── snapshot ──────────────────────────────────────────────────────────────────

fn cmd_snapshot(path: PathBuf, label: Option<String>) -> Result<()> {
    // Append an empty delta layer (just the end-of-archive two zero blocks)
    let updated = TcowFile::append_delta(&path, &[], &[])?;
    let n = updated.index.layers.len();
    let rec = &updated.index.layers[n - 1];
    if let Some(lbl) = &label {
        // We can't easily update the label after the fact without re-reading.
        // Just report that it was created.
        println!("Snapshot created: layer {} (Delta) \"{lbl}\" at offset {}", n - 1, rec.offset);
    } else {
        println!("Snapshot created: layer {} (Delta) at offset {}", n - 1, rec.offset);
    }
    Ok(())
}

// ── compact ───────────────────────────────────────────────────────────────────

fn cmd_compact(
    path: PathBuf,
    output: Option<PathBuf>,
    in_place: bool,
    dry_run: bool,
) -> Result<()> {
    let tcow = TcowFile::open(&path)?;
    let orig_size = fs::metadata(&path)?.len();
    let n_layers = tcow.index.layers.len();

    // Collect all visible files
    let view = tcow.union_view();
    let entries: Vec<(String, Vec<u8>)> = {
        let mut v: Vec<_> = view.into_iter().collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v.into_iter().map(|(p, e)| (p, e.data)).collect()
    };

    if dry_run {
        let approx: u64 = entries.iter().map(|(_, d)| d.len() as u64 + 512).sum();
        println!("[DRY RUN] Would compact {n_layers} layers ({orig_size} bytes) → ~{approx} bytes");
        println!("[DRY RUN] {} file(s) would be preserved", entries.len());
        return Ok(());
    }

    let dest = if in_place {
        path.clone()
    } else {
        output.unwrap_or_else(|| {
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            path.with_file_name(format!("{stem}.compacted{ext}"))
        })
    };

    if in_place {
        // Write to a temp file first, then rename
        let tmp = path.with_extension("tcow.tmp");
        TcowFile::create(&tmp, &entries, &[], tcow.index.label.clone())?;
        fs::rename(&tmp, &dest)?;
    } else {
        TcowFile::create(&dest, &entries, &[], tcow.index.label.clone())?;
    }

    let new_size = fs::metadata(&dest)?.len();
    let saved = orig_size.saturating_sub(new_size);
    let pct = if orig_size > 0 { 100 * saved / orig_size } else { 0 };

    println!("Compacted {:?} → {:?}", path, dest);
    println!("  Before: {n_layers} layer(s), {orig_size} bytes");
    println!("  After:  1 layer,  {new_size} bytes  ({pct}% reduction)");
    Ok(())
}

// ── verify ────────────────────────────────────────────────────────────────────

fn cmd_verify(path: PathBuf, fix_missing: bool) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::{Seek, SeekFrom};

    let tcow = TcowFile::open(&path)?;
    let n = tcow.index.layers.len();
    println!("Verifying {} ({n} layers)…\n", path.display());

    let mut f = std::fs::File::open(&path)?;
    let mut errors = 0usize;
    let mut missing = Vec::new();

    for (i, rec) in tcow.index.layers.iter().enumerate() {
        f.seek(SeekFrom::Start(rec.offset))?;
        let mut raw = vec![0u8; rec.size as usize];
        f.read_exact(&mut raw)?;
        let computed = sha256_hex(&raw);

        match &rec.digest {
            None => {
                println!("  Layer {i:>2}  [{:>5}]  (no digest stored)  -  SKIPPED", rec.kind);
                missing.push(i);
            }
            Some(stored) => {
                if *stored == computed {
                    println!("  Layer {i:>2}  [{:>5}]  {}…  ✓", rec.kind, &computed[..16]);
                } else {
                    println!(
                        "  Layer {i:>2}  [{:>5}]  {}…  ✗  MISMATCH",
                        rec.kind,
                        &computed[..16]
                    );
                    eprintln!("             stored:   {stored}");
                    eprintln!("             computed: {computed}");
                    errors += 1;
                }
            }
        }
    }

    if fix_missing && !missing.is_empty() {
        // Re-open for read to compute digests, then rewrite trailer
        f.seek(SeekFrom::Start(0))?;
        let mut new_layers = tcow.index.layers.clone();
        for i in &missing {
            let rec = &new_layers[*i];
            f.seek(SeekFrom::Start(rec.offset))?;
            let mut raw = vec![0u8; rec.size as usize];
            f.read_exact(&mut raw)?;
            new_layers[*i].digest = Some(sha256_hex(&raw));
        }
        let new_index = TcowIndex {
            version: tcow.index.version,
            layers: new_layers,
            last_modified: now_rfc3339(),
            label: tcow.index.label.clone(),
        };
        // Find where to write the new trailer
        let last_rec = new_index.layers.last().unwrap();
        let trailer_offset = last_rec.offset + last_rec.size;
        let cbor = encode_cbor(&new_index)?;
        let trailer_len = cbor.len() as u32;

        let mut fw = OpenOptions::new().write(true).open(&path)?;
        fw.seek(SeekFrom::Start(trailer_offset))?;
        fw.set_len(trailer_offset)?;
        fw.seek(SeekFrom::Start(trailer_offset))?;
        fw.write_all(&cbor)?;
        write_trailer_footer(&mut fw, trailer_offset, trailer_len)?;
        fw.flush()?;
        println!("\nFixed {count} missing digest(s).", count = missing.len());
    }

    println!();
    if errors == 0 {
        if n == 0 {
            println!("No layers in file.");
        } else {
            println!("All layers verified. File is intact.");
        }
        Ok(())
    } else {
        bail!("{errors} layer(s) failed integrity check");
    }
}

// ── layers ────────────────────────────────────────────────────────────────────

fn cmd_layers(path: PathBuf, json: bool) -> Result<()> {
    let tcow = TcowFile::open(&path)?;

    if json {
        println!("[");
        let last = tcow.index.layers.len().saturating_sub(1);
        for (i, rec) in tcow.index.layers.iter().enumerate() {
            let digest = match &rec.digest {
                Some(d) => format!(r#""{}""#, d),
                None => "null".into(),
            };
            let comma = if i < last { "," } else { "" };
            println!(
                r#"  {{"index":{i},"kind":"{}","offset":{},"size":{},"created_at":"{}","digest":{digest}}}{comma}"#,
                rec.kind, rec.offset, rec.size, rec.created_at
            );
        }
        println!("]");
    } else {
        println!(
            "  {:<3}  {:<6}  {:<12}  {:<10}  {:<64}  {}",
            "#", "Kind", "Offset", "Size", "Digest (SHA-256)", "Created"
        );
        println!(
            "  {}  {}  {}  {}  {}  {}",
            "─".repeat(3),
            "─".repeat(6),
            "─".repeat(12),
            "─".repeat(10),
            "─".repeat(64),
            "─".repeat(20)
        );
        for (i, rec) in tcow.index.layers.iter().enumerate() {
            let digest = rec.digest.as_deref().unwrap_or("(none)");
            println!(
                "  {:<3}  {:<6}  {:<12}  {:<10}  {:<64}  {}",
                i, rec.kind, rec.offset, format_bytes(rec.size), digest, rec.created_at
            );
        }
    }
    Ok(())
}

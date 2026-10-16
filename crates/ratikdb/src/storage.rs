//! Durability: a write-ahead log of committed transactions plus periodic
//! snapshots, all in plain files inside one data directory.
//!
//! ```text
//! <data>/
//!   LOCK          exclusive lock so two processes never share a directory
//!   snapshot.rtk  compacted state at generation G
//!   wal.rtk       transactions committed after the snapshot (generation G+1)
//! ```
//!
//! Both files are a header followed by length-prefixed, CRC-checked records.
//! A torn record at the end of the log (power loss mid-write) is detected by
//! its checksum and truncated away on the next open.

use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::graph::{Graph, Op};

const WAL_MAGIC: &[u8; 8] = b"RATIKWAL";
const SNAP_MAGIC: &[u8; 8] = b"RATIKSNP";
const FORMAT_VERSION: u32 = 1;
const HEADER_LEN: u64 = 8 + 4 + 8;
const MAX_RECORD: u32 = 1 << 30;

/// When the log is flushed to stable storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncMode {
    /// `fsync` after every committed transaction (safest; default).
    #[default]
    Always,
    /// Leave flushing to the operating system (fastest; a power loss may lose
    /// the most recent transactions, but never corrupts the database).
    Never,
}

#[derive(Debug)]
pub(crate) struct Wal {
    file: File,
    dir: PathBuf,
    generation: u64,
    size: u64,
    sync: SyncMode,
    _lock: File,
}

fn write_header(w: &mut impl Write, magic: &[u8; 8], generation: u64) -> Result<()> {
    w.write_all(magic)?;
    w.write_all(&FORMAT_VERSION.to_le_bytes())?;
    w.write_all(&generation.to_le_bytes())?;
    Ok(())
}

fn read_header(r: &mut impl Read, magic: &[u8; 8], what: &str) -> Result<u64> {
    let mut buf = [0u8; HEADER_LEN as usize];
    r.read_exact(&mut buf).map_err(|_| Error::Corrupt(format!("{what}: truncated header")))?;
    if &buf[..8] != magic {
        return Err(Error::Corrupt(format!("{what}: not a RatikDB file")));
    }
    let version = u32::from_le_bytes(buf[8..8 + 4].try_into().expect("4-byte slice"));
    if version != FORMAT_VERSION {
        return Err(Error::Corrupt(format!("{what}: unsupported format version {version}")));
    }
    Ok(u64::from_le_bytes(buf[8 + 4..8 + 4 + 8].try_into().expect("8-byte slice")))
}

fn write_record(w: &mut impl Write, payload: &[u8]) -> Result<()> {
    let len = u32::try_from(payload.len()).map_err(|_| Error::query("transaction too large (over 4 GiB)"))?;
    let mut buf = Vec::with_capacity(8 + payload.len());
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf)?;
    Ok(())
}

/// Reads the next record. `Ok(None)` at a clean end or a torn tail.
fn read_record(r: &mut impl Read) -> Result<Option<Vec<u8>>> {
    let mut head = [0u8; 8];
    match r.read_exact(&mut head) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(head[..4].try_into().expect("4-byte slice"));
    let crc = u32::from_le_bytes(head[4..4 + 4].try_into().expect("4-byte slice"));
    if len > MAX_RECORD {
        return Ok(None);
    }
    let mut payload = vec![0u8; len as usize];
    match r.read_exact(&mut payload) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    Ok((crc32fast::hash(&payload) == crc).then_some(payload))
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8], what: &str) -> Result<T> {
    postcard::from_bytes(bytes).map_err(|e| Error::Corrupt(format!("{what}: {e}")))
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    postcard::to_stdvec(value).map_err(|e| Error::Corrupt(format!("encode: {e}")))
}

fn sync_dir(dir: &Path) {
    // Directory fsync makes renames durable on Unix; not supported everywhere.
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

impl Wal {
    /// Opens (or creates) a data directory and rebuilds the graph from it.
    pub(crate) fn open(dir: &Path, sync: SyncMode) -> Result<(Graph, Self)> {
        fs::create_dir_all(dir)?;
        let lock = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("LOCK"))?;
        lock.try_lock().map_err(|_| Error::Locked(dir.display().to_string()))?;

        let mut graph = Graph::default();
        let snap_path = dir.join("snapshot.rtk");
        let mut snap_gen = 0;
        if snap_path.exists() {
            let mut r = BufReader::new(File::open(&snap_path)?);
            snap_gen = read_header(&mut r, SNAP_MAGIC, "snapshot")?;
            while let Some(rec) = read_record(&mut r)? {
                let op: Op = decode(&rec, "snapshot")?;
                graph.apply(&op)?;
            }
        }

        let wal_path = dir.join("wal.rtk");
        let (file, generation, size) = if wal_path.exists() {
            let mut file = OpenOptions::new().read(true).write(true).open(&wal_path)?;
            let mut r = BufReader::new(&mut file);
            let generation = read_header(&mut r, WAL_MAGIC, "wal")?;
            let mut good = HEADER_LEN;
            while let Some(rec) = read_record(&mut r)? {
                if generation > snap_gen {
                    let ops: Vec<Op> = decode(&rec, "wal")?;
                    for op in &ops {
                        graph.apply(op)?;
                    }
                }
                good += 8 + rec.len() as u64;
            }
            drop(r);
            // Drop a torn tail so new records append after the last good one.
            file.set_len(good)?;
            file.seek(SeekFrom::End(0))?;
            if generation <= snap_gen {
                // Crash between writing a snapshot and rotating the log:
                // everything in this log is already in the snapshot.
                let (file, size) = Self::fresh_log(dir, snap_gen + 1)?;
                (file, snap_gen + 1, size)
            } else {
                (file, generation, good)
            }
        } else {
            let (file, size) = Self::fresh_log(dir, snap_gen + 1)?;
            (file, snap_gen + 1, size)
        };

        Ok((graph, Self { file, dir: dir.to_path_buf(), generation, size, sync, _lock: lock }))
    }

    /// Atomically replaces `wal.rtk` with an empty log of `generation`.
    fn fresh_log(dir: &Path, generation: u64) -> Result<(File, u64)> {
        let tmp = dir.join("wal.rtk.tmp");
        {
            let mut f = File::create(&tmp)?;
            write_header(&mut f, WAL_MAGIC, generation)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, dir.join("wal.rtk"))?;
        sync_dir(dir);
        let mut file = OpenOptions::new().read(true).write(true).open(dir.join("wal.rtk"))?;
        file.seek(SeekFrom::End(0))?;
        Ok((file, HEADER_LEN))
    }

    /// Durably appends one committed transaction.
    pub(crate) fn append(&mut self, ops: &[Op]) -> Result<()> {
        let payload = encode(&ops)?;
        write_record(&mut self.file, &payload)?;
        if self.sync == SyncMode::Always {
            self.file.sync_data()?;
        }
        self.size += 8 + payload.len() as u64;
        Ok(())
    }

    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    /// Writes a snapshot of `graph` and starts a new, empty log.
    pub(crate) fn snapshot(&mut self, graph: &Graph) -> Result<()> {
        let tmp = self.dir.join("snapshot.rtk.tmp");
        {
            let mut w = BufWriter::new(File::create(&tmp)?);
            write_header(&mut w, SNAP_MAGIC, self.generation)?;
            for op in graph.snapshot_ops() {
                write_record(&mut w, &encode(&op)?)?;
            }
            let f = w.into_inner().map_err(|e| Error::Io(e.into_error()))?;
            f.sync_all()?;
        }
        fs::rename(&tmp, self.dir.join("snapshot.rtk"))?;
        sync_dir(&self.dir);
        let (file, size) = Self::fresh_log(&self.dir, self.generation + 1)?;
        self.file = file;
        self.size = size;
        self.generation += 1;
        Ok(())
    }
}

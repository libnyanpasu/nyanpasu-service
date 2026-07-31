//! Durable JSONL archive of the manager's core-log stream.
//!
//! One rolling file per writer run in `{runtime_dir}/logs/`, rotated by size and
//! retained by count. The writer is an ordinary [`broadcast`] subscriber, so it
//! never touches the supervision loop that produces the frames
//! (`instance.rs`'s `publish_log_frame`, which runs inline on that loop): a slow
//! disk costs this task its own backlog and nothing else.
//!
//! The on-disk contract, in full:
//!
//! - append-only, line-delimited JSON, one object per line, never rewritten and
//!   never renamed;
//! - every record carries `t` (`"log"` or `"gap"`) and `at`, the writer-observed
//!   unix-millisecond instant. `at` is the only sortable clock: a frame whose
//!   header did not parse has no timestamp at all, and clash premium's and
//!   clash-rs's are inferred;
//! - a crash can only truncate the **last** line, because each batch is one
//!   `write_all`. A reader splits on `\n`, parses each line, and ignores a
//!   trailing line that does not parse;
//! - there is no `fsync`. This is diagnostic data, deliberately unlike
//!   `RuntimeConfigStore`, which pays the full stage/fsync/replace price because
//!   it holds authoritative configuration.

use camino::{Utf8Path, Utf8PathBuf};
use clash_api::LogField;
use serde::Serialize;
use tokio::{
    io::AsyncWriteExt,
    sync::broadcast::{
        Receiver,
        error::{RecvError, TryRecvError},
    },
};
use tokio_util::sync::CancellationToken;

use crate::{
    error::Error,
    kind::CoreKind,
    log::{LogFrame, LogLevel, LogStream, LogTimestamp},
    // `crate::runtime_store`, not `crate::config::runtime_store`: `lib.rs:19`
    // re-exports it and `manager/mod.rs:26` already spells it this way.
    runtime_store::validate_directory_metadata,
    state::now_ms,
};

/// Subdirectory of the manager's runtime directory. Deliberately not a sibling
/// of the epoch artifacts: `cleanup_epoch` and `artifact_epochs` scan the
/// runtime directory by filename prefix, and a log file's useful life starts
/// exactly where an epoch artifact's ends.
const LOG_DIR_NAME: &str = "logs";

const FILE_PREFIX: &str = "core-";
const FILE_SUFFIX: &str = ".jsonl";

/// Per-record cap on the two free-text fields. The parser's 16 KiB budget bounds
/// only continuation growth, so a single enormous line still arrives whole; this
/// is what actually makes "a rotated file overshoots by at most one record" a
/// statement with a number behind it.
const MAX_TEXT_BYTES: usize = 16 * 1024;

/// Rotation knobs, mirrored from `ManagerOptions` so the writer does not carry
/// the whole option bag.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SinkOptions {
    pub max_bytes: u64,
    pub max_files: usize,
}

/// A `"log"` line. The fields are borrowed from the frame so the two capped
/// texts can be sliced instead of cloned; every key is always present, including
/// the nulls, so a reader never has to branch on the schema.
#[derive(Serialize)]
struct LogRecord<'a> {
    t: &'static str,
    at: i64,
    epoch: u64,
    kind: CoreKind,
    stream: LogStream,
    level: LogLevel,
    timestamp: Option<&'a LogTimestamp>,
    target: Option<&'a str>,
    message: &'a str,
    fields: &'a [LogField],
    raw: &'a str,
    /// The frame's own flag, OR-ed with whichever cap this record hit.
    truncated: bool,
}

/// A `"gap"` line: the writer fell behind and the broadcast dropped `dropped`
/// frames on its behalf. Writing it turns a silent hole into a visible one.
#[derive(Serialize)]
struct GapRecord {
    t: &'static str,
    at: i64,
    dropped: u64,
}

impl<'a> LogRecord<'a> {
    fn new(frame: &'a LogFrame, at: i64) -> Self {
        let (message, message_cut) = clamp(&frame.message);
        let (raw, raw_cut) = clamp(&frame.raw);
        Self {
            t: "log",
            at,
            epoch: frame.epoch,
            kind: frame.kind,
            stream: frame.stream,
            level: frame.level,
            timestamp: frame.timestamp.as_ref(),
            target: frame.target.as_deref(),
            message,
            fields: &frame.fields,
            raw,
            truncated: frame.truncated || message_cut || raw_cut,
        }
    }
}

impl GapRecord {
    fn new(dropped: u64, at: i64) -> Self {
        Self {
            t: "gap",
            at,
            dropped,
        }
    }
}

/// Cuts at the last char boundary at or below the limit, so the result is still
/// a `str` and still serializes as valid JSON.
fn clamp(text: &str) -> (&str, bool) {
    if text.len() <= MAX_TEXT_BYTES {
        return (text, false);
    }
    let mut end = MAX_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// Creates and hardens `{parent}/logs`, returning its path.
///
/// The DACL is applied here rather than inherited: the parent grants
/// `OICI` inheritance, but an inherited descriptor does not carry
/// `SE_DACL_PROTECTED`, which is exactly what `verify_windows_directory_acl`
/// requires. A directory this fails on is a construction failure, the same class
/// as a runtime directory that cannot be hardened.
pub(crate) async fn prepare_dir(parent: &Utf8Path) -> Result<Utf8PathBuf, Error> {
    let dir = parent.join(LOG_DIR_NAME);
    match tokio::fs::symlink_metadata(&dir).await {
        Ok(metadata) => validate_directory_metadata(&dir, &metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir_all(&dir).await?;
        }
        Err(error) => return Err(error.into()),
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).await?;
    }
    #[cfg(windows)]
    {
        nyanpasu_utils::io::atomic_fs::harden_windows_directory_acl(&dir)?;
        nyanpasu_utils::io::atomic_fs::verify_windows_directory_acl(&dir)?;
    }

    Ok(dir)
}

/// Opens the first file and hands the writer to a background task.
///
/// Opening before spawning is deliberate: a directory that cannot be written is
/// a `CoreManager::new` failure, not a task that dies quietly three seconds
/// later.
pub(crate) async fn spawn(
    dir: Utf8PathBuf,
    options: SinkOptions,
    logs: Receiver<LogFrame>,
    cancel: CancellationToken,
) -> Result<(), Error> {
    let writer = Writer::open(dir, options).await?;
    tokio::spawn(run(writer, logs, cancel));
    Ok(())
}

/// Drains the broadcast in batches until the token is cancelled or the last
/// sender is gone. One `recv().await` for the first frame, then `try_recv()`
/// until empty: a start-up burst costs one write instead of one per line.
async fn run(mut writer: Writer, mut logs: Receiver<LogFrame>, cancel: CancellationToken) {
    let mut batch = Vec::new();
    loop {
        // Cleared before the select, never after: the cancellation arm writes
        // this same buffer, and a stale batch there would be written twice.
        batch.clear();
        let first = tokio::select! {
            _ = cancel.cancelled() => {
                drain(&mut logs, &mut batch);
                writer.write(&batch).await;
                break;
            }
            received = logs.recv() => received,
        };
        let closed = match first {
            Ok(frame) => {
                batch.push(Entry::Log(frame));
                false
            }
            Err(RecvError::Lagged(dropped)) => {
                batch.push(Entry::Gap(dropped));
                false
            }
            Err(RecvError::Closed) => true,
        };
        // Short-circuits deliberately: `Closed` from `recv` already means the
        // ring is empty, so there is nothing left for `drain` to find.
        let closed = closed || drain(&mut logs, &mut batch);
        writer.write(&batch).await;
        if closed {
            break;
        }
    }
}

enum Entry {
    Log(LogFrame),
    Gap(u64),
}

/// Appends everything already buffered. `true` means the channel is closed and
/// the caller must stop after writing what it has.
fn drain(logs: &mut Receiver<LogFrame>, batch: &mut Vec<Entry>) -> bool {
    loop {
        match logs.try_recv() {
            Ok(frame) => batch.push(Entry::Log(frame)),
            Err(TryRecvError::Lagged(dropped)) => batch.push(Entry::Gap(dropped)),
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Closed) => return true,
        }
    }
}

struct Writer {
    dir: Utf8PathBuf,
    options: SinkOptions,
    file: tokio::fs::File,
    written: u64,
    seq: u64,
}

impl Writer {
    async fn open(dir: Utf8PathBuf, options: SinkOptions) -> Result<Self, Error> {
        // A new file per run, never an append: a crash can leave the previous
        // file's last line half-written, and appending would glue it to a fresh
        // record. One file also means one run, which is a useful boundary when
        // somebody hands you a log directory.
        let seq = next_seq(&dir).await?;
        let file = create(&dir, seq).await?;
        let writer = Self {
            dir,
            options,
            file,
            written: 0,
            seq,
        };
        // A previous run may have died holding more files than the retention
        // limit allows.
        writer.prune().await;
        Ok(writer)
    }

    /// Serializes and appends one batch, rotating whenever the active file has
    /// already crossed the limit. The check runs per record, not per batch, so
    /// a burst that arrives as one batch still cannot produce a file more than
    /// one record past `max_bytes` — a record being bounded by MAX_TEXT_BYTES
    /// on each of its two free-text fields. Errors are logged and swallowed:
    /// losing diagnostic output must never take down a manager that is
    /// otherwise healthy.
    async fn write(&mut self, batch: &[Entry]) {
        if batch.is_empty() {
            return;
        }
        let at = now_ms();
        let mut buffer = Vec::new();
        for entry in batch {
            // Rotate before the record that would land past the limit,
            // counting both what is on disk and what is still buffered for the
            // active file.
            if self.written + buffer.len() as u64 >= self.options.max_bytes {
                self.flush_pending(&mut buffer).await;
                self.rotate().await;
            }
            let mark = buffer.len();
            let result = match entry {
                Entry::Log(frame) => serde_json::to_writer(&mut buffer, &LogRecord::new(frame, at)),
                Entry::Gap(dropped) => {
                    serde_json::to_writer(&mut buffer, &GapRecord::new(*dropped, at))
                }
            };
            if let Err(error) = result {
                buffer.truncate(mark);
                tracing::error!("failed to serialize a core log record: {error}");
                continue;
            }
            buffer.push(b'\n');
        }
        self.flush_pending(&mut buffer).await;
    }

    /// Appends whatever is buffered to the active file and clears the buffer.
    /// On failure the pending records are dropped (logged), matching the
    /// policy that disk trouble never backs up into the manager.
    async fn flush_pending(&mut self, buffer: &mut Vec<u8>) {
        if buffer.is_empty() {
            return;
        }
        if let Err(error) = self.file.write_all(buffer).await {
            tracing::error!("failed to write core log records: {error}");
            buffer.clear();
            return;
        }
        // tokio's File buffers, so without this the bytes are not on their way
        // to the OS and the file is not tailable.
        if let Err(error) = self.file.flush().await {
            tracing::error!("failed to flush core log records: {error}");
        }
        self.written += buffer.len() as u64;
        buffer.clear();
    }

    async fn rotate(&mut self) {
        let seq = self.seq + 1;
        match create(&self.dir, seq).await {
            Ok(file) => {
                self.file = file;
                self.seq = seq;
                self.written = 0;
                self.prune().await;
            }
            // Keep writing into the oversized file rather than losing the
            // stream; the next record's pre-check retries.
            Err(error) => {
                tracing::error!("failed to roll over the core log file: {error}");
            }
        }
    }

    /// Deletes everything past the newest `max_files`, the active file included
    /// in the count. A delete that fails — a file held open by an editor, which
    /// Windows refuses to unlink — is logged and retried on the next rollover.
    async fn prune(&self) {
        let existing = match read_seqs(&self.dir).await {
            Ok(seqs) => seqs,
            Err(error) => {
                tracing::warn!("failed to list the core log directory: {error}");
                return;
            }
        };
        for seq in prune_targets(existing, self.options.max_files) {
            let path = self.dir.join(file_name(seq));
            if let Err(error) = tokio::fs::remove_file(&path).await {
                tracing::warn!("failed to prune the rotated core log file {path}: {error}");
            }
        }
    }
}

async fn create(dir: &Utf8Path, seq: u64) -> Result<tokio::fs::File, Error> {
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(dir.join(file_name(seq))).await?)
}

fn file_name(seq: u64) -> String {
    format!("{FILE_PREFIX}{seq:06}{FILE_SUFFIX}")
}

/// `None` for anything this writer did not create, so an unrelated file in the
/// directory is never a rotation candidate.
fn file_seq(name: &str) -> Option<u64> {
    name.strip_prefix(FILE_PREFIX)
        .and_then(|rest| rest.strip_suffix(FILE_SUFFIX))
        .filter(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|digits| digits.parse().ok())
}

async fn read_seqs(dir: &Utf8Path) -> Result<Vec<u64>, std::io::Error> {
    let mut seqs = Vec::new();
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        // Bound before borrowing, the same shape `cleanup_epoch` uses: the
        // `OsString` has to outlive the `&str` taken out of it.
        let name = entry.file_name();
        if let Some(seq) = name.to_str().and_then(file_seq) {
            seqs.push(seq);
        }
    }
    Ok(seqs)
}

async fn next_seq(dir: &Utf8Path) -> Result<u64, Error> {
    let seqs = read_seqs(dir).await?;
    Ok(seqs.into_iter().max().unwrap_or(0) + 1)
}

/// Newest `max_files` survive; the rest are returned oldest-last. Ordering is
/// numeric, not lexicographic: the zero padding is six wide for readability and
/// stops agreeing with string order at a million files.
fn prune_targets(mut seqs: Vec<u64>, max_files: usize) -> Vec<u64> {
    seqs.sort_unstable_by_key(|seq| std::cmp::Reverse(*seq));
    seqs.into_iter().skip(max_files).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::kind::CoreKind;

    fn temp_dir() -> (tempfile::TempDir, Utf8PathBuf) {
        let guard = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(guard.path().to_path_buf()).unwrap();
        (guard, path)
    }

    fn options(max_bytes: u64, max_files: usize) -> SinkOptions {
        SinkOptions {
            max_bytes,
            max_files,
        }
    }

    fn frame(message: &str) -> LogFrame {
        LogFrame {
            epoch: 7,
            kind: CoreKind::Mihomo,
            stream: LogStream::Stdout,
            timestamp: Some(LogTimestamp {
                raw: "2026-07-29T00:16:22.646059400+08:00".to_owned(),
                unix_ms: Some(1_753_719_382_646),
                inferred: false,
            }),
            level: LogLevel::Info,
            target: None,
            message: message.to_owned(),
            fields: vec![LogField {
                key: "request".to_owned(),
                value: "7".to_owned(),
            }],
            raw: format!("time=\"...\" level=info msg=\"{message}\""),
            truncated: false,
        }
    }

    fn touch(dir: &Utf8Path, seq: u64) {
        std::fs::write(dir.join(file_name(seq)), b"{}\n").unwrap();
    }

    fn names(dir: &Utf8Path) -> Vec<String> {
        let mut names = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn lines(path: &Utf8Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).expect("every line is one JSON object"))
            .collect()
    }

    fn record(frame: &LogFrame) -> serde_json::Value {
        serde_json::to_value(LogRecord::new(frame, 1_700_000_000_000)).unwrap()
    }

    #[test]
    fn sequence_names_round_trip_and_reject_everything_else() {
        assert_eq!(file_name(7), "core-000007.jsonl");
        assert_eq!(file_name(1_234_567), "core-1234567.jsonl");
        assert_eq!(file_seq("core-000007.jsonl"), Some(7));
        assert_eq!(file_seq("core-1234567.jsonl"), Some(1_234_567));
        for alien in [
            "core-.jsonl",
            "core-abc.jsonl",
            "core--1.jsonl",
            "core-7.jsonl.tmp",
            "core-7.pid",
            "config-7.yaml",
            ".manager.lock",
        ] {
            assert_eq!(file_seq(alien), None, "{alien}");
        }
    }

    #[tokio::test]
    async fn the_next_sequence_is_one_past_the_highest_existing_file() {
        let (_guard, dir) = temp_dir();
        assert_eq!(next_seq(&dir).await.unwrap(), 1);
        touch(&dir, 2);
        touch(&dir, 10);
        std::fs::write(dir.join("core-7.pid"), b"").unwrap();
        assert_eq!(next_seq(&dir).await.unwrap(), 11);
    }

    #[test]
    fn pruning_keeps_the_newest_files_by_number_rather_than_by_name() {
        assert_eq!(prune_targets(vec![8, 9, 10, 11, 12], 3), [9, 8]);
        assert_eq!(prune_targets(vec![1, 2], 5), Vec::<u64>::new());
        // Six-wide padding stops agreeing with string order here, which is the
        // whole reason the comparison is numeric.
        assert_eq!(prune_targets(vec![999_999, 1_000_000, 5], 2), [5]);
    }

    #[test]
    fn a_log_record_carries_the_observed_time_and_the_whole_frame() {
        let value = record(&frame("startup frame"));
        assert_eq!(value["t"], "log");
        assert_eq!(value["at"], 1_700_000_000_000_i64);
        assert_eq!(value["epoch"], 7);
        assert_eq!(value["kind"], "mihomo");
        assert_eq!(value["stream"], "stdout");
        assert_eq!(value["level"], "info");
        assert_eq!(value["timestamp"]["unix_ms"], 1_753_719_382_646_i64);
        assert_eq!(value["timestamp"]["inferred"], false);
        assert_eq!(value["target"], serde_json::Value::Null);
        assert_eq!(value["message"], "startup frame");
        assert_eq!(value["fields"][0]["key"], "request");
        assert_eq!(value["truncated"], false);
        assert!(value["raw"].as_str().unwrap().contains("startup frame"));
    }

    /// A frame whose header did not parse has no clock of its own, which is the
    /// reason `at` exists. The key is still present, so a reader never branches.
    #[test]
    fn a_degraded_frame_keeps_a_null_timestamp_and_still_carries_at() {
        let mut degraded = frame("unparsed line");
        degraded.timestamp = None;
        let value = record(&degraded);
        assert_eq!(value["timestamp"], serde_json::Value::Null);
        assert_eq!(value["at"], 1_700_000_000_000_i64);
    }

    /// The parser's 16 KiB budget bounds continuations, not the root line, so an
    /// enormous single line reaches the sink whole. Both texts are cut and the
    /// record says so.
    #[test]
    fn an_oversized_record_is_capped_and_flagged_truncated() {
        let huge = "x".repeat(MAX_TEXT_BYTES * 2);
        let mut oversized = frame("ignored");
        oversized.message = huge.clone();
        oversized.raw = huge;
        let value = record(&oversized);
        assert_eq!(value["message"].as_str().unwrap().len(), MAX_TEXT_BYTES);
        assert_eq!(value["raw"].as_str().unwrap().len(), MAX_TEXT_BYTES);
        assert_eq!(value["truncated"], true);
    }

    /// Multi-byte text must not be cut mid-character, or the line stops being
    /// JSON at all.
    #[test]
    fn capping_cuts_on_a_character_boundary() {
        // Three bytes per char, so the limit never lands on a boundary and the
        // walk-back actually runs.
        let text = "€".repeat(MAX_TEXT_BYTES);
        let (cut, truncated) = clamp(&text);
        assert!(truncated);
        assert!(cut.len() < MAX_TEXT_BYTES, "the boundary walk did not run");
        assert!(text.starts_with(cut));
    }

    #[test]
    fn a_gap_record_reports_the_dropped_count() {
        let value = serde_json::to_value(GapRecord::new(128, 1_700_000_000_000)).unwrap();
        assert_eq!(value["t"], "gap");
        assert_eq!(value["dropped"], 128);
        assert_eq!(value["at"], 1_700_000_000_000_i64);
    }

    #[tokio::test]
    async fn writing_past_the_size_limit_rotates_and_overshoots_by_at_most_one_record() {
        let (_guard, dir) = temp_dir();
        let mut writer = Writer::open(dir.clone(), options(512, 9)).await.unwrap();
        let one = serde_json::to_vec(&LogRecord::new(&frame("rotate me"), now_ms()))
            .unwrap()
            .len() as u64
            + 1;
        assert!(one < 512, "the fixture record must fit under the limit");
        // One record per write until the roll happens, so the assertion below
        // holds whatever the fixture serializes to.
        let mut writes = 0;
        while names(&dir).len() == 1 {
            writer.write(&[Entry::Log(frame("rotate me"))]).await;
            writes += 1;
            assert!(writes < 100, "the writer never rotated");
        }
        drop(writer);

        assert_eq!(names(&dir), ["core-000001.jsonl", "core-000002.jsonl"]);
        let first = std::fs::metadata(dir.join(file_name(1))).unwrap().len();
        assert!(first >= 512, "rotated too early at {first}");
        assert!(
            first < 512 + one,
            "overshot by more than one record: {first}"
        );
    }

    #[tokio::test]
    async fn rotation_prunes_the_oldest_files_beyond_the_retention_limit() {
        let (_guard, dir) = temp_dir();
        let mut writer = Writer::open(dir.clone(), options(1, 2)).await.unwrap();
        for _ in 0..4 {
            writer.write(&[Entry::Log(frame("roll"))]).await;
        }
        drop(writer);

        // max_bytes = 1 means every write after the first starts by rotating
        // (the pre-record check sees a full file); only the newest two survive.
        assert_eq!(names(&dir), ["core-000003.jsonl", "core-000004.jsonl"]);
    }

    /// One oversized batch must not land in one oversized file: the limit is
    /// checked per record, so a burst is split across files and every file
    /// stays within one record of the limit.
    #[tokio::test]
    async fn a_batch_spanning_the_limit_rotates_mid_batch() {
        let (_guard, dir) = temp_dir();
        let mut writer = Writer::open(dir.clone(), options(512, 16)).await.unwrap();
        let one = serde_json::to_vec(&LogRecord::new(&frame("burst"), now_ms()))
            .unwrap()
            .len() as u64
            + 1;
        assert!(one < 512, "the fixture record must fit under the limit");
        let batch = (0..20)
            .map(|_| Entry::Log(frame("burst")))
            .collect::<Vec<_>>();
        writer.write(&batch).await;
        drop(writer);

        let all = names(&dir);
        assert!(all.len() > 1, "one file swallowed the whole burst");
        let mut total = 0;
        for name in &all {
            let path = dir.join(name.as_str());
            total += lines(&path).len();
            let size = std::fs::metadata(&path).unwrap().len();
            assert!(
                size < 512 + one,
                "{name} overshot by more than one record: {size}"
            );
        }
        assert_eq!(total, 20, "records were lost in rotation");
    }

    #[tokio::test]
    async fn startup_prunes_files_left_behind_by_a_previous_run() {
        let (_guard, dir) = temp_dir();
        for seq in 1..=6 {
            touch(&dir, seq);
        }
        let writer = Writer::open(dir.clone(), options(4096, 3)).await.unwrap();
        drop(writer);

        // The freshly opened core-000007 counts toward the retention budget.
        assert_eq!(
            names(&dir),
            [
                "core-000005.jsonl",
                "core-000006.jsonl",
                "core-000007.jsonl"
            ]
        );
    }

    /// A crash can leave the previous file's last line half-written; appending
    /// would splice it onto a fresh record.
    #[tokio::test]
    async fn a_restarted_writer_opens_a_new_file_instead_of_appending() {
        let (_guard, dir) = temp_dir();
        let mut first = Writer::open(dir.clone(), options(4096, 5)).await.unwrap();
        first.write(&[Entry::Log(frame("first run"))]).await;
        drop(first);

        let mut second = Writer::open(dir.clone(), options(4096, 5)).await.unwrap();
        second.write(&[Entry::Log(frame("second run"))]).await;
        drop(second);

        assert_eq!(names(&dir), ["core-000001.jsonl", "core-000002.jsonl"]);
        let one = lines(&dir.join(file_name(1)));
        assert_eq!(one.len(), 1);
        assert_eq!(one[0]["message"], "first run");
        let two = lines(&dir.join(file_name(2)));
        assert_eq!(two.len(), 1);
        assert_eq!(two[0]["message"], "second run");
    }

    /// Deterministic by construction: the receiver exists before anything is
    /// sent, the ring holds two, five go in, and the sender is dropped — so
    /// `run` sees exactly `Lagged(3)`, then the two survivors, then `Closed`.
    #[tokio::test]
    async fn a_lagging_writer_records_the_gap_before_the_surviving_frames() {
        let (_guard, dir) = temp_dir();
        let (log_tx, logs) = tokio::sync::broadcast::channel(2);
        for index in 0..5 {
            log_tx.send(frame(&format!("line {index}"))).unwrap();
        }
        drop(log_tx);

        let writer = Writer::open(dir.clone(), options(1024 * 1024, 5))
            .await
            .unwrap();
        run(writer, logs, CancellationToken::new()).await;

        let records = lines(&dir.join(file_name(1)));
        assert_eq!(records.len(), 3);
        assert_eq!(records[0]["t"], "gap");
        assert_eq!(records[0]["dropped"], 3);
        assert_eq!(records[1]["message"], "line 3");
        assert_eq!(records[2]["message"], "line 4");
    }

    /// The subdirectory has to be hardened explicitly: the parent's DACL is
    /// inheritable, but an inherited descriptor does not carry
    /// `SE_DACL_PROTECTED`, which is what the verifier demands.
    #[tokio::test]
    async fn the_log_directory_is_hardened_like_the_runtime_directory() {
        let (_guard, dir) = temp_dir();
        let logs = prepare_dir(&dir).await.unwrap();
        assert_eq!(logs, dir.join("logs"));
        assert!(logs.is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&logs).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        #[cfg(windows)]
        {
            nyanpasu_utils::io::atomic_fs::verify_windows_directory_acl(&logs).unwrap();
        }
    }
}

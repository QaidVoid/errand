//! What a brokered credential was used for, kept after the fact.
//!
//! One row per provider call a session's broker handled: which session, which
//! provider and credential, where it went, what came of it, and how much
//! crossed. Never a header, a body, a query, a credential or a nonce. Kept in
//! SQLite beside the daemon's other state, written by the daemon and read by
//! the CLI from another process, which is what WAL is for.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OpenFlags, params};

use crate::log::{LogValue, Logger, fields};

/// The file the receipts are kept in, under the state directory.
pub const RECEIPTS_FILENAME: &str = "receipts.db";

/// The shape of the table this errand writes and reads. A store of a later
/// shape is refused rather than read wrongly or written over.
const SCHEMA_VERSION: i64 = 1;

/// The clock receipts are stamped with, in milliseconds since the epoch.
/// Injected, so a test needs no clock.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// What came of a provider call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The provider answered and the answer was relayed to its end. An
    /// upstream error status is still completed; the status says what it was.
    Completed,
    /// The answer failed or was dropped part way.
    Interrupted,
    /// The call did not carry the session's stand-in.
    Denied,
    /// The broker would not forward it, or would not relay the answer.
    Refused,
    /// The provider could not be reached.
    UpstreamUnreachable,
}

impl Outcome {
    /// Every outcome, as stored.
    const ALL: [Self; 5] = [
        Self::Completed,
        Self::Interrupted,
        Self::Denied,
        Self::Refused,
        Self::UpstreamUnreachable,
    ];

    /// The outcome stored under `name`, if it is one.
    fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|outcome| outcome.as_str() == name)
    }

    /// The name the outcome is stored and shown under.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
            Self::Denied => "denied",
            Self::Refused => "refused",
            Self::UpstreamUnreachable => "upstream-unreachable",
        }
    }
}

/// One provider call, as recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// When the call ended, in milliseconds since the epoch.
    pub at: i64,
    /// The session whose broker handled it.
    pub session_id: String,
    /// The provider's route, such as `/provider/zai`.
    pub provider: String,
    /// The variable the credential is configured under, never its value.
    pub credential_name: Option<String>,
    /// The host the call went to, when it got that far.
    pub upstream_host: Option<String>,
    /// The method.
    pub method: String,
    /// The path after the route's prefix.
    pub path: String,
    /// How long the query was, when there was one. Its text is never kept,
    /// since a query can carry a key.
    pub query_bytes: Option<u64>,
    /// The status the session was answered with, when it was answered.
    pub status: Option<u16>,
    /// What came of it.
    pub outcome: Outcome,
    /// From the call arriving to the receipt being written.
    pub elapsed_ms: u64,
    /// Request body bytes forwarded to the provider.
    pub bytes_out: u64,
    /// Answer body bytes relayed to the session.
    pub bytes_in: u64,
}

/// The receipts, in one SQLite file.
///
/// The connection sits behind a mutex and every method is one short
/// statement, so it is safe to write from a stream's `Drop`.
pub struct ReceiptStore {
    db: Mutex<Connection>,
    failed: AtomicU64,
}

impl ReceiptStore {
    /// Opens or creates the store at `path`.
    ///
    /// The file is made readable by the daemon's user alone, as the other
    /// session records are: it holds paths the sessions chose.
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let path = path.as_ref();
        let created = !path.exists();
        let opened = Self::open_at(path, created);
        // A store that was made here and could not be set up is taken away
        // again, so nothing reads a file that holds no table.
        if opened.is_err() && created {
            let _ = std::fs::remove_file(path);
        }
        opened
    }

    fn open_at(path: &Path, create: bool) -> rusqlite::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;

        if create {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .map_err(|error| {
                    rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                        Some(error.to_string()),
                    )
                })?;
        }
        let db = Connection::open(path)?;
        known_shape(&db)?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        // Every call writes a row, so a commit is not made to wait on the
        // disk each time. Under WAL a crash can lose the last few receipts,
        // never the file.
        db.pragma_update(None, "synchronous", "NORMAL")?;
        db.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS broker_receipts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                at INTEGER NOT NULL,
                session_id TEXT NOT NULL,
                provider TEXT NOT NULL,
                credential_name TEXT,
                upstream_host TEXT,
                method TEXT NOT NULL,
                path TEXT NOT NULL,
                query_bytes INTEGER,
                status INTEGER,
                outcome TEXT NOT NULL,
                elapsed_ms INTEGER NOT NULL,
                bytes_out INTEGER NOT NULL,
                bytes_in INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS receipts_by_session
                ON broker_receipts (session_id, id DESC);",
        )?;
        Ok(Self {
            db: Mutex::new(db),
            failed: AtomicU64::new(0),
        })
    }

    /// Writes one receipt.
    ///
    /// A failed write never fails the call it describes, and is never
    /// quiet either: it is counted, and the log says what was lost.
    pub fn record(&self, receipt: &Receipt, log: &Logger) {
        if let Err(error) = self.insert(receipt) {
            let failed = self.failed.fetch_add(1, Ordering::SeqCst) + 1;
            log.warn(
                "a receipt for a provider call could not be written",
                &fields([
                    ("provider", LogValue::from(receipt.provider.as_str())),
                    ("method", LogValue::from(receipt.method.as_str())),
                    ("path", LogValue::from(receipt.path.as_str())),
                    ("detail", LogValue::from(error.to_string())),
                    ("failed_so_far", LogValue::from(failed)),
                ]),
            );
        }
    }

    fn insert(&self, receipt: &Receipt) -> rusqlite::Result<()> {
        let db = self
            .db
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        db.execute(
            "INSERT INTO broker_receipts (at, session_id, provider, credential_name,
                upstream_host, method, path, query_bytes, status, outcome, elapsed_ms,
                bytes_out, bytes_in)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                receipt.at,
                receipt.session_id,
                receipt.provider,
                receipt.credential_name,
                receipt.upstream_host,
                receipt.method,
                receipt.path,
                receipt.query_bytes.map(saturate),
                receipt.status,
                receipt.outcome.as_str(),
                saturate(receipt.elapsed_ms),
                saturate(receipt.bytes_out),
                saturate(receipt.bytes_in),
            ],
        )?;
        Ok(())
    }
}

/// Refuses a store written by a later errand, whose table this one does not
/// know the shape of.
fn known_shape(db: &Connection) -> rusqlite::Result<()> {
    let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH),
            Some(format!(
                "the receipt store is of shape {version}, newer than this errand reads"
            )),
        ));
    }
    Ok(())
}

/// SQLite stores a signed 64-bit integer.
fn saturate(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// The newest `limit` receipts for a session, newest first, read without
/// creating anything: a store that does not exist yet holds no receipts.
pub fn receipts_for(path: &Path, session_id: &str, limit: u32) -> rusqlite::Result<Vec<Receipt>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    known_shape(&db)?;
    let mut statement = db.prepare(
        "SELECT at, session_id, provider, credential_name, upstream_host, method, path,
                query_bytes, status, outcome, elapsed_ms, bytes_out, bytes_in
         FROM broker_receipts WHERE session_id = ?1 ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![session_id, limit], |row| {
        let outcome: String = row.get(9)?;
        let unsigned = |value: i64| u64::try_from(value).unwrap_or(0);
        Ok(Receipt {
            at: row.get(0)?,
            session_id: row.get(1)?,
            provider: row.get(2)?,
            credential_name: row.get(3)?,
            upstream_host: row.get(4)?,
            method: row.get(5)?,
            path: row.get(6)?,
            query_bytes: row.get::<_, Option<i64>>(7)?.map(unsigned),
            status: row.get(8)?,
            outcome: Outcome::parse(&outcome).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    9,
                    rusqlite::types::Type::Text,
                    format!("{outcome} is not an outcome this errand knows").into(),
                )
            })?,
            elapsed_ms: unsigned(row.get(10)?),
            bytes_out: unsigned(row.get(11)?),
            bytes_in: unsigned(row.get(12)?),
        })
    })?;
    rows.collect()
}

/// Where one session's broker writes its receipts.
#[derive(Clone)]
pub struct Ledger {
    /// The store.
    pub store: Arc<ReceiptStore>,
    /// The clock receipts are stamped and timed with.
    pub now: Clock,
    /// The session the broker belongs to.
    pub session_id: String,
}

/// A receipt being written for one call, which writes it exactly once.
///
/// It is written when the call ends rather than when it is answered, since
/// only then are the bytes known: an answer may stream for minutes. A guard
/// dropped before it was finished writes `interrupted`, which is how a
/// session that hung up mid-answer shows.
pub struct ReceiptGuard {
    ledger: Option<Ledger>,
    log: Logger,
    receipt: Receipt,
    started: i64,
    /// Request body bytes forwarded, counted as they go.
    pub bytes_out: Arc<AtomicU64>,
}

impl ReceiptGuard {
    /// A receipt for a call that arrived now. With no ledger, nothing is
    /// ever written.
    pub fn begin(ledger: Option<&Ledger>, log: &Logger, receipt: Receipt) -> Self {
        let started = ledger.map_or(0, |ledger| (ledger.now)());
        Self {
            ledger: ledger.cloned(),
            log: log.clone(),
            receipt,
            started,
            bytes_out: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Notes the host the call went to and the status it was answered with.
    pub fn answered(&mut self, upstream_host: Option<String>, status: Option<u16>) {
        self.receipt.upstream_host = upstream_host;
        self.receipt.status = status;
    }

    /// Notes a status the session was answered with in place of the
    /// provider's, keeping where the call went.
    pub fn answered_instead(&mut self, status: u16) {
        self.receipt.status = Some(status);
    }

    /// Whether an answer with `status` to this call carries no body, so the
    /// session is answered without one ever being read.
    pub fn bodiless(&self, status: u16) -> bool {
        self.receipt.method.eq_ignore_ascii_case("HEAD") || matches!(status, 204 | 304)
    }

    /// Counts answer bytes relayed to the session.
    pub fn relayed(&mut self, bytes: usize) {
        self.receipt.bytes_in = self.receipt.bytes_in.saturating_add(bytes as u64);
    }

    /// Writes the receipt with `outcome`.
    pub fn finish(mut self, outcome: Outcome) {
        self.write(outcome);
    }

    /// Ends the call without a receipt, for one the record already speaks
    /// for.
    pub fn discard(mut self) {
        self.ledger = None;
    }

    fn write(&mut self, outcome: Outcome) {
        let Some(ledger) = self.ledger.take() else {
            return;
        };
        let at = (ledger.now)();
        self.receipt.at = at;
        self.receipt.session_id.clone_from(&ledger.session_id);
        self.receipt.outcome = outcome;
        self.receipt.elapsed_ms = u64::try_from(at.saturating_sub(self.started)).unwrap_or(0);
        self.receipt.bytes_out = self.bytes_out.load(Ordering::SeqCst);
        ledger.store.record(&self.receipt, &self.log);
    }
}

impl Drop for ReceiptGuard {
    fn drop(&mut self) {
        self.write(Outcome::Interrupted);
    }
}

#[cfg(test)]
mod tests;

//! Raw SQLite probes for deterministic shared-cache interleavings: step a
//! statement once without rusqlite's unlock-notify wait, register for the
//! unlock notification by hand, and pause or record a connection's
//! statements with a profile trace hook — which SQLite calls after a
//! statement has released the shared-cache mutex but while its table locks
//! are held.

use std::ffi::{CStr, CString, c_int, c_uint, c_void};
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use rusqlite::{Connection, ffi};

/// How long a paused statement waits to be released.
const PATIENCE: Duration = Duration::from_secs(10);

/// Prepare and step one statement once through SQLite's raw API — no
/// unlock-notify retry — so a shared-cache conflict is observed instead of
/// waited out. Table locks the step took stay held until the connection's
/// transaction ends, finalized statement or not. Returns the result code.
pub fn step_once(conn: &Connection, sql: &str, param: Option<&str>) -> c_int {
    let sql = CString::new(sql).unwrap();
    let param = param.map(|param| CString::new(param).unwrap());
    unsafe {
        let db = conn.handle();
        let mut stmt = ptr::null_mut();
        let rc = ffi::sqlite3_prepare_v2(db, sql.as_ptr(), -1, &mut stmt, ptr::null_mut());
        if rc != ffi::SQLITE_OK {
            return rc;
        }
        if let Some(param) = &param {
            let rc = ffi::sqlite3_bind_text(stmt, 1, param.as_ptr(), -1, ffi::SQLITE_TRANSIENT());
            assert_eq!(rc, ffi::SQLITE_OK);
        }
        let rc = ffi::sqlite3_step(stmt);
        ffi::sqlite3_finalize(stmt);
        rc
    }
}

pub fn is_locked(rc: c_int) -> bool {
    rc & 0xff == ffi::SQLITE_LOCKED
}

pub fn stepped(rc: c_int) -> bool {
    rc == ffi::SQLITE_ROW || rc == ffi::SQLITE_DONE
}

unsafe extern "C" fn unlocked(args: *mut *mut c_void, count: c_int) {
    for index in 0..usize::try_from(count).unwrap_or(0) {
        let sender = unsafe { Box::from_raw(*args.add(index) as *mut Sender<()>) };
        let _ = sender.send(());
    }
}

/// Register for the unlock notification `conn`'s last `SQLITE_LOCKED`
/// earned — what rusqlite's waiting step does before it blocks — without
/// blocking. `Ok` once registered: the receiver fires when the blocking
/// transaction ends. `Err` when SQLite refuses because waiting would close
/// a cycle (the deadlock rusqlite reports as `SQLITE_LOCKED`).
pub fn register_unlock(conn: &Connection) -> Result<Receiver<()>, c_int> {
    let (sender, receiver) = mpsc::channel();
    let arg = Box::into_raw(Box::new(sender)) as *mut c_void;
    let rc = unsafe { ffi::sqlite3_unlock_notify(conn.handle(), Some(unlocked), arg) };
    if rc == ffi::SQLITE_OK {
        Ok(receiver)
    } else {
        drop(unsafe { Box::from_raw(arg as *mut Sender<()>) });
        Err(rc)
    }
}

/// Pauses a connection after the first completed statement that `matches`,
/// with that statement's table locks still held, until released.
pub struct Pause {
    gate: &'static Gate,
    reached: Receiver<String>,
    resume: Sender<()>,
}

struct Gate {
    matches: fn(&str) -> bool,
    armed: AtomicBool,
    reached: Mutex<Sender<String>>,
    resume: Mutex<Receiver<()>>,
}

unsafe extern "C" fn traced(
    kind: c_uint,
    ctx: *mut c_void,
    stmt: *mut c_void,
    _: *mut c_void,
) -> c_int {
    if kind != ffi::SQLITE_TRACE_PROFILE as c_uint {
        return 0;
    }
    let gate = unsafe { &*(ctx as *const Gate) };
    let sql = unsafe { ffi::sqlite3_sql(stmt as *mut ffi::sqlite3_stmt) };
    if sql.is_null() {
        return 0;
    }
    let sql = unsafe { CStr::from_ptr(sql) }.to_string_lossy();
    if (gate.matches)(sql.trim()) && gate.armed.swap(false, Ordering::SeqCst) {
        if let Ok(reached) = gate.reached.lock() {
            let _ = reached.send(sql.trim().to_string());
        }
        if let Ok(resume) = gate.resume.lock() {
            let _ = resume.recv_timeout(PATIENCE);
        }
    }
    0
}

impl Pause {
    pub fn after(conn: &Connection, matches: fn(&str) -> bool) -> Self {
        let (reached_tx, reached) = mpsc::channel();
        let (resume, resume_rx) = mpsc::channel();
        // Leaked: the trace may fire on the writer thread after a failed
        // assertion here unwinds, so the gate must outlive the test body.
        let gate: &'static Gate = Box::leak(Box::new(Gate {
            matches,
            armed: AtomicBool::new(true),
            reached: Mutex::new(reached_tx),
            resume: Mutex::new(resume_rx),
        }));
        let rc = unsafe {
            ffi::sqlite3_trace_v2(
                conn.handle(),
                ffi::SQLITE_TRACE_PROFILE as c_uint,
                Some(traced),
                gate as *const Gate as *mut c_void,
            )
        };
        assert_eq!(rc, ffi::SQLITE_OK);
        Self {
            gate,
            reached,
            resume,
        }
    }

    /// The paused statement, or `None` when none completed in `within`.
    pub fn reached(&self, within: Duration) -> Option<String> {
        match self.reached.recv_timeout(within) {
            Ok(sql) => Some(sql),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => panic!("pause gate closed"),
        }
    }

    /// Stop pausing; true when the gate had not fired yet.
    pub fn disarm(&self) -> bool {
        self.gate.armed.swap(false, Ordering::SeqCst)
    }

    pub fn release(&self) {
        self.resume.send(()).unwrap();
    }
}

pub fn untrace(conn: &Connection) {
    unsafe { ffi::sqlite3_trace_v2(conn.handle(), 0, None, ptr::null_mut()) };
}

/// Every statement a connection completes, in order.
pub struct Recorder(&'static Mutex<Vec<String>>);

unsafe extern "C" fn recorded(
    kind: c_uint,
    ctx: *mut c_void,
    stmt: *mut c_void,
    _: *mut c_void,
) -> c_int {
    if kind != ffi::SQLITE_TRACE_PROFILE as c_uint {
        return 0;
    }
    let log = unsafe { &*(ctx as *const Mutex<Vec<String>>) };
    let sql = unsafe { ffi::sqlite3_sql(stmt as *mut ffi::sqlite3_stmt) };
    if !sql.is_null()
        && let Ok(mut log) = log.lock()
    {
        log.push(
            unsafe { CStr::from_ptr(sql) }
                .to_string_lossy()
                .trim()
                .to_string(),
        );
    }
    0
}

impl Recorder {
    pub fn on(conn: &Connection) -> Self {
        let log: &'static Mutex<Vec<String>> = Box::leak(Box::new(Mutex::new(Vec::new())));
        let rc = unsafe {
            ffi::sqlite3_trace_v2(
                conn.handle(),
                ffi::SQLITE_TRACE_PROFILE as c_uint,
                Some(recorded),
                log as *const Mutex<Vec<String>> as *mut c_void,
            )
        };
        assert_eq!(rc, ffi::SQLITE_OK);
        Self(log)
    }

    pub fn statements(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

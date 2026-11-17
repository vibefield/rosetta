//! Full decoded-prefix verification for audits and representation transitions.
use super::*;

pub(super) fn verify_chain(
    file: &mut File,
    path: &Path,
    previous: &TailCheckpoint,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<CommittedVerification, CaptureError> {
    file.rewind()
        .map_err(|e| io_error("rewinding verification", path, e))?;
    let mut remaining = previous.committed_offset;
    let mut chain = CONTENT_CHAIN_BASIS;
    let mut buffer = [0_u8; READ_BUFFER_BYTES];
    while remaining > 0 {
        if cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
            return Err(io_error(
                "verifying decoded prefix",
                path,
                std::io::Error::new(std::io::ErrorKind::Interrupted, "capture cancelled"),
            ));
        }
        let wanted = usize::try_from(remaining.min(READ_BUFFER_BYTES as u64))
            .expect("bounded read length always fits usize");
        let count = file
            .read(&mut buffer[..wanted])
            .map_err(|error| io_error("verifying", path, error))?;
        if count == 0 {
            return Ok(CommittedVerification::Truncated);
        }
        chain = chain_bytes(chain, &buffer[..count]);
        remaining -= count as u64;
    }
    Ok(match previous.committed_chain {
        Some(expected) if expected == chain => CommittedVerification::Verified,
        Some(_) => CommittedVerification::Mismatch,
        None => CommittedVerification::Established(chain),
    })
}

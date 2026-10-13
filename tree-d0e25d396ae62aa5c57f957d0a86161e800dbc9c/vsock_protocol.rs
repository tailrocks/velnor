//! Versioned host/guest vsock protocol for the microVM backend.
//!
//! Frames have a fixed header, explicit maximum lengths, and a checksum.
//! The guest never receives runner-registration keys or host signing material.

use std::io::{Read, Write};

use sha2::{Digest, Sha256};

use crate::job_summary::JobConclusion;

/// Protocol version. Mismatch fails closed.
pub const PROTOCOL_VERSION: u16 = 2;
/// Maximum payload bytes per frame (1 MiB).
pub const MAX_PAYLOAD_BYTES: u32 = 1024 * 1024;
/// stdout stream tag in [`VsockMessage::Stdio`].
pub const STDOUT_STREAM: u8 = 1;
/// stderr stream tag in [`VsockMessage::Stdio`].
pub const STDERR_STREAM: u8 = 2;

const HEADER_LEN: usize = 8;
const CHECKSUM_LEN: usize = 32;

/// Typed vsock messages. Host control uses vsock, never SSH or a TCP port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VsockMessage {
    GuestReady {
        isolation_id: String,
        generation: u64,
        docker_healthy: bool,
        job_credentials_absent: bool,
    },
    DeliverPlan {
        job_id: String,
        isolation_id: String,
        generation: u64,
        plan_bytes: Vec<u8>,
    },
    ImportBlob {
        digest_sha256: String,
        bytes: Vec<u8>,
    },
    StepStarted {
        step_id: String,
    },
    StepCompleted {
        step_id: String,
        exit_code: i32,
    },
    Stdio {
        stream: u8,
        bytes: Vec<u8>,
    },
    CommandFile {
        path: String,
        bytes: Vec<u8>,
    },
    Annotation {
        text: String,
    },
    Cancel,
    Telemetry {
        cpu_millis: u64,
        memory_bytes: u64,
    },
    ResultExport {
        digest_sha256: String,
        bytes: Vec<u8>,
    },
    TeardownAck {
        job_id: String,
        isolation_id: String,
        generation: u64,
    },
    JobCompleted {
        conclusion: JobConclusion,
        exit_code: i32,
    },
}

impl VsockMessage {
    #[must_use]
    pub fn kind(&self) -> u16 {
        match self {
            Self::GuestReady { .. } => 1,
            Self::DeliverPlan { .. } => 2,
            Self::ImportBlob { .. } => 3,
            Self::StepStarted { .. } => 4,
            Self::StepCompleted { .. } => 5,
            Self::Stdio { .. } => 6,
            Self::CommandFile { .. } => 7,
            Self::Annotation { .. } => 8,
            Self::Cancel => 9,
            Self::Telemetry { .. } => 10,
            Self::ResultExport { .. } => 11,
            Self::TeardownAck { .. } => 12,
            Self::JobCompleted { .. } => 13,
        }
    }

    /// Encode with version, kind, length, payload, SHA-256 checksum.
    ///
    /// # Errors
    /// [`VsockCodecError::PayloadTooLarge`] when the payload exceeds the max.
    pub fn encode(&self) -> Result<Vec<u8>, VsockCodecError> {
        let payload = encode_payload(self)?;
        if payload.len() > MAX_PAYLOAD_BYTES as usize {
            return Err(VsockCodecError::PayloadTooLarge { len: payload.len() });
        }
        let mut frame = Vec::with_capacity(HEADER_LEN + payload.len() + CHECKSUM_LEN);
        frame.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
        frame.extend_from_slice(&self.kind().to_be_bytes());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&payload);
        let digest = Sha256::digest(&frame);
        frame.extend_from_slice(&digest);
        Ok(frame)
    }

    /// Write one framed message. Callers must not concatenate raw payloads.
    ///
    /// # Errors
    /// Encode or write failure.
    pub fn write_to<W: Write>(&self, writer: &mut W) -> Result<(), VsockCodecError> {
        let bytes = self.encode()?;
        writer
            .write_all(&bytes)
            .map_err(|error| VsockCodecError::Io {
                detail: error.to_string(),
            })
    }

    /// Read exactly one framed message from a stream.
    ///
    /// # Errors
    /// Truncation, IO, or decode failure.
    pub fn read_from<R: Read>(reader: &mut R) -> Result<Self, VsockCodecError> {
        let mut header = [0_u8; HEADER_LEN];
        reader
            .read_exact(&mut header)
            .map_err(|error| VsockCodecError::Io {
                detail: error.to_string(),
            })?;
        let payload_len = u32::from_be_bytes(
            header[4..8]
                .try_into()
                .map_err(|_| VsockCodecError::Truncated { len: HEADER_LEN })?,
        ) as usize;
        if payload_len > MAX_PAYLOAD_BYTES as usize {
            return Err(VsockCodecError::PayloadTooLarge { len: payload_len });
        }
        let mut rest = vec![0_u8; payload_len + CHECKSUM_LEN];
        reader
            .read_exact(&mut rest)
            .map_err(|error| VsockCodecError::Io {
                detail: error.to_string(),
            })?;
        let mut frame = Vec::with_capacity(HEADER_LEN + rest.len());
        frame.extend_from_slice(&header);
        frame.extend_from_slice(&rest);
        Self::decode(&frame)
    }

    /// Decode one frame. Truncation, version mismatch, bad checksum, and
    /// unknown kinds fail closed.
    ///
    /// # Errors
    /// [`VsockCodecError`] for every malformed input class.
    pub fn decode(bytes: &[u8]) -> Result<Self, VsockCodecError> {
        if bytes.len() < HEADER_LEN + CHECKSUM_LEN {
            return Err(VsockCodecError::Truncated { len: bytes.len() });
        }
        let version = u16::from_be_bytes(
            bytes[0..2]
                .try_into()
                .map_err(|_| VsockCodecError::Truncated { len: bytes.len() })?,
        );
        if version != PROTOCOL_VERSION {
            return Err(VsockCodecError::VersionMismatch { version });
        }
        let kind = u16::from_be_bytes(
            bytes[2..4]
                .try_into()
                .map_err(|_| VsockCodecError::Truncated { len: bytes.len() })?,
        );
        let payload_len = u32::from_be_bytes(
            bytes[4..8]
                .try_into()
                .map_err(|_| VsockCodecError::Truncated { len: bytes.len() })?,
        );
        if payload_len > MAX_PAYLOAD_BYTES {
            return Err(VsockCodecError::PayloadTooLarge {
                len: payload_len as usize,
            });
        }
        let payload_end = HEADER_LEN + payload_len as usize;
        let checksum_end = payload_end + CHECKSUM_LEN;
        if bytes.len() != checksum_end {
            return Err(VsockCodecError::Truncated { len: bytes.len() });
        }
        let expected = Sha256::digest(&bytes[..payload_end]);
        if expected.as_slice() != &bytes[payload_end..checksum_end] {
            return Err(VsockCodecError::ChecksumMismatch);
        }
        decode_payload(kind, &bytes[HEADER_LEN..payload_end])
    }
}

fn encode_payload(message: &VsockMessage) -> Result<Vec<u8>, VsockCodecError> {
    let mut payload = Vec::new();
    match message {
        VsockMessage::GuestReady {
            isolation_id,
            generation,
            docker_healthy,
            job_credentials_absent,
        } => {
            write_string(&mut payload, isolation_id)?;
            payload.extend_from_slice(&generation.to_be_bytes());
            payload.push(u8::from(*docker_healthy));
            payload.push(u8::from(*job_credentials_absent));
        }
        VsockMessage::DeliverPlan {
            job_id,
            isolation_id,
            generation,
            plan_bytes,
        } => {
            write_string(&mut payload, job_id)?;
            write_string(&mut payload, isolation_id)?;
            payload.extend_from_slice(&generation.to_be_bytes());
            write_bytes(&mut payload, plan_bytes)?;
        }
        VsockMessage::ImportBlob {
            digest_sha256,
            bytes,
        } => {
            write_string(&mut payload, digest_sha256)?;
            write_bytes(&mut payload, bytes)?;
        }
        VsockMessage::StepStarted { step_id } => write_string(&mut payload, step_id)?,
        VsockMessage::StepCompleted { step_id, exit_code } => {
            write_string(&mut payload, step_id)?;
            payload.extend_from_slice(&exit_code.to_be_bytes());
        }
        VsockMessage::Stdio { stream, bytes } => {
            payload.push(*stream);
            write_bytes(&mut payload, bytes)?;
        }
        VsockMessage::CommandFile { path, bytes } => {
            write_string(&mut payload, path)?;
            write_bytes(&mut payload, bytes)?;
        }
        VsockMessage::Annotation { text } => write_string(&mut payload, text)?,
        VsockMessage::Cancel => {}
        VsockMessage::Telemetry {
            cpu_millis,
            memory_bytes,
        } => {
            payload.extend_from_slice(&cpu_millis.to_be_bytes());
            payload.extend_from_slice(&memory_bytes.to_be_bytes());
        }
        VsockMessage::ResultExport {
            digest_sha256,
            bytes,
        } => {
            write_string(&mut payload, digest_sha256)?;
            write_bytes(&mut payload, bytes)?;
        }
        VsockMessage::TeardownAck {
            job_id,
            isolation_id,
            generation,
        } => {
            write_string(&mut payload, job_id)?;
            write_string(&mut payload, isolation_id)?;
            payload.extend_from_slice(&generation.to_be_bytes());
        }
        VsockMessage::JobCompleted {
            conclusion,
            exit_code,
        } => {
            write_string(&mut payload, conclusion.as_str())?;
            payload.extend_from_slice(&exit_code.to_be_bytes());
        }
    }
    Ok(payload)
}

fn decode_payload(kind: u16, payload: &[u8]) -> Result<VsockMessage, VsockCodecError> {
    let mut cur = payload;
    let message = match kind {
        1 => {
            let isolation_id = read_string(&mut cur)?;
            let generation = read_u64(&mut cur)?;
            let docker_healthy = read_u8(&mut cur)? != 0;
            let job_credentials_absent = read_u8(&mut cur)? != 0;
            VsockMessage::GuestReady {
                isolation_id,
                generation,
                docker_healthy,
                job_credentials_absent,
            }
        }
        2 => {
            let job_id = read_string(&mut cur)?;
            let isolation_id = read_string(&mut cur)?;
            let generation = read_u64(&mut cur)?;
            let plan_bytes = read_bytes(&mut cur)?;
            VsockMessage::DeliverPlan {
                job_id,
                isolation_id,
                generation,
                plan_bytes,
            }
        }
        3 => {
            let digest_sha256 = read_string(&mut cur)?;
            let bytes = read_bytes(&mut cur)?;
            VsockMessage::ImportBlob {
                digest_sha256,
                bytes,
            }
        }
        4 => VsockMessage::StepStarted {
            step_id: read_string(&mut cur)?,
        },
        5 => {
            let step_id = read_string(&mut cur)?;
            let exit_code = read_i32(&mut cur)?;
            VsockMessage::StepCompleted { step_id, exit_code }
        }
        6 => {
            let stream = read_u8(&mut cur)?;
            if stream != STDOUT_STREAM && stream != STDERR_STREAM {
                return Err(VsockCodecError::InvalidStream { value: stream });
            }
            let bytes = read_bytes(&mut cur)?;
            VsockMessage::Stdio { stream, bytes }
        }
        7 => {
            let path = read_string(&mut cur)?;
            let bytes = read_bytes(&mut cur)?;
            VsockMessage::CommandFile { path, bytes }
        }
        8 => VsockMessage::Annotation {
            text: read_string(&mut cur)?,
        },
        9 => VsockMessage::Cancel,
        10 => VsockMessage::Telemetry {
            cpu_millis: read_u64(&mut cur)?,
            memory_bytes: read_u64(&mut cur)?,
        },
        11 => {
            let digest_sha256 = read_string(&mut cur)?;
            let bytes = read_bytes(&mut cur)?;
            VsockMessage::ResultExport {
                digest_sha256,
                bytes,
            }
        }
        12 => {
            let job_id = read_string(&mut cur)?;
            let isolation_id = read_string(&mut cur)?;
            let generation = read_u64(&mut cur)?;
            VsockMessage::TeardownAck {
                job_id,
                isolation_id,
                generation,
            }
        }
        13 => {
            let raw = read_string(&mut cur)?;
            let conclusion = JobConclusion::try_from(raw.as_str())
                .map_err(|_| VsockCodecError::InvalidConclusion { value: raw })?;
            let exit_code = read_i32(&mut cur)?;
            VsockMessage::JobCompleted {
                conclusion,
                exit_code,
            }
        }
        other => return Err(VsockCodecError::UnknownKind { kind: other }),
    };
    if !cur.is_empty() {
        return Err(VsockCodecError::TrailingBytes { len: cur.len() });
    }
    Ok(message)
}

fn write_string(out: &mut Vec<u8>, value: &str) -> Result<(), VsockCodecError> {
    write_bytes(out, value.as_bytes())
}

fn write_bytes(out: &mut Vec<u8>, value: &[u8]) -> Result<(), VsockCodecError> {
    let len = u32::try_from(value.len())
        .map_err(|_| VsockCodecError::PayloadTooLarge { len: value.len() })?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(value);
    Ok(())
}

fn read_string(cur: &mut &[u8]) -> Result<String, VsockCodecError> {
    let bytes = read_bytes(cur)?;
    String::from_utf8(bytes).map_err(|_| VsockCodecError::InvalidUtf8)
}

fn read_bytes(cur: &mut &[u8]) -> Result<Vec<u8>, VsockCodecError> {
    let len = read_u32(cur)? as usize;
    if cur.len() < len {
        return Err(VsockCodecError::Truncated { len: cur.len() });
    }
    let (head, tail) = cur.split_at(len);
    *cur = tail;
    Ok(head.to_vec())
}

fn read_u8(cur: &mut &[u8]) -> Result<u8, VsockCodecError> {
    if cur.is_empty() {
        return Err(VsockCodecError::Truncated { len: 0 });
    }
    let value = cur[0];
    *cur = &cur[1..];
    Ok(value)
}

fn read_u32(cur: &mut &[u8]) -> Result<u32, VsockCodecError> {
    if cur.len() < 4 {
        return Err(VsockCodecError::Truncated { len: cur.len() });
    }
    let value = u32::from_be_bytes(
        cur[..4]
            .try_into()
            .map_err(|_| VsockCodecError::Truncated { len: cur.len() })?,
    );
    *cur = &cur[4..];
    Ok(value)
}

fn read_u64(cur: &mut &[u8]) -> Result<u64, VsockCodecError> {
    if cur.len() < 8 {
        return Err(VsockCodecError::Truncated { len: cur.len() });
    }
    let value = u64::from_be_bytes(
        cur[..8]
            .try_into()
            .map_err(|_| VsockCodecError::Truncated { len: cur.len() })?,
    );
    *cur = &cur[8..];
    Ok(value)
}

fn read_i32(cur: &mut &[u8]) -> Result<i32, VsockCodecError> {
    if cur.len() < 4 {
        return Err(VsockCodecError::Truncated { len: cur.len() });
    }
    let value = i32::from_be_bytes(
        cur[..4]
            .try_into()
            .map_err(|_| VsockCodecError::Truncated { len: cur.len() })?,
    );
    *cur = &cur[4..];
    Ok(value)
}

/// Malformed vsock frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VsockCodecError {
    Truncated { len: usize },
    VersionMismatch { version: u16 },
    PayloadTooLarge { len: usize },
    ChecksumMismatch,
    UnknownKind { kind: u16 },
    InvalidUtf8,
    TrailingBytes { len: usize },
    Io { detail: String },
    InvalidConclusion { value: String },
    InvalidStream { value: u8 },
}

impl std::fmt::Display for VsockCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated { len } => write!(f, "vsock frame truncated ({len} bytes)"),
            Self::VersionMismatch { version } => {
                write!(f, "vsock protocol version {version} != {PROTOCOL_VERSION}")
            }
            Self::PayloadTooLarge { len } => {
                write!(f, "vsock payload {len} exceeds {MAX_PAYLOAD_BYTES}")
            }
            Self::ChecksumMismatch => write!(f, "vsock checksum mismatch"),
            Self::UnknownKind { kind } => write!(f, "vsock unknown kind {kind}"),
            Self::InvalidUtf8 => write!(f, "vsock payload is not UTF-8"),
            Self::TrailingBytes { len } => write!(f, "vsock trailing payload {len} bytes"),
            Self::Io { detail } => write!(f, "vsock io: {detail}"),
            Self::InvalidConclusion { value } => {
                write!(f, "vsock unknown job conclusion {value}")
            }
            Self::InvalidStream { value } => {
                write!(
                    f,
                    "vsock unknown stdio stream {value}; expected {STDOUT_STREAM} or {STDERR_STREAM}"
                )
            }
        }
    }
}

impl std::error::Error for VsockCodecError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job_summary::JobConclusion;

    #[test]
    fn round_trip_guest_ready_and_cancel() {
        let ready = VsockMessage::GuestReady {
            isolation_id: "job-1".into(),
            generation: 7,
            docker_healthy: true,
            job_credentials_absent: true,
        };
        let bytes = ready.encode().unwrap();
        assert_eq!(VsockMessage::decode(&bytes).unwrap(), ready);
        let cancel = VsockMessage::Cancel.encode().unwrap();
        assert_eq!(VsockMessage::decode(&cancel).unwrap(), VsockMessage::Cancel);
        let completed = VsockMessage::JobCompleted {
            conclusion: JobConclusion::TimedOut,
            exit_code: 1,
        };
        let bytes = completed.encode().unwrap();
        assert_eq!(VsockMessage::decode(&bytes).unwrap(), completed);
    }

    #[test]
    fn malformed_frames_fail_closed() {
        assert!(matches!(
            VsockMessage::decode(&[0, 1, 0, 1]),
            Err(VsockCodecError::Truncated { .. })
        ));
        let mut ready = VsockMessage::Cancel.encode().unwrap();
        ready[0] = 0;
        ready[1] = 99;
        assert!(matches!(
            VsockMessage::decode(&ready),
            Err(VsockCodecError::VersionMismatch { version: 99 })
        ));
        let mut bad = VsockMessage::Cancel.encode().unwrap();
        let last = bad.len() - 1;
        bad[last] ^= 0xff;
        assert_eq!(
            VsockMessage::decode(&bad),
            Err(VsockCodecError::ChecksumMismatch)
        );
    }

    #[test]
    fn read_from_splits_concatenated_frames() {
        use std::io::Cursor;
        let mut buf = VsockMessage::Cancel.encode().unwrap();
        buf.extend(
            VsockMessage::GuestReady {
                isolation_id: "job-1".into(),
                generation: 1,
                docker_healthy: true,
                job_credentials_absent: true,
            }
            .encode()
            .unwrap(),
        );
        let mut cursor = Cursor::new(buf);
        assert_eq!(
            VsockMessage::read_from(&mut cursor).unwrap(),
            VsockMessage::Cancel
        );
        assert_eq!(
            VsockMessage::read_from(&mut cursor).unwrap(),
            VsockMessage::GuestReady {
                isolation_id: "job-1".into(),
                generation: 1,
                docker_healthy: true,
                job_credentials_absent: true,
            }
        );
    }
}

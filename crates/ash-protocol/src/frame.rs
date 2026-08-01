//! Length-prefixed framing for persistent `ash rpc` sessions.

use std::io;

use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::ason::{DecodeError, Document, Limits, decode_with_limits};

pub const HARD_MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const DEFAULT_MAX_FRAME_BYTES: usize = 1024 * 1024;

/// A bounded codec for four-byte big-endian ASH/1 frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameCodec {
    max_payload: usize,
}

impl FrameCodec {
    pub fn new(max_payload: usize) -> Result<Self, FrameError> {
        if max_payload == 0 || max_payload > HARD_MAX_FRAME_BYTES {
            return Err(FrameError::InvalidLimit {
                requested: max_payload,
                hard_max: HARD_MAX_FRAME_BYTES,
            });
        }
        Ok(Self { max_payload })
    }

    #[must_use]
    pub const fn max_payload(self) -> usize {
        self.max_payload
    }

    /// Reads one frame. Clean EOF before a prefix returns `None`.
    pub async fn read_frame<R>(&self, reader: &mut R) -> Result<Option<Vec<u8>>, FrameError>
    where
        R: AsyncRead + Unpin,
    {
        let mut prefix = [0_u8; 4];
        let mut prefix_bytes = 0;
        while prefix_bytes < prefix.len() {
            let read = reader
                .read(&mut prefix[prefix_bytes..])
                .await
                .map_err(FrameError::Io)?;
            if read == 0 {
                return if prefix_bytes == 0 {
                    Ok(None)
                } else {
                    Err(FrameError::TruncatedPrefix {
                        received: prefix_bytes,
                    })
                };
            }
            prefix_bytes += read;
        }

        let length = u32::from_be_bytes(prefix) as usize;
        if length == 0 {
            return Err(FrameError::ZeroLength);
        }
        if length > self.max_payload {
            return Err(FrameError::PayloadTooLarge {
                declared: length,
                max: self.max_payload,
            });
        }

        let mut payload = vec![0_u8; length];
        let mut payload_bytes = 0;
        while payload_bytes < length {
            let read = reader
                .read(&mut payload[payload_bytes..])
                .await
                .map_err(FrameError::Io)?;
            if read == 0 {
                return Err(FrameError::TruncatedPayload {
                    expected: length,
                    received: payload_bytes,
                });
            }
            payload_bytes += read;
        }
        Ok(Some(payload))
    }

    /// Writes one frame without flushing the caller-owned stream.
    pub async fn write_frame<W>(&self, writer: &mut W, payload: &[u8]) -> Result<(), FrameError>
    where
        W: AsyncWrite + Unpin,
    {
        if payload.is_empty() {
            return Err(FrameError::ZeroLength);
        }
        if payload.len() > self.max_payload {
            return Err(FrameError::PayloadTooLarge {
                declared: payload.len(),
                max: self.max_payload,
            });
        }
        let length = u32::try_from(payload.len()).map_err(|_| FrameError::PayloadTooLarge {
            declared: payload.len(),
            max: self.max_payload,
        })?;
        writer
            .write_all(&length.to_be_bytes())
            .await
            .map_err(FrameError::Io)?;
        writer.write_all(payload).await.map_err(FrameError::Io)
    }

    /// Reads a frame, validates UTF-8 and ASON, and requires canonical bytes.
    pub async fn read_document<R>(
        &self,
        reader: &mut R,
        limits: &Limits,
    ) -> Result<Option<Document>, ProtocolReadError>
    where
        R: AsyncRead + Unpin,
    {
        let Some(payload) = self.read_frame(reader).await? else {
            return Ok(None);
        };
        let text =
            std::str::from_utf8(&payload).map_err(|error| ProtocolReadError::InvalidUtf8 {
                valid_up_to: error.valid_up_to(),
            })?;
        let document = decode_with_limits(text, limits)?;
        if document.encode().as_bytes() != payload {
            return Err(ProtocolReadError::NonCanonical);
        }
        Ok(Some(document))
    }

    /// Writes a document using its canonical ASON encoding.
    pub async fn write_document<W>(
        &self,
        writer: &mut W,
        document: &Document,
    ) -> Result<(), FrameError>
    where
        W: AsyncWrite + Unpin,
    {
        self.write_frame(writer, document.encode().as_bytes()).await
    }
}

impl Default for FrameCodec {
    fn default() -> Self {
        Self {
            max_payload: DEFAULT_MAX_FRAME_BYTES,
        }
    }
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame limit {requested} is outside 1..={hard_max}")]
    InvalidLimit { requested: usize, hard_max: usize },
    #[error("zero-length ASH frames are invalid")]
    ZeroLength,
    #[error("frame declares {declared} bytes, exceeding the limit of {max}")]
    PayloadTooLarge { declared: usize, max: usize },
    #[error("EOF after {received} of 4 frame-prefix bytes")]
    TruncatedPrefix { received: usize },
    #[error("EOF after {received} of {expected} payload bytes")]
    TruncatedPayload { expected: usize, received: usize },
    #[error("frame I/O failed: {0}")]
    Io(#[source] io::Error),
}

#[derive(Debug, Error)]
pub enum ProtocolReadError {
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("frame payload is not UTF-8; valid prefix ends at byte {valid_up_to}")]
    InvalidUtf8 { valid_up_to: usize },
    #[error(transparent)]
    Ason(#[from] DecodeError),
    #[error("framed ASON payload is valid but not canonical")]
    NonCanonical,
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncWriteExt, duplex};

    use super::{FrameCodec, FrameError, ProtocolReadError};
    use crate::ason::Limits;

    #[tokio::test]
    async fn frames_round_trip_and_clean_eof_is_distinct() {
        let codec = FrameCodec::new(64).expect("valid limit");
        let (mut client, mut server) = duplex(128);

        codec
            .write_frame(&mut client, b"t:1\n")
            .await
            .expect("write frame");
        client.shutdown().await.expect("shutdown writer");

        assert_eq!(
            codec.read_frame(&mut server).await.expect("read frame"),
            Some(b"t:1\n".to_vec())
        );
        assert_eq!(
            codec.read_frame(&mut server).await.expect("clean EOF"),
            None
        );
    }

    #[tokio::test]
    async fn declared_limit_is_rejected_before_payload_read() {
        let codec = FrameCodec::new(8).expect("valid limit");
        let (mut client, mut server) = duplex(16);
        client
            .write_all(&9_u32.to_be_bytes())
            .await
            .expect("write prefix");

        assert!(matches!(
            codec.read_frame(&mut server).await,
            Err(FrameError::PayloadTooLarge {
                declared: 9,
                max: 8
            })
        ));
    }

    #[tokio::test]
    async fn truncated_prefix_and_payload_are_distinct() {
        let codec = FrameCodec::new(16).expect("valid limit");

        let (mut prefix_writer, mut prefix_reader) = duplex(16);
        prefix_writer
            .write_all(&[0, 0])
            .await
            .expect("write prefix");
        prefix_writer.shutdown().await.expect("shutdown");
        assert!(matches!(
            codec.read_frame(&mut prefix_reader).await,
            Err(FrameError::TruncatedPrefix { received: 2 })
        ));

        let (mut payload_writer, mut payload_reader) = duplex(16);
        payload_writer
            .write_all(&[0, 0, 0, 4, b'a', b'b'])
            .await
            .expect("write partial payload");
        payload_writer.shutdown().await.expect("shutdown");
        assert!(matches!(
            codec.read_frame(&mut payload_reader).await,
            Err(FrameError::TruncatedPayload {
                expected: 4,
                received: 2
            })
        ));
    }

    #[tokio::test]
    async fn document_reader_rejects_noncanonical_ason() {
        let codec = FrameCodec::new(64).expect("valid limit");
        let (mut client, mut server) = duplex(128);
        codec
            .write_frame(&mut client, b"v:\"safe\"\n")
            .await
            .expect("write frame");

        assert!(matches!(
            codec.read_document(&mut server, &Limits::default()).await,
            Err(ProtocolReadError::NonCanonical)
        ));
    }
}

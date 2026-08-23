// ChunkedBody: turn Vec<Bytes> into a REPLAYABLE ByteStream without copying.
//
// Why this exists: a part body is not always one contiguous slice — the ring
// read can wrap (2 slices) and part #1 is prepended with the 64-byte unique
// header. Concatenating would memcpy up to 256 MiB per part; instead we stream
// the chunks as-is. `SdkBody::retryable` keeps the body replayable (each
// rebuild only clones `Bytes` refcounts), so our retry loop can safely resend.

use aws_smithy_types::body::SdkBody;
use aws_smithy_types::byte_stream::ByteStream;
use bytes::Bytes;
use http_body::{Frame, SizeHint};
use std::pin::Pin;
use std::task::{Context, Poll};

struct ChunkedHttpBody {
    chunks: Vec<Bytes>,
    idx: usize,
    remaining: u64,
}

impl ChunkedHttpBody {
    fn new(chunks: Vec<Bytes>) -> Self {
        let remaining = chunks.iter().map(|c| c.len() as u64).sum();
        Self {
            chunks,
            idx: 0,
            remaining,
        }
    }
}

impl http_body::Body for ChunkedHttpBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.idx < this.chunks.len() {
            let chunk = this.chunks[this.idx].clone(); // refcount clone only
            this.idx += 1;
            this.remaining -= chunk.len() as u64;
            Poll::Ready(Some(Ok(Frame::data(chunk))))
        } else {
            Poll::Ready(None)
        }
    }

    fn is_end_stream(&self) -> bool {
        self.idx >= self.chunks.len()
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.remaining)
    }
}

/// Build a replayable ByteStream from refcounted chunks.
pub fn replayable_stream(chunks: Vec<Bytes>) -> ByteStream {
    let make = move || SdkBody::from_body_1_x(ChunkedHttpBody::new(chunks.clone()));
    ByteStream::new(SdkBody::retryable(make))
}

/// Total byte length of a chunk list (used for content-length).
pub fn chunks_len(chunks: &[Bytes]) -> u64 {
    chunks.iter().map(|c| c.len() as u64).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stream_yields_all_chunks_in_order() {
        let chunks = vec![Bytes::from_static(b"hello "), Bytes::from_static(b"world")];
        let stream = replayable_stream(chunks.clone());
        let collected = stream.collect().await.unwrap().into_bytes();
        assert_eq!(&collected[..], b"hello world");
    }

    #[tokio::test]
    async fn stream_is_replayable() {
        let chunks = vec![Bytes::from_static(b"abc"), Bytes::from_static(b"def")];
        // Two independent streams from the same chunks must both yield everything
        let a = replayable_stream(chunks.clone()).collect().await.unwrap().into_bytes();
        let b = replayable_stream(chunks).collect().await.unwrap().into_bytes();
        assert_eq!(a, b);
        assert_eq!(&a[..], b"abcdef");
    }
}

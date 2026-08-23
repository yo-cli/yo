// In-memory random data pool. Generated ONCE at startup; every uploaded part
// is a refcounted `Bytes::slice` into it (O(1), zero-copy). Nothing is ever
// read from disk and no random data is generated inside the upload loop.

use bytes::Bytes;
use chrono::Utc;
use rand::rngs::SmallRng;
use rand::{RngCore, SeedableRng};
use uuid::Uuid;

use super::OBJECT_HEADER_LEN;

pub struct BufferPool {
    data: Bytes,
}

impl BufferPool {
    /// Fill `size` bytes of non-cryptographic random data in parallel with
    /// per-chunk SmallRng (cryptographic RNG would slow startup for no gain —
    /// the data only needs to be incompressible and dedup-hostile).
    pub fn generate(size: u64) -> Self {
        let size = size as usize;
        let mut data = vec![0u8; size];
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let chunk = size.div_ceil(threads).max(1);
        let seed = Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64;
        std::thread::scope(|s| {
            for (i, part) in data.chunks_mut(chunk).enumerate() {
                s.spawn(move || {
                    let mut rng = SmallRng::seed_from_u64(seed.wrapping_add(i as u64 * 0x9E37_79B9));
                    rng.fill_bytes(part);
                });
            }
        });
        Self {
            data: Bytes::from(data),
        }
    }

    pub fn len(&self) -> u64 {
        self.data.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Take `len` bytes starting at `offset`, wrapping around the pool end
    /// (ring read). Returns 1 slice, or 2 when the read crosses the boundary.
    /// All slices are refcount clones — no data is copied.
    /// Requires len ≤ pool size (guaranteed by pool ≥ 2 × part validation).
    pub fn ring_chunks(&self, offset: u64, len: u64) -> Vec<Bytes> {
        let n = self.data.len();
        let off = (offset % n as u64) as usize;
        let len = len as usize;
        debug_assert!(len <= n);
        if off + len <= n {
            vec![self.data.slice(off..off + len)]
        } else {
            let first = n - off;
            vec![self.data.slice(off..), self.data.slice(..len - first)]
        }
    }
}

/// Build the 64-byte unique header that starts every object:
/// magic + run_id + iteration + timestamp + a fresh UUID, zero-padded.
/// This is the only real data copy in the whole tool (64 bytes per object).
pub fn object_header(run_id: &Uuid, iteration: u64) -> Bytes {
    let mut h = Vec::with_capacity(OBJECT_HEADER_LEN as usize);
    h.extend_from_slice(b"YOS3");
    h.extend_from_slice(run_id.as_bytes());
    h.extend_from_slice(&iteration.to_be_bytes());
    h.extend_from_slice(&(Utc::now().timestamp_millis() as u64).to_be_bytes());
    h.extend_from_slice(Uuid::new_v4().as_bytes());
    h.resize(OBJECT_HEADER_LEN as usize, 0);
    Bytes::from(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_read_within_bounds_is_single_slice() {
        let pool = BufferPool::generate(1024);
        let chunks = pool.ring_chunks(100, 200);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 200);
    }

    #[test]
    fn ring_read_wraps_into_two_slices() {
        let pool = BufferPool::generate(1024);
        let chunks = pool.ring_chunks(1000, 100);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 24);
        assert_eq!(chunks[1].len(), 76);
        // wrapped content must equal a straight ring read done by hand
        let mut joined = Vec::new();
        for c in &chunks {
            joined.extend_from_slice(c);
        }
        let direct = pool.ring_chunks(1000 + 1024, 100); // same offset modulo pool
        assert_eq!(joined[..24], direct[0][..]);
    }

    #[test]
    fn header_is_64_bytes_and_unique() {
        let run = Uuid::new_v4();
        let a = object_header(&run, 1);
        let b = object_header(&run, 2);
        assert_eq!(a.len(), OBJECT_HEADER_LEN as usize);
        assert_eq!(b.len(), OBJECT_HEADER_LEN as usize);
        assert_ne!(a, b);
        assert_eq!(&a[..4], b"YOS3");
    }
}

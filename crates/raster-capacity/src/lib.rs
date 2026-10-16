//! Portable admission limits for exact raster preparation, separate from saved-source limits.
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

/// Letter at 1200 ppi including 1/8 inch bleed on every edge, in either orientation.
pub const MAX_PRODUCTION_RASTER_DIMENSION: u32 = 13_500;
pub const MAX_PRODUCTION_RASTER_PIXELS: usize = 10_500 * 13_500;
pub const MAX_RETAINED_RASTER_BYTES: usize = 1_536 * 1024 * 1024;
pub const MAX_PREPARED_RASTER_CACHE_BYTES: usize = 192 * 1024 * 1024;
pub const MAX_RASTER_DECODE_WORKER_BYTES: usize = 2 * 1024 * 1024 * 1024;
pub const MAX_RASTER_COLOR_WORKER_BYTES: usize = 1_152 * 1024 * 1024;
static WORK_ACTIVE: AtomicBool = AtomicBool::new(false);
static RETAINED_BYTES: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RasterCapacityExceeded;

/// A nonblocking process-wide owner. Private decode/color jobs never queue indefinitely.
pub struct RasterWorkPermit;
impl RasterWorkPermit {
    /// # Errors
    /// Refuses another active raster decode or color operation.
    pub fn acquire() -> Result<Self, RasterCapacityExceeded> {
        WORK_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| RasterCapacityExceeded)
    }
}
impl Drop for RasterWorkPermit {
    fn drop(&mut self) {
        WORK_ACTIVE.store(false, Ordering::Release);
    }
}

#[derive(Debug)]
struct Reservation {
    bytes: usize,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        RETAINED_BYTES.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
/// A retained-byte lease. Clones share one reservation until the last owner drops.
#[derive(Clone, Debug)]
pub struct RasterMemoryPermit(Arc<Reservation>);
impl RasterMemoryPermit {
    /// # Errors
    /// Refuses an excessive aggregate before allocating or starting private work.
    pub fn reserve(bytes: usize) -> Result<Self, RasterCapacityExceeded> {
        if bytes == 0 {
            return Err(RasterCapacityExceeded);
        }
        RETAINED_BYTES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|total| *total <= MAX_RETAINED_RASTER_BYTES)
            })
            .map_err(|_| RasterCapacityExceeded)?;
        Ok(Self(Arc::new(Reservation { bytes })))
    }
    /// Releases unused preflight capacity before sharing an accepted result.
    /// # Errors
    /// Refuses expansion or modification of an already shared lease.
    pub fn shrink_to(&mut self, bytes: usize) -> Result<(), RasterCapacityExceeded> {
        let reservation = Arc::get_mut(&mut self.0).ok_or(RasterCapacityExceeded)?;
        if bytes > reservation.bytes {
            return Err(RasterCapacityExceeded);
        }
        RETAINED_BYTES.fetch_sub(reservation.bytes - bytes, Ordering::AcqRel);
        reservation.bytes = bytes;
        Ok(())
    }
}

/// # Errors
/// Refuses invalid sides and pixel counts before allocations.
pub fn production_raster_pixels(width: u32, height: u32) -> Result<usize, RasterCapacityExceeded> {
    if width == 0
        || height == 0
        || width > MAX_PRODUCTION_RASTER_DIMENSION
        || height > MAX_PRODUCTION_RASTER_DIMENSION
    {
        return Err(RasterCapacityExceeded);
    }
    let pixels = usize::try_from(u64::from(width) * u64::from(height))
        .map_err(|_| RasterCapacityExceeded)?;
    if pixels > MAX_PRODUCTION_RASTER_PIXELS {
        return Err(RasterCapacityExceeded);
    }
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declared_letter_bleed_and_atomic_shared_ownership_are_bounded() {
        assert_eq!(production_raster_pixels(10_500, 13_500), Ok(141_750_000));
        assert_eq!(production_raster_pixels(13_500, 10_500), Ok(141_750_000));
        assert!(production_raster_pixels(10_501, 13_500).is_err());
        assert!(production_raster_pixels(13_501, 1).is_err());
        assert!(production_raster_pixels(0, 1).is_err());
        let work = RasterWorkPermit::acquire().unwrap();
        assert!(RasterWorkPermit::acquire().is_err());
        assert!(
            std::thread::spawn(|| RasterWorkPermit::acquire().is_err())
                .join()
                .unwrap()
        );
        drop(work);
        drop(RasterWorkPermit::acquire().unwrap());
        let mut lease = RasterMemoryPermit::reserve(MAX_RETAINED_RASTER_BYTES).unwrap();
        assert!(RasterMemoryPermit::reserve(1).is_err());
        lease.shrink_to(MAX_RETAINED_RASTER_BYTES - 1).unwrap();
        drop(RasterMemoryPermit::reserve(1).unwrap());
        let shared = lease.clone();
        assert!(lease.shrink_to(1).is_err());
        drop(lease);
        assert!(RasterMemoryPermit::reserve(2).is_err());
        drop(shared);
        drop(RasterMemoryPermit::reserve(MAX_RETAINED_RASTER_BYTES).unwrap());
    }
}

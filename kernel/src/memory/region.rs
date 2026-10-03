/// A half-open physical byte range. Empty and wrapping ranges are rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeError {
    InvalidRange,
    Overflow,
    Capacity,
}

impl Region {
    pub fn new(start: usize, end: usize) -> Result<Self, RangeError> {
        if start >= end {
            return Err(RangeError::InvalidRange);
        }
        Ok(Self { start, end })
    }

    pub fn from_size(start: usize, size: usize) -> Result<Self, RangeError> {
        Self::new(start, start.checked_add(size).ok_or(RangeError::Overflow)?)
    }

    pub const fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// Fixed-capacity storage: boot-time discovery must not depend on a heap.
#[derive(Debug)]
pub struct Regions<const N: usize> {
    entries: [Region; N],
    len: usize,
}

impl<const N: usize> Regions<N> {
    pub const fn new() -> Self {
        Self {
            entries: [Region { start: 0, end: 0 }; N],
            len: 0,
        }
    }

    pub fn push(&mut self, region: Region) -> Result<(), RangeError> {
        Region::new(region.start, region.end)?;
        let entry = self.entries.get_mut(self.len).ok_or(RangeError::Capacity)?;
        *entry = region;
        self.len += 1;
        Ok(())
    }

    pub fn as_slice(&self) -> &[Region] {
        &self.entries[..self.len]
    }
}

impl<const N: usize> Default for Regions<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_wrapping_and_capacity_overflow() {
        assert_eq!(Region::new(1, 1), Err(RangeError::InvalidRange));
        assert_eq!(Region::from_size(usize::MAX, 2), Err(RangeError::Overflow));
        let mut regions = Regions::<1>::new();
        regions.push(Region::new(0, 4096).unwrap()).unwrap();
        assert_eq!(
            regions.push(Region::new(4096, 8192).unwrap()),
            Err(RangeError::Capacity)
        );
        assert_eq!(regions.as_slice().len(), 1);
    }
}

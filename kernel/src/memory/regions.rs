//! Allocation-free physical byte ranges. Every end address is exclusive.

pub const MAX_REGIONS: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Region {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionError {
    Empty,
    Overflow,
    Capacity,
}

impl Region {
    pub fn new(start: usize, size: usize) -> Result<Self, RegionError> {
        if size == 0 {
            return Err(RegionError::Empty);
        }
        let end = start.checked_add(size).ok_or(RegionError::Overflow)?;
        Ok(Self { start, end })
    }
}

/// Sorted, disjoint ranges; touching ranges are merged as well.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegionSet {
    regions: [Region; MAX_REGIONS],
    len: usize,
}

impl RegionSet {
    pub const fn new() -> Self {
        Self {
            regions: [Region { start: 0, end: 0 }; MAX_REGIONS],
            len: 0,
        }
    }

    /// A capacity error leaves the set unchanged.
    pub fn insert(&mut self, mut region: Region) -> Result<(), RegionError> {
        if region.start >= region.end {
            return Err(RegionError::Empty);
        }

        let mut first = 0;
        while first < self.len && self.regions[first].end < region.start {
            first += 1;
        }

        let mut last = first;
        while last < self.len && self.regions[last].start <= region.end {
            region.start = region.start.min(self.regions[last].start);
            region.end = region.end.max(self.regions[last].end);
            last += 1;
        }

        if first == last {
            if self.len == MAX_REGIONS {
                return Err(RegionError::Capacity);
            }
            self.regions.copy_within(first..self.len, first + 1);
            self.len += 1;
        } else {
            self.regions.copy_within(last..self.len, first + 1);
            self.len -= last - first - 1;
        }
        self.regions[first] = region;
        Ok(())
    }

    pub fn as_slice(&self) -> &[Region] {
        &self.regions[..self.len]
    }
}

impl Default for RegionSet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_merge_and_bridge_adjacent_ranges() {
        let mut regions = RegionSet::new();
        regions
            .insert(Region::new(0x5000, 0x1000).unwrap())
            .unwrap();
        regions
            .insert(Region::new(0x1000, 0x2000).unwrap())
            .unwrap();
        regions
            .insert(Region::new(0x2800, 0x3000).unwrap())
            .unwrap();
        assert_eq!(
            regions.as_slice(),
            &[Region {
                start: 0x1000,
                end: 0x6000
            }]
        );
    }

    #[test]
    fn full_set_can_merge_but_cannot_insert_disjoint_range() {
        let mut regions = RegionSet::new();
        for index in 0..MAX_REGIONS {
            regions.insert(Region::new(index * 4, 2).unwrap()).unwrap();
        }
        let original = regions.clone();
        assert_eq!(
            regions.insert(Region::new(MAX_REGIONS * 4, 2).unwrap()),
            Err(RegionError::Capacity)
        );
        assert_eq!(regions, original);
        regions
            .insert(Region::new(1, MAX_REGIONS * 4).unwrap())
            .unwrap();
        assert_eq!(regions.as_slice().len(), 1);
    }

    #[test]
    fn reject_empty_and_wrapping_ranges() {
        assert_eq!(Region::new(3, 0), Err(RegionError::Empty));
        assert_eq!(Region::new(usize::MAX, 1), Err(RegionError::Overflow));
        let mut regions = RegionSet::new();
        assert_eq!(
            regions.insert(Region { start: 8, end: 7 }),
            Err(RegionError::Empty)
        );
        assert!(regions.as_slice().is_empty());
    }
}

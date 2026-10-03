//! Strict USTAR subset. Metadata is validated before installing any inode.

use super::{FileSystem, FsError, INIT, MAX_PATH};

const BLOCK: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TarError {
    Truncated,
    InvalidHeader,
    InvalidChecksum,
    UnsupportedEntry,
    InvalidPath,
    Filesystem(FsError),
}

fn octal(field: &[u8]) -> Result<usize, TarError> {
    let mut value = 0usize;
    let mut digits = false;
    let mut ended = false;
    for &byte in field {
        match byte {
            b'0'..=b'7' if !ended => {
                value = value
                    .checked_mul(8)
                    .and_then(|v| v.checked_add((byte - b'0') as usize))
                    .ok_or(TarError::InvalidHeader)?;
                digits = true;
            }
            b' ' if !digits && !ended => {}
            0 | b' ' => ended = true,
            _ => return Err(TarError::InvalidHeader),
        }
    }
    if digits {
        Ok(value)
    } else {
        Err(TarError::InvalidHeader)
    }
}

fn field_bytes(field: &[u8]) -> Result<&[u8], TarError> {
    let end = field
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(field.len());
    if field[end..].iter().any(|&byte| byte != 0) {
        return Err(TarError::InvalidHeader);
    }
    Ok(&field[..end])
}

fn walk(
    archive: &'static [u8],
    mut visit: impl FnMut(&[u8], Option<&'static [u8]>) -> Result<(), TarError>,
) -> Result<(), TarError> {
    if archive.len() % BLOCK != 0 {
        return Err(TarError::Truncated);
    }
    let mut cursor = 0usize;
    loop {
        let header = archive
            .get(cursor..cursor + BLOCK)
            .ok_or(TarError::Truncated)?;
        if header.iter().all(|&byte| byte == 0) {
            let trailer = archive.get(cursor + BLOCK..).ok_or(TarError::Truncated)?;
            if trailer.len() < BLOCK || trailer.iter().any(|&byte| byte != 0) {
                return Err(TarError::Truncated);
            }
            return Ok(());
        }
        if &header[257..263] != b"ustar\0" || &header[263..265] != b"00" {
            return Err(TarError::InvalidHeader);
        }
        let expected = octal(&header[148..156])?;
        let checksum = header
            .iter()
            .enumerate()
            .map(|(i, &byte)| {
                if (148..156).contains(&i) {
                    b' ' as usize
                } else {
                    byte as usize
                }
            })
            .sum::<usize>();
        if checksum != expected {
            return Err(TarError::InvalidChecksum);
        }
        let size = octal(&header[124..136])?;
        let directory = match header[156] {
            0 | b'0' => false,
            b'5' if size == 0 => true,
            b'5' => return Err(TarError::InvalidHeader),
            _ => return Err(TarError::UnsupportedEntry),
        };
        // This subset has no links, sparse extension or alternate encodings.
        if field_bytes(&header[157..257])?.len() != 0 {
            return Err(TarError::UnsupportedEntry);
        }
        let name = field_bytes(&header[..100])?;
        let prefix = field_bytes(&header[345..500])?;
        let path_length = name.len() + prefix.len() + usize::from(!prefix.is_empty());
        if path_length == 0 || path_length > MAX_PATH {
            return Err(TarError::InvalidPath);
        }
        let mut path = [0u8; MAX_PATH];
        let mut written = 0;
        if !prefix.is_empty() {
            path[..prefix.len()].copy_from_slice(prefix);
            written = prefix.len();
            path[written] = b'/';
            written += 1;
        }
        path[written..written + name.len()].copy_from_slice(name);
        let normalized = if directory {
            path[..path_length]
                .strip_suffix(b"/")
                .unwrap_or(&path[..path_length])
        } else {
            &path[..path_length]
        };
        if normalized.is_empty()
            || normalized[0] == b'/'
            || normalized
                .split(|&byte| byte == b'/')
                .any(|part| part.is_empty() || part == b"." || part == b"..")
        {
            return Err(TarError::InvalidPath);
        }
        let payload = cursor.checked_add(BLOCK).ok_or(TarError::Truncated)?;
        let end = payload.checked_add(size).ok_or(TarError::Truncated)?;
        let bytes = archive.get(payload..end).ok_or(TarError::Truncated)?;
        let padded = size
            .checked_add(BLOCK - 1)
            .map(|v| v / BLOCK * BLOCK)
            .ok_or(TarError::Truncated)?;
        cursor = payload.checked_add(padded).ok_or(TarError::Truncated)?;
        if cursor > archive.len() {
            return Err(TarError::Truncated);
        }
        visit(normalized, if directory { None } else { Some(bytes) })?;
    }
}

impl FileSystem {
    /// Mount the supported USTAR subset at /init before process creation.
    /// Requires an empty /init; every malformed/capacity failure leaves it empty.
    /// Immutable payload slices retain the archive's static boot ownership.
    pub fn mount_tar(&mut self, archive: &'static [u8]) -> Result<(), TarError> {
        self.ready().map_err(TarError::Filesystem)?;
        if self.processes != 0
            || self
                .nodes
                .iter()
                .any(|node| node.linked && node.parent == INIT)
        {
            return Err(TarError::Filesystem(FsError::Busy));
        }
        walk(archive, |_, _| Ok(()))?;
        let result = walk(archive, |path, bytes| {
            self.add_init_entry(path, bytes)
                .map_err(TarError::Filesystem)
        });
        if result.is_err() {
            // All read-only nodes except the mount itself were created by this
            // transaction, and no process can hold a reference yet.
            for index in 0..self.nodes.len() {
                if index != INIT as usize && self.nodes[index].read_only {
                    self.nodes[index].linked = false;
                    self.collect_node(index as u8);
                }
            }
        }
        result
    }
}

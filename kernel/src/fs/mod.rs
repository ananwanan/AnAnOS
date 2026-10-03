//! Bounded CPU0 VFS, read-only boot files, RAM files and process descriptors.
//!
//! All storage is owned by a single `FileSystem`, normally kept in static
//! kernel storage. Callers serialize access (including IRQ entry) externally;
//! no allocator, atomics, active page-table access or user pointer is involved.
//! File bytes and process buffers passed here are already privileged slices.

mod tar;
#[cfg(test)]
mod tests;

pub use tar::TarError;

pub const MAX_NODES: usize = 64;
pub const MAX_OPEN_FILES: usize = 32;
pub const MAX_FDS: usize = 16;
pub const MAX_NAME: usize = 48;
pub const MAX_PATH: usize = 256;
pub const MAX_RAM_FILES: usize = 16;
pub const MAX_FILE_BYTES: usize = 4096;
const NONE: u8 = u8::MAX;
const ROOT: u8 = 0;
const INIT: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsError {
    NotFound,
    BadDescriptor,
    InvalidArgument,
    NameTooLong,
    NotDirectory,
    IsDirectory,
    AlreadyExists,
    ReadOnly,
    NoSpace,
    TooManyFiles,
    Busy,
    NotEmpty,
    Overflow,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorKind {
    Stdin,
    Stdout,
    Stderr,
    File,
    Directory,
}

/// Provisional ABI bits, intentionally independent of hosted libc constants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenFlags(u32);

impl OpenFlags {
    pub const READ: Self = Self(1);
    pub const WRITE: Self = Self(2);
    pub const CREATE: Self = Self(4);
    pub const EXCLUSIVE: Self = Self(8);
    pub const TRUNCATE: Self = Self(16);
    pub const APPEND: Self = Self(32);
    pub const DIRECTORY: Self = Self(64);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub fn from_bits(bits: u32) -> Result<Self, FsError> {
        let flags = Self(bits);
        if bits & !127 != 0
            || bits & 3 == 0
            || flags.contains(Self::EXCLUSIVE) && !flags.contains(Self::CREATE)
            || (flags.contains(Self::TRUNCATE) || flags.contains(Self::APPEND))
                && !flags.contains(Self::WRITE)
        {
            return Err(FsError::InvalidArgument);
        }
        Ok(flags)
    }

    const fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekWhence {
    Start,
    Current,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub inode: u64,
    pub size: u64,
    pub kind: DescriptorKind,
    pub read_only: bool,
    pub linked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub inode: u64,
    pub kind: DescriptorKind,
    pub name_length: usize,
    pub name: [u8; MAX_NAME],
}

impl DirEntry {
    pub const EMPTY: Self = Self {
        inode: 0,
        kind: DescriptorKind::File,
        name_length: 0,
        name: [0; MAX_NAME],
    };
}

#[derive(Clone, Copy)]
enum NodeKind {
    Empty,
    Directory,
    RamFile(u8),
    BootFile(&'static [u8]),
}

#[derive(Clone, Copy)]
struct Node {
    kind: NodeKind,
    parent: u8,
    name: [u8; MAX_NAME],
    name_length: u8,
    read_only: bool,
    linked: bool,
    // Open descriptions and process working directories retain an inode.
    references: u16,
    generation: u32,
}

impl Node {
    const EMPTY: Self = Self {
        kind: NodeKind::Empty,
        parent: NONE,
        name: [0; MAX_NAME],
        name_length: 0,
        read_only: false,
        linked: false,
        references: 0,
        generation: 0,
    };

    fn name(&self) -> &[u8] {
        &self.name[..self.name_length as usize]
    }

    const fn is_directory(&self) -> bool {
        matches!(self.kind, NodeKind::Directory)
    }

    const fn descriptor_kind(&self) -> DescriptorKind {
        if self.is_directory() {
            DescriptorKind::Directory
        } else {
            DescriptorKind::File
        }
    }

    fn inode(&self, index: u8) -> u64 {
        ((self.generation as u64) << 8) | index as u64
    }
}

#[derive(Clone, Copy)]
enum OpenTarget {
    Free,
    Stdin,
    Stdout,
    Stderr,
    Node(u8),
}

#[derive(Clone, Copy)]
struct OpenFile {
    target: OpenTarget,
    offset: usize,
    flags: OpenFlags,
    references: u16,
}

impl OpenFile {
    const EMPTY: Self = Self {
        target: OpenTarget::Free,
        offset: 0,
        flags: OpenFlags(0),
        references: 0,
    };
}

/// Descriptor numbers are private to this process. Duplicated descriptors
/// reference one shared open description, including its offset and flags.
pub struct ProcessFs {
    descriptors: [u8; MAX_FDS],
    cwd: u8,
    attached: bool,
}

impl ProcessFs {
    pub const fn new() -> Self {
        Self {
            descriptors: [NONE; MAX_FDS],
            cwd: ROOT,
            attached: false,
        }
    }

    pub const fn is_attached(&self) -> bool {
        self.attached
    }
}

impl Default for ProcessFs {
    fn default() -> Self {
        Self::new()
    }
}

pub struct FileSystem {
    nodes: [Node; MAX_NODES],
    opens: [OpenFile; MAX_OPEN_FILES],
    ram: [[u8; MAX_FILE_BYTES]; MAX_RAM_FILES],
    ram_lengths: [usize; MAX_RAM_FILES],
    ram_used: [bool; MAX_RAM_FILES],
    initialized: bool,
    processes: u16,
}

impl FileSystem {
    /// Keep this object in static storage on bare metal; its RAM backing pool
    /// deliberately exceeds the size of a typical exception stack.
    pub const fn new() -> Self {
        Self {
            nodes: [Node::EMPTY; MAX_NODES],
            opens: [OpenFile::EMPTY; MAX_OPEN_FILES],
            ram: [[0; MAX_FILE_BYTES]; MAX_RAM_FILES],
            ram_lengths: [0; MAX_RAM_FILES],
            ram_used: [false; MAX_RAM_FILES],
            initialized: false,
            processes: 0,
        }
    }

    pub fn initialize(&mut self) -> Result<(), FsError> {
        if self.initialized {
            return Err(FsError::Busy);
        }
        self.nodes[ROOT as usize] = Self::node(ROOT, b"", NodeKind::Directory, false, 1);
        self.nodes[INIT as usize] = Self::node(ROOT, b"init", NodeKind::Directory, true, 1);
        self.nodes[2] = Self::node(ROOT, b"tmp", NodeKind::Directory, false, 1);
        self.initialized = true;
        Ok(())
    }

    fn node(parent: u8, name: &[u8], kind: NodeKind, read_only: bool, generation: u32) -> Node {
        let mut node = Node {
            kind,
            parent,
            read_only,
            linked: true,
            generation,
            ..Node::EMPTY
        };
        node.name[..name.len()].copy_from_slice(name);
        node.name_length = name.len() as u8;
        node
    }

    fn ready(&self) -> Result<(), FsError> {
        if self.initialized {
            Ok(())
        } else {
            Err(FsError::InvalidArgument)
        }
    }

    fn process(&self, process: &ProcessFs) -> Result<(), FsError> {
        self.ready()?;
        if process.attached {
            Ok(())
        } else {
            Err(FsError::InvalidArgument)
        }
    }

    pub fn attach_process(&mut self, process: &mut ProcessFs) -> Result<(), FsError> {
        self.ready()?;
        if process.attached {
            return Err(FsError::Busy);
        }
        if self.processes == u16::MAX || self.nodes[ROOT as usize].references == u16::MAX {
            return Err(FsError::TooManyFiles);
        }
        let mut free = [NONE; 3];
        let mut count = 0;
        for (index, open) in self.opens.iter().enumerate() {
            if matches!(open.target, OpenTarget::Free) {
                free[count] = index as u8;
                count += 1;
                if count == 3 {
                    break;
                }
            }
        }
        if count != 3 {
            return Err(FsError::TooManyFiles);
        }
        for (fd, (target, flags)) in [
            (OpenTarget::Stdin, OpenFlags::READ),
            (OpenTarget::Stdout, OpenFlags::WRITE),
            (OpenTarget::Stderr, OpenFlags::WRITE),
        ]
        .into_iter()
        .enumerate()
        {
            self.opens[free[fd] as usize] = OpenFile {
                target,
                offset: 0,
                flags,
                references: 1,
            };
            process.descriptors[fd] = free[fd];
        }
        process.cwd = ROOT;
        process.attached = true;
        self.nodes[ROOT as usize].references += 1;
        self.processes += 1;
        Ok(())
    }

    /// Spawn-style descriptor inheritance; no parent process memory is copied.
    /// Capacity/refcount validation precedes every mutation.
    pub fn inherit_process(
        &mut self,
        parent: &ProcessFs,
        child: &mut ProcessFs,
    ) -> Result<(), FsError> {
        self.process(parent)?;
        if child.attached {
            return Err(FsError::Busy);
        }
        if self.processes == u16::MAX || self.nodes[parent.cwd as usize].references == u16::MAX {
            return Err(FsError::TooManyFiles);
        }
        for (index, open) in self.opens.iter().enumerate() {
            let additions = parent
                .descriptors
                .iter()
                .filter(|&&slot| slot as usize == index)
                .count();
            if (open.references as usize) + additions > u16::MAX as usize {
                return Err(FsError::TooManyFiles);
            }
        }
        for &slot in &parent.descriptors {
            if slot != NONE {
                self.opens[slot as usize].references += 1;
            }
        }
        child.descriptors = parent.descriptors;
        child.cwd = parent.cwd;
        child.attached = true;
        self.nodes[child.cwd as usize].references += 1;
        self.processes += 1;
        Ok(())
    }

    /// Idempotent exit cleanup, including retained unlinked file storage.
    pub fn cleanup_process(&mut self, process: &mut ProcessFs) {
        if !process.attached {
            return;
        }
        for fd in 0..MAX_FDS {
            if process.descriptors[fd] != NONE {
                let _ = self.close(process, fd);
            }
        }
        self.release_node(process.cwd);
        process.cwd = ROOT;
        process.attached = false;
        self.processes -= 1;
    }

    /// Boot acceptance checks both descriptor and cwd inode references after
    /// the final process exits; linked filesystem content itself remains live.
    pub fn has_process_resources(&self) -> bool {
        self.processes != 0
            || self
                .opens
                .iter()
                .any(|open| !matches!(open.target, OpenTarget::Free))
            || self.nodes.iter().any(|node| node.references != 0)
    }

    fn validate_path(path: &[u8]) -> Result<(), FsError> {
        if path.len() > MAX_PATH {
            return Err(FsError::NameTooLong);
        }
        if path.is_empty() {
            return Err(FsError::NotFound);
        }
        if path.contains(&0) {
            return Err(FsError::InvalidArgument);
        }
        Ok(())
    }

    fn child(&self, parent: u8, name: &[u8]) -> Option<u8> {
        self.nodes.iter().enumerate().find_map(|(index, node)| {
            (node.linked && node.parent == parent && node.name() == name).then_some(index as u8)
        })
    }

    fn resolve(&self, cwd: u8, path: &[u8]) -> Result<u8, FsError> {
        Self::validate_path(path)?;
        let mut current = if path[0] == b'/' { ROOT } else { cwd };
        for name in path
            .split(|&byte| byte == b'/')
            .filter(|name| !name.is_empty())
        {
            if !self.nodes[current as usize].is_directory() {
                return Err(FsError::NotDirectory);
            }
            if name.len() > MAX_NAME {
                return Err(FsError::NameTooLong);
            }
            current = match name {
                b"." => current,
                b".." => self.nodes[current as usize].parent,
                _ => self.child(current, name).ok_or(FsError::NotFound)?,
            };
        }
        if path.last() == Some(&b'/') && !self.nodes[current as usize].is_directory() {
            return Err(FsError::NotDirectory);
        }
        Ok(current)
    }

    fn parent<'p>(&self, cwd: u8, path: &'p [u8]) -> Result<(u8, &'p [u8]), FsError> {
        Self::validate_path(path)?;
        let end = path
            .iter()
            .rposition(|&byte| byte != b'/')
            .ok_or(FsError::Busy)?
            + 1;
        let start = path[..end]
            .iter()
            .rposition(|&byte| byte == b'/')
            .map_or(0, |i| i + 1);
        let name = &path[start..end];
        if name == b"." || name == b".." {
            return Err(FsError::InvalidArgument);
        }
        if name.len() > MAX_NAME {
            return Err(FsError::NameTooLong);
        }
        let parent = if start == 0 {
            cwd
        } else {
            self.resolve(cwd, &path[..start])?
        };
        if !self.nodes[parent as usize].is_directory() {
            return Err(FsError::NotDirectory);
        }
        Ok((parent, name))
    }

    fn free_node(&self) -> Result<u8, FsError> {
        self.nodes
            .iter()
            .position(|node| matches!(node.kind, NodeKind::Empty))
            .map(|index| index as u8)
            .ok_or(FsError::NoSpace)
    }

    fn insert_node(&mut self, index: u8, parent: u8, name: &[u8], kind: NodeKind, read_only: bool) {
        let generation = self.nodes[index as usize].generation.wrapping_add(1).max(1);
        self.nodes[index as usize] = Self::node(parent, name, kind, read_only, generation);
    }

    fn release_node(&mut self, index: u8) {
        self.nodes[index as usize].references -= 1;
        self.collect_node(index);
    }

    fn collect_node(&mut self, index: u8) {
        let node = &mut self.nodes[index as usize];
        if !node.linked && node.references == 0 {
            if let NodeKind::RamFile(slot) = node.kind {
                self.ram_used[slot as usize] = false;
                self.ram_lengths[slot as usize] = 0;
                // Reused files cannot observe bytes from the previous owner.
                self.ram[slot as usize].fill(0);
            }
            let generation = node.generation;
            *node = Node {
                generation,
                ..Node::EMPTY
            };
        }
    }

    fn description(&self, process: &ProcessFs, fd: usize) -> Result<usize, FsError> {
        self.process(process)?;
        let slot = *process.descriptors.get(fd).ok_or(FsError::BadDescriptor)?;
        if slot == NONE {
            Err(FsError::BadDescriptor)
        } else {
            Ok(slot as usize)
        }
    }

    pub fn descriptor_kind(
        &self,
        process: &ProcessFs,
        fd: usize,
    ) -> Result<DescriptorKind, FsError> {
        let slot = self.description(process, fd)?;
        Ok(match self.opens[slot].target {
            OpenTarget::Stdin => DescriptorKind::Stdin,
            OpenTarget::Stdout => DescriptorKind::Stdout,
            OpenTarget::Stderr => DescriptorKind::Stderr,
            OpenTarget::Node(node) => self.nodes[node as usize].descriptor_kind(),
            OpenTarget::Free => return Err(FsError::BadDescriptor),
        })
    }

    /// Permission/type preflight for adapters that validate user memory before
    /// performing I/O. Empty transfers still validate the descriptor.
    pub fn validate_read(&self, process: &ProcessFs, fd: usize) -> Result<DescriptorKind, FsError> {
        let slot = self.description(process, fd)?;
        if !self.opens[slot].flags.contains(OpenFlags::READ) {
            return Err(FsError::BadDescriptor);
        }
        let kind = self.descriptor_kind(process, fd)?;
        if kind == DescriptorKind::Directory {
            return Err(FsError::IsDirectory);
        }
        Ok(kind)
    }

    pub fn validate_write(
        &self,
        process: &ProcessFs,
        fd: usize,
    ) -> Result<DescriptorKind, FsError> {
        let slot = self.description(process, fd)?;
        if !self.opens[slot].flags.contains(OpenFlags::WRITE) {
            return Err(FsError::BadDescriptor);
        }
        let kind = self.descriptor_kind(process, fd)?;
        if kind == DescriptorKind::Directory {
            return Err(FsError::IsDirectory);
        }
        Ok(kind)
    }

    pub fn open(
        &mut self,
        process: &mut ProcessFs,
        path: &[u8],
        flags: OpenFlags,
    ) -> Result<usize, FsError> {
        self.process(process)?;
        OpenFlags::from_bits(flags.bits())?;
        let fd = process
            .descriptors
            .iter()
            .position(|&slot| slot == NONE)
            .ok_or(FsError::TooManyFiles)?;
        let slot = self
            .opens
            .iter()
            .position(|open| matches!(open.target, OpenTarget::Free))
            .ok_or(FsError::TooManyFiles)?;
        let node = match self.resolve(process.cwd, path) {
            Ok(node) => {
                if flags.contains(OpenFlags::CREATE) && flags.contains(OpenFlags::EXCLUSIVE) {
                    return Err(FsError::AlreadyExists);
                }
                node
            }
            Err(FsError::NotFound) if flags.contains(OpenFlags::CREATE) => {
                if flags.contains(OpenFlags::DIRECTORY) {
                    return Err(FsError::NotFound);
                }
                if path.last() == Some(&b'/') {
                    return Err(FsError::NotDirectory);
                }
                let (parent, name) = self.parent(process.cwd, path)?;
                if self.nodes[parent as usize].read_only {
                    return Err(FsError::ReadOnly);
                }
                // Reserve every bounded resource before making a visible file.
                let node = self.free_node()?;
                let ram = self
                    .ram_used
                    .iter()
                    .position(|&used| !used)
                    .ok_or(FsError::NoSpace)?;
                self.ram_used[ram] = true;
                self.ram_lengths[ram] = 0;
                self.insert_node(node, parent, name, NodeKind::RamFile(ram as u8), false);
                node
            }
            Err(error) => return Err(error),
        };
        let inode = &self.nodes[node as usize];
        if inode.is_directory() && flags.contains(OpenFlags::WRITE) {
            return Err(FsError::IsDirectory);
        }
        if !inode.is_directory() && flags.contains(OpenFlags::DIRECTORY) {
            return Err(FsError::NotDirectory);
        }
        if inode.read_only && flags.contains(OpenFlags::WRITE) {
            return Err(FsError::ReadOnly);
        }
        if flags.contains(OpenFlags::TRUNCATE) {
            if let NodeKind::RamFile(ram) = inode.kind {
                self.ram_lengths[ram as usize] = 0;
                self.ram[ram as usize].fill(0);
            }
        }
        self.nodes[node as usize].references += 1;
        self.opens[slot] = OpenFile {
            target: OpenTarget::Node(node),
            offset: 0,
            flags,
            references: 1,
        };
        process.descriptors[fd] = slot as u8;
        Ok(fd)
    }

    pub fn close(&mut self, process: &mut ProcessFs, fd: usize) -> Result<(), FsError> {
        let slot = self.description(process, fd)?;
        process.descriptors[fd] = NONE;
        self.opens[slot].references -= 1;
        if self.opens[slot].references == 0 {
            if let OpenTarget::Node(node) = self.opens[slot].target {
                self.release_node(node);
            }
            self.opens[slot] = OpenFile::EMPTY;
        }
        Ok(())
    }

    pub fn dup(&mut self, process: &mut ProcessFs, fd: usize) -> Result<usize, FsError> {
        let slot = self.description(process, fd)?;
        let new_fd = process
            .descriptors
            .iter()
            .position(|&slot| slot == NONE)
            .ok_or(FsError::TooManyFiles)?;
        self.dup2(process, fd, new_fd)?;
        debug_assert_eq!(process.descriptors[new_fd] as usize, slot);
        Ok(new_fd)
    }

    pub fn dup2(
        &mut self,
        process: &mut ProcessFs,
        fd: usize,
        new_fd: usize,
    ) -> Result<usize, FsError> {
        let slot = self.description(process, fd)?;
        if new_fd >= MAX_FDS {
            return Err(FsError::BadDescriptor);
        }
        if fd == new_fd {
            return Ok(new_fd);
        }
        if self.opens[slot].references == u16::MAX {
            return Err(FsError::TooManyFiles);
        }
        // Increment before closing the target, which may be another duplicate.
        self.opens[slot].references += 1;
        if process.descriptors[new_fd] != NONE {
            self.close(process, new_fd)?;
        }
        process.descriptors[new_fd] = slot as u8;
        Ok(new_fd)
    }

    pub fn read(
        &mut self,
        process: &mut ProcessFs,
        fd: usize,
        output: &mut [u8],
    ) -> Result<usize, FsError> {
        let slot = self.description(process, fd)?;
        let open = self.opens[slot];
        if !open.flags.contains(OpenFlags::READ) {
            return Err(FsError::BadDescriptor);
        }
        let OpenTarget::Node(node) = open.target else {
            return Err(FsError::Unsupported);
        };
        let bytes = match self.nodes[node as usize].kind {
            NodeKind::RamFile(ram) => &self.ram[ram as usize][..self.ram_lengths[ram as usize]],
            NodeKind::BootFile(bytes) => bytes,
            NodeKind::Directory => return Err(FsError::IsDirectory),
            NodeKind::Empty => return Err(FsError::BadDescriptor),
        };
        let start = open.offset.min(bytes.len());
        let count = output.len().min(bytes.len() - start);
        output[..count].copy_from_slice(&bytes[start..start + count]);
        self.opens[slot].offset += count;
        Ok(count)
    }

    pub fn write(
        &mut self,
        process: &mut ProcessFs,
        fd: usize,
        bytes: &[u8],
    ) -> Result<usize, FsError> {
        let slot = self.description(process, fd)?;
        let open = self.opens[slot];
        if !open.flags.contains(OpenFlags::WRITE) {
            return Err(FsError::BadDescriptor);
        }
        let OpenTarget::Node(node) = open.target else {
            return Err(FsError::Unsupported);
        };
        let ram = match self.nodes[node as usize].kind {
            NodeKind::RamFile(ram) => ram as usize,
            NodeKind::Directory => return Err(FsError::IsDirectory),
            NodeKind::BootFile(_) => return Err(FsError::ReadOnly),
            NodeKind::Empty => return Err(FsError::BadDescriptor),
        };
        if bytes.is_empty() {
            return Ok(0);
        }
        let start = if open.flags.contains(OpenFlags::APPEND) {
            self.ram_lengths[ram]
        } else {
            open.offset
        };
        if start >= MAX_FILE_BYTES {
            return Err(FsError::NoSpace);
        }
        let count = bytes.len().min(MAX_FILE_BYTES - start);
        // Seeking beyond EOF creates a zero-filled hole, including after truncation.
        if start > self.ram_lengths[ram] {
            self.ram[ram][self.ram_lengths[ram]..start].fill(0);
        }
        self.ram[ram][start..start + count].copy_from_slice(&bytes[..count]);
        self.ram_lengths[ram] = self.ram_lengths[ram].max(start + count);
        self.opens[slot].offset = start + count;
        Ok(count)
    }

    pub fn seek(
        &mut self,
        process: &mut ProcessFs,
        fd: usize,
        offset: i64,
        whence: SeekWhence,
    ) -> Result<u64, FsError> {
        let slot = self.description(process, fd)?;
        let OpenTarget::Node(node) = self.opens[slot].target else {
            return Err(FsError::Unsupported);
        };
        if self.nodes[node as usize].is_directory() {
            // Directory cookies are internal node indices. Only rewind is exposed.
            if offset == 0 && whence == SeekWhence::Start {
                self.opens[slot].offset = 0;
                return Ok(0);
            }
            return Err(FsError::InvalidArgument);
        }
        let base = match whence {
            SeekWhence::Start => 0,
            SeekWhence::Current => self.opens[slot].offset as u64,
            SeekWhence::End => self.node_stat(node).size,
        };
        let result = if offset >= 0 {
            base.checked_add(offset as u64)
        } else {
            base.checked_sub(offset.unsigned_abs())
        }
        .filter(|&value| value <= i64::MAX as u64 && value <= usize::MAX as u64)
        .ok_or(FsError::InvalidArgument)?;
        self.opens[slot].offset = result as usize;
        Ok(result)
    }

    fn node_stat(&self, node: u8) -> Stat {
        let inode = &self.nodes[node as usize];
        let size = match inode.kind {
            NodeKind::RamFile(ram) => self.ram_lengths[ram as usize] as u64,
            NodeKind::BootFile(bytes) => bytes.len() as u64,
            _ => 0,
        };
        Stat {
            inode: inode.inode(node),
            size,
            kind: inode.descriptor_kind(),
            read_only: inode.read_only,
            linked: inode.linked,
        }
    }

    pub fn stat(&self, process: &ProcessFs, path: &[u8]) -> Result<Stat, FsError> {
        self.process(process)?;
        Ok(self.node_stat(self.resolve(process.cwd, path)?))
    }

    pub fn fstat(&self, process: &ProcessFs, fd: usize) -> Result<Stat, FsError> {
        let slot = self.description(process, fd)?;
        if let OpenTarget::Node(node) = self.opens[slot].target {
            Ok(self.node_stat(node))
        } else {
            Ok(Stat {
                inode: 0,
                size: 0,
                kind: self.descriptor_kind(process, fd)?,
                read_only: false,
                linked: true,
            })
        }
    }

    pub fn getdents(
        &mut self,
        process: &mut ProcessFs,
        fd: usize,
        output: &mut [DirEntry],
    ) -> Result<usize, FsError> {
        let slot = self.description(process, fd)?;
        let OpenTarget::Node(directory) = self.opens[slot].target else {
            return Err(FsError::NotDirectory);
        };
        if !self.nodes[directory as usize].is_directory() {
            return Err(FsError::NotDirectory);
        }
        let mut count = 0;
        let mut cookie = self.opens[slot].offset;
        while count < output.len() && cookie < MAX_NODES + 2 {
            let candidate = match cookie {
                0 => Some((directory, b".".as_slice())),
                1 => Some((self.nodes[directory as usize].parent, b"..".as_slice())),
                value => {
                    let node = &self.nodes[value - 2];
                    (node.linked && node.parent == directory && value - 2 != ROOT as usize)
                        .then_some(((value - 2) as u8, node.name()))
                }
            };
            cookie += 1;
            if let Some((index, name)) = candidate {
                let node = &self.nodes[index as usize];
                let mut entry = DirEntry {
                    inode: node.inode(index),
                    kind: node.descriptor_kind(),
                    ..DirEntry::EMPTY
                };
                entry.name[..name.len()].copy_from_slice(name);
                entry.name_length = name.len();
                output[count] = entry;
                count += 1;
            }
        }
        self.opens[slot].offset = cookie;
        Ok(count)
    }

    pub fn mkdir(&mut self, process: &mut ProcessFs, path: &[u8]) -> Result<(), FsError> {
        self.process(process)?;
        let (parent, name) = self.parent(process.cwd, path)?;
        if self.nodes[parent as usize].read_only {
            return Err(FsError::ReadOnly);
        }
        if self.child(parent, name).is_some() {
            return Err(FsError::AlreadyExists);
        }
        let index = self.free_node()?;
        self.insert_node(index, parent, name, NodeKind::Directory, false);
        Ok(())
    }

    fn removable(&self, node: u8) -> Result<(), FsError> {
        if node == ROOT || node == INIT || node == 2 {
            return Err(FsError::Busy);
        }
        let inode = &self.nodes[node as usize];
        if inode.read_only || self.nodes[inode.parent as usize].read_only {
            return Err(FsError::ReadOnly);
        }
        if inode.is_directory() {
            if self
                .nodes
                .iter()
                .any(|child| child.linked && child.parent == node)
            {
                return Err(FsError::NotEmpty);
            }
            if inode.references != 0 {
                return Err(FsError::Busy);
            }
        }
        Ok(())
    }

    /// This bootstrap operation removes regular files and empty directories;
    /// open regular files continue to exist until their final descriptor closes.
    pub fn unlink(&mut self, process: &mut ProcessFs, path: &[u8]) -> Result<(), FsError> {
        self.process(process)?;
        let (parent, name) = self.parent(process.cwd, path)?;
        let node = self.child(parent, name).ok_or(FsError::NotFound)?;
        if path.last() == Some(&b'/') && !self.nodes[node as usize].is_directory() {
            return Err(FsError::NotDirectory);
        }
        self.removable(node)?;
        self.nodes[node as usize].linked = false;
        self.collect_node(node);
        Ok(())
    }

    /// Atomic namespace mutation: all validation precedes unlinking any target.
    pub fn rename(
        &mut self,
        process: &mut ProcessFs,
        old: &[u8],
        new: &[u8],
    ) -> Result<(), FsError> {
        self.process(process)?;
        let (old_parent, old_name) = self.parent(process.cwd, old)?;
        let source = self.child(old_parent, old_name).ok_or(FsError::NotFound)?;
        let (parent, name) = self.parent(process.cwd, new)?;
        let inode = self.nodes[source as usize];
        if source == ROOT || source == INIT || source == 2 {
            return Err(FsError::Busy);
        }
        if inode.read_only
            || self.nodes[old_parent as usize].read_only
            || self.nodes[parent as usize].read_only
        {
            return Err(FsError::ReadOnly);
        }
        if (old.last() == Some(&b'/') || new.last() == Some(&b'/')) && !inode.is_directory() {
            return Err(FsError::NotDirectory);
        }
        if inode.is_directory() {
            let mut ancestor = parent;
            loop {
                if ancestor == source {
                    return Err(FsError::InvalidArgument);
                }
                if ancestor == ROOT {
                    break;
                }
                ancestor = self.nodes[ancestor as usize].parent;
            }
        }
        if let Some(target) = self.child(parent, name) {
            if target == source {
                return Ok(());
            }
            let target_dir = self.nodes[target as usize].is_directory();
            if target_dir != inode.is_directory() {
                return Err(if target_dir {
                    FsError::IsDirectory
                } else {
                    FsError::NotDirectory
                });
            }
            self.removable(target)?;
            self.nodes[target as usize].linked = false;
            self.collect_node(target);
        }
        let node = &mut self.nodes[source as usize];
        node.parent = parent;
        node.name.fill(0);
        node.name[..name.len()].copy_from_slice(name);
        node.name_length = name.len() as u8;
        Ok(())
    }

    pub fn chdir(&mut self, process: &mut ProcessFs, path: &[u8]) -> Result<(), FsError> {
        self.process(process)?;
        let target = self.resolve(process.cwd, path)?;
        if !self.nodes[target as usize].is_directory() {
            return Err(FsError::NotDirectory);
        }
        if target == process.cwd {
            return Ok(());
        }
        if self.nodes[target as usize].references == u16::MAX {
            return Err(FsError::TooManyFiles);
        }
        self.nodes[target as usize].references += 1;
        self.release_node(process.cwd);
        process.cwd = target;
        Ok(())
    }

    /// Returns a byte count excluding NUL. No bytes are modified on failure;
    /// the syscall adapter is responsible for its chosen NUL convention.
    pub fn getcwd(&self, process: &ProcessFs, output: &mut [u8]) -> Result<usize, FsError> {
        self.process(process)?;
        let mut chain = [NONE; MAX_NODES];
        let mut count = 0;
        let mut length = 1;
        let mut current = process.cwd;
        while current != ROOT {
            chain[count] = current;
            count += 1;
            length += self.nodes[current as usize].name_length as usize + usize::from(count > 1);
            current = self.nodes[current as usize].parent;
        }
        if length > output.len() {
            return Err(FsError::Overflow);
        }
        output[0] = b'/';
        let mut written = 1;
        for (ordinal, &node) in chain[..count].iter().rev().enumerate() {
            if ordinal != 0 {
                output[written] = b'/';
                written += 1;
            }
            let name = self.nodes[node as usize].name();
            output[written..written + name.len()].copy_from_slice(name);
            written += name.len();
        }
        Ok(written)
    }

    /// Stable bytes for the ELF loader; writable files must use bounded reads.
    pub fn boot_file(&self, process: &ProcessFs, path: &[u8]) -> Result<&'static [u8], FsError> {
        self.process(process)?;
        match self.nodes[self.resolve(process.cwd, path)? as usize].kind {
            NodeKind::BootFile(bytes) => Ok(bytes),
            NodeKind::Directory => Err(FsError::IsDirectory),
            _ => Err(FsError::Unsupported),
        }
    }

    /// Kernel loader view. The shared filesystem borrow keeps RAM file bytes
    /// immutable until the loader has copied them into the private user pages.
    pub fn file_bytes<'a>(&'a self, process: &ProcessFs, path: &[u8]) -> Result<&'a [u8], FsError> {
        self.process(process)?;
        match self.nodes[self.resolve(process.cwd, path)? as usize].kind {
            NodeKind::BootFile(bytes) => Ok(bytes),
            NodeKind::RamFile(ram) => Ok(&self.ram[ram as usize][..self.ram_lengths[ram as usize]]),
            NodeKind::Directory => Err(FsError::IsDirectory),
            NodeKind::Empty => Err(FsError::NotFound),
        }
    }

    /// Register immutable boot data under /init before the first process runs.
    /// Intermediate directories are created; failed insertion removes only
    /// directories newly created by this call.
    pub fn add_init_file(&mut self, path: &[u8], bytes: &'static [u8]) -> Result<(), FsError> {
        self.add_init_entry(path, Some(bytes))
    }

    fn add_init_entry(&mut self, path: &[u8], bytes: Option<&'static [u8]>) -> Result<(), FsError> {
        self.ready()?;
        if self.processes != 0 {
            return Err(FsError::Busy);
        }
        Self::validate_path(path)?;
        if path[0] == b'/' {
            return Err(FsError::InvalidArgument);
        }
        if bytes.is_some() && path.last() == Some(&b'/') {
            return Err(FsError::NotDirectory);
        }
        let path = path.strip_suffix(b"/").unwrap_or(path);
        let mut parent = INIT;
        let mut inserted = [NONE; MAX_NODES];
        let mut count = 0;
        let result = (|| {
            let mut components = path.split(|&byte| byte == b'/').peekable();
            while let Some(name) = components.next() {
                if name.is_empty() || name == b"." || name == b".." {
                    return Err(FsError::InvalidArgument);
                }
                if name.len() > MAX_NAME {
                    return Err(FsError::NameTooLong);
                }
                let last = components.peek().is_none();
                if let Some(child) = self.child(parent, name) {
                    if last && bytes.is_some() {
                        return Err(FsError::AlreadyExists);
                    }
                    if !self.nodes[child as usize].is_directory() {
                        return Err(FsError::NotDirectory);
                    }
                    parent = child;
                } else {
                    let index = self.free_node()?;
                    let kind = if last {
                        bytes.map_or(NodeKind::Directory, NodeKind::BootFile)
                    } else {
                        NodeKind::Directory
                    };
                    self.insert_node(index, parent, name, kind, true);
                    inserted[count] = index;
                    count += 1;
                    parent = index;
                }
            }
            Ok(())
        })();
        if result.is_err() {
            for &index in inserted[..count].iter().rev() {
                self.nodes[index as usize].linked = false;
                self.collect_node(index);
            }
        }
        result
    }
}

impl Default for FileSystem {
    fn default() -> Self {
        Self::new()
    }
}

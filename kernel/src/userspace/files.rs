//! Filesystem syscall services shared by the board runtime and host tests.
//!
//! Every counted user buffer is validated in full before an offset, directory
//! cursor, terminal input or filesystem namespace can change. Scratch buffers
//! are bounded; the VFS receives only privileged slices and never user pointers.

use super::abi::{self, Errno};
use super::copy::{self, UserMemory};
use super::space::UserAccess;
use super::syscall;
use crate::fs::{DescriptorKind, DirEntry, FileSystem, FsError, OpenFlags, ProcessFs, SeekWhence};

pub trait Terminal {
    fn read(&mut self, output: &mut [u8]) -> Result<usize, Errno>;
    fn write(&mut self, bytes: &[u8]);
}

pub struct FileServices<'a, M, T> {
    pub fs: &'a mut FileSystem,
    pub process: &'a mut ProcessFs,
    pub memory: &'a M,
    pub terminal: &'a mut T,
}

pub const fn fs_errno(error: FsError) -> Errno {
    match error {
        FsError::NotFound => Errno::NoEntry,
        FsError::BadDescriptor => Errno::BadFileDescriptor,
        FsError::InvalidArgument => Errno::InvalidArgument,
        FsError::NameTooLong => Errno::NameTooLong,
        FsError::NotDirectory => Errno::NotDirectory,
        FsError::IsDirectory => Errno::IsDirectory,
        FsError::AlreadyExists => Errno::Exists,
        FsError::ReadOnly => Errno::ReadOnly,
        FsError::NoSpace => Errno::NoSpace,
        FsError::TooManyFiles => Errno::TooManyFiles,
        FsError::Busy => Errno::Busy,
        FsError::NotEmpty => Errno::NotEmpty,
        FsError::Overflow => Errno::FileTooLarge,
        FsError::Unsupported => Errno::NotImplemented,
    }
}

fn fd(value: u64) -> Result<usize, Errno> {
    usize::try_from(value).map_err(|_| Errno::BadFileDescriptor)
}

fn length(value: u64) -> Result<usize, Errno> {
    if value > abi::MAX_WRITE_BYTES as u64 {
        Err(Errno::InvalidArgument)
    } else {
        Ok(value as usize)
    }
}

/// Translate the public Unix-style open ABI into the VFS's private bit set.
/// The zero access mode means read-only; private READ is deliberately nonzero.
pub fn open_flags(bits: u64) -> Result<OpenFlags, Errno> {
    let known = 3 | abi::O_CREAT | abi::O_EXCL | abi::O_TRUNC | abi::O_APPEND | abi::O_DIRECTORY;
    if bits & !known != 0 {
        return Err(Errno::InvalidArgument);
    }
    let mut flags = match bits & 3 {
        abi::O_RDONLY => OpenFlags::READ,
        abi::O_WRONLY => OpenFlags::WRITE,
        abi::O_RDWR => OpenFlags::READ.union(OpenFlags::WRITE),
        _ => return Err(Errno::InvalidArgument),
    };
    for (external, internal) in [
        (abi::O_CREAT, OpenFlags::CREATE),
        (abi::O_EXCL, OpenFlags::EXCLUSIVE),
        (abi::O_TRUNC, OpenFlags::TRUNCATE),
        (abi::O_APPEND, OpenFlags::APPEND),
        (abi::O_DIRECTORY, OpenFlags::DIRECTORY),
    ] {
        if bits & external != 0 {
            flags = flags.union(internal);
        }
    }
    OpenFlags::from_bits(flags.bits()).map_err(fs_errno)
}

/// Fixed-width ABI kind values, independent of the Rust enum representation.
pub const fn abi_kind(kind: DescriptorKind) -> u32 {
    match kind {
        DescriptorKind::File => 1,
        DescriptorKind::Directory => 2,
        DescriptorKind::Stdin | DescriptorKind::Stdout | DescriptorKind::Stderr => 3,
    }
}

impl<M: UserMemory, T: Terminal> FileServices<'_, M, T> {
    pub fn validate_write_fd(&self, number: u64) -> Result<(), Errno> {
        self.fs
            .validate_write(self.process, fd(number)?)
            .map(|_| ())
            .map_err(fs_errno)
    }

    pub fn write_fd(&mut self, number: u64, address: u64, count: usize) -> Result<u64, Errno> {
        let fd = fd(number)?;
        let kind = self.fs.validate_write(self.process, fd).map_err(fs_errno)?;
        if count > abi::MAX_WRITE_BYTES {
            return Err(Errno::InvalidArgument);
        }
        if count == 0 {
            return Ok(0);
        }
        let mut buffer = [0u8; abi::MAX_WRITE_BYTES];
        copy::copy_in(self.memory, address, &mut buffer[..count])?;
        let written = match kind {
            DescriptorKind::Stdout | DescriptorKind::Stderr => {
                self.terminal.write(&buffer[..count]);
                count
            }
            _ => self
                .fs
                .write(self.process, fd, &buffer[..count])
                .map_err(fs_errno)?,
        };
        Ok(written as u64)
    }

    fn read_fd(&mut self, arguments: [u64; 6]) -> Result<u64, Errno> {
        let fd = fd(arguments[0])?;
        let kind = self.fs.validate_read(self.process, fd).map_err(fs_errno)?;
        let count = length(arguments[2])?;
        if count == 0 {
            return Ok(0);
        }
        self.memory
            .validate(arguments[1], count, UserAccess::Write)?;
        let mut buffer = [0u8; abi::MAX_WRITE_BYTES];
        let read = match kind {
            DescriptorKind::Stdin => self.terminal.read(&mut buffer[..count])?,
            _ => self
                .fs
                .read(self.process, fd, &mut buffer[..count])
                .map_err(fs_errno)?,
        };
        if read > count {
            return Err(Errno::Io);
        }
        self.memory.write_validated(arguments[1], &buffer[..read]);
        Ok(read as u64)
    }

    fn fstat(&mut self, arguments: [u64; 6]) -> Result<u64, Errno> {
        let stat = self
            .fs
            .fstat(self.process, fd(arguments[0])?)
            .map_err(fs_errno)?;
        let kind = abi_kind(stat.kind);
        let mode: u32 = match stat.kind {
            DescriptorKind::File => 0o100000 | if stat.read_only { 0o444 } else { 0o644 },
            DescriptorKind::Directory => 0o040000 | if stat.read_only { 0o555 } else { 0o755 },
            _ => 0o020000 | 0o666,
        };
        let mut bytes = [0u8; abi::STAT_BYTES];
        bytes[..8].copy_from_slice(&stat.inode.to_le_bytes());
        bytes[8..16].copy_from_slice(&stat.size.to_le_bytes());
        bytes[16..20].copy_from_slice(&kind.to_le_bytes());
        bytes[20..24].copy_from_slice(&mode.to_le_bytes());
        bytes[24..32].copy_from_slice(&u64::from(stat.linked).to_le_bytes());
        copy::copy_out(self.memory, arguments[1], &bytes)?;
        Ok(0)
    }

    fn getdents(&mut self, arguments: [u64; 6]) -> Result<u64, Errno> {
        let fd = fd(arguments[0])?;
        if self
            .fs
            .descriptor_kind(self.process, fd)
            .map_err(fs_errno)?
            != DescriptorKind::Directory
        {
            return Err(Errno::NotDirectory);
        }
        let count = length(arguments[2])?;
        if count < abi::DIRENT_BYTES {
            return Err(Errno::InvalidArgument);
        }
        self.memory
            .validate(arguments[1], count, UserAccess::Write)?;
        let mut entries = [DirEntry::EMPTY; 1];
        if self
            .fs
            .getdents(self.process, fd, &mut entries)
            .map_err(fs_errno)?
            == 0
        {
            return Ok(0);
        }
        let entry = entries[0];
        let mut bytes = [0u8; abi::DIRENT_BYTES];
        bytes[..8].copy_from_slice(&entry.inode.to_le_bytes());
        bytes[8..12].copy_from_slice(&abi_kind(entry.kind).to_le_bytes());
        bytes[12..16].copy_from_slice(&(entry.name_length as u32).to_le_bytes());
        bytes[16..16 + entry.name_length].copy_from_slice(&entry.name[..entry.name_length]);
        self.memory.write_validated(arguments[1], &bytes);
        Ok(bytes.len() as u64)
    }

    fn getcwd(&mut self, arguments: [u64; 6]) -> Result<u64, Errno> {
        let count = length(arguments[1])?;
        if count == 0 {
            return Err(Errno::InvalidArgument);
        }
        self.memory
            .validate(arguments[0], count, UserAccess::Write)?;
        let mut bytes = [0u8; abi::MAX_WRITE_BYTES];
        let written = self
            .fs
            .getcwd(self.process, &mut bytes[..count - 1])
            .map_err(|error| {
                if error == FsError::Overflow {
                    Errno::Range
                } else {
                    fs_errno(error)
                }
            })?;
        bytes[written] = 0;
        self.memory
            .write_validated(arguments[0], &bytes[..written + 1]);
        Ok((written + 1) as u64)
    }

    pub fn filesystem_call(&mut self, number: u64, arguments: [u64; 6]) -> Result<u64, Errno> {
        match number {
            abi::SYS_READ => self.read_fd(arguments),
            abi::SYS_OPEN => {
                let flags = open_flags(arguments[2])?;
                let mut path = [0u8; abi::MAX_PATH_BYTES];
                let count = copy::path_in(self.memory, arguments[0], arguments[1], &mut path)?;
                self.fs
                    .open(self.process, &path[..count], flags)
                    .map(|fd| fd as u64)
                    .map_err(fs_errno)
            }
            abi::SYS_CLOSE => {
                self.fs
                    .close(self.process, fd(arguments[0])?)
                    .map_err(fs_errno)?;
                Ok(0)
            }
            abi::SYS_LSEEK => {
                let fd = fd(arguments[0])?;
                // Descriptor validation precedes the whence argument too.
                self.fs
                    .descriptor_kind(self.process, fd)
                    .map_err(fs_errno)?;
                let whence = match arguments[2] {
                    0 => SeekWhence::Start,
                    1 => SeekWhence::Current,
                    2 => SeekWhence::End,
                    _ => return Err(Errno::InvalidArgument),
                };
                self.fs
                    .seek(self.process, fd, arguments[1] as i64, whence)
                    .map_err(|error| {
                        if error == FsError::Unsupported {
                            Errno::IllegalSeek
                        } else {
                            fs_errno(error)
                        }
                    })
            }
            abi::SYS_FSTAT => self.fstat(arguments),
            abi::SYS_GETDENTS => self.getdents(arguments),
            abi::SYS_MKDIR | abi::SYS_UNLINK | abi::SYS_CHDIR => {
                let mut path = [0u8; abi::MAX_PATH_BYTES];
                let count = copy::path_in(self.memory, arguments[0], arguments[1], &mut path)?;
                match number {
                    abi::SYS_MKDIR => self.fs.mkdir(self.process, &path[..count]),
                    abi::SYS_UNLINK => self.fs.unlink(self.process, &path[..count]),
                    _ => self.fs.chdir(self.process, &path[..count]),
                }
                .map_err(fs_errno)?;
                Ok(0)
            }
            abi::SYS_RENAME => {
                let mut old = [0u8; abi::MAX_PATH_BYTES];
                let mut new = [0u8; abi::MAX_PATH_BYTES];
                let old_count = copy::path_in(self.memory, arguments[0], arguments[1], &mut old)?;
                let new_count = copy::path_in(self.memory, arguments[2], arguments[3], &mut new)?;
                self.fs
                    .rename(self.process, &old[..old_count], &new[..new_count])
                    .map_err(fs_errno)?;
                Ok(0)
            }
            abi::SYS_GETCWD => self.getcwd(arguments),
            abi::SYS_DUP => self
                .fs
                .dup(self.process, fd(arguments[0])?)
                .map(|fd| fd as u64)
                .map_err(fs_errno),
            abi::SYS_DUP2 => self
                .fs
                .dup2(self.process, fd(arguments[0])?, fd(arguments[1])?)
                .map(|fd| fd as u64)
                .map_err(fs_errno),
            _ => Err(Errno::NotImplemented),
        }
    }
}

impl<M: UserMemory, T: Terminal> syscall::Services for FileServices<'_, M, T> {
    fn validate_write(&self, fd: u64) -> Result<(), Errno> {
        self.validate_write_fd(fd)
    }
    fn write(&mut self, fd: u64, address: u64, length: usize) -> Result<u64, Errno> {
        self.write_fd(fd, address, length)
    }
    fn yield_now(&mut self) {}
    fn call(&mut self, number: u64, arguments: [u64; 6]) -> Result<u64, Errno> {
        self.filesystem_call(number, arguments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::context::ExceptionContext;
    use core::cell::{Cell, RefCell};
    use std::boxed::Box;
    use std::vec::Vec;

    const BASE: u64 = super::super::space::USER_DATA;
    struct Memory {
        bytes: RefCell<[u8; 8192]>,
        deny_read: Cell<Option<(usize, usize)>>,
        deny_write: Cell<Option<(usize, usize)>>,
        reads: Cell<usize>,
        writes: Cell<usize>,
    }
    impl Memory {
        fn new() -> Self {
            Self {
                bytes: RefCell::new([0; 8192]),
                deny_read: Cell::new(None),
                deny_write: Cell::new(None),
                reads: Cell::new(0),
                writes: Cell::new(0),
            }
        }
        fn store(&self, offset: usize, bytes: &[u8]) -> u64 {
            self.bytes.borrow_mut()[offset..offset + bytes.len()].copy_from_slice(bytes);
            BASE + offset as u64
        }
        fn offset(address: u64) -> usize {
            usize::try_from(address - BASE).unwrap()
        }
        fn load(&self, offset: usize, count: usize) -> Vec<u8> {
            self.bytes.borrow()[offset..offset + count].to_vec()
        }
    }
    impl UserMemory for Memory {
        fn validate(&self, address: u64, length: usize, access: UserAccess) -> Result<(), Errno> {
            let offset = address
                .checked_sub(BASE)
                .and_then(|offset| usize::try_from(offset).ok())
                .ok_or(Errno::Fault)?;
            let end = offset
                .checked_add(length)
                .filter(|&end| end <= 8192)
                .ok_or(Errno::Fault)?;
            let denied = if access == UserAccess::Write {
                self.deny_write.get()
            } else {
                self.deny_read.get()
            };
            if denied.is_some_and(|(start, stop)| offset < stop && end > start) {
                return Err(Errno::Fault);
            }
            Ok(())
        }
        fn read_validated(&self, address: u64, output: &mut [u8]) {
            self.reads.set(self.reads.get() + 1);
            let offset = Self::offset(address);
            output.copy_from_slice(&self.bytes.borrow()[offset..offset + output.len()]);
        }
        fn write_validated(&self, address: u64, input: &[u8]) {
            self.writes.set(self.writes.get() + 1);
            let offset = Self::offset(address);
            self.bytes.borrow_mut()[offset..offset + input.len()].copy_from_slice(input);
        }
    }
    #[derive(Default)]
    struct Console {
        bytes: Vec<u8>,
        input: Vec<u8>,
        consumed: usize,
    }
    impl Terminal for Console {
        fn read(&mut self, output: &mut [u8]) -> Result<usize, Errno> {
            let count = output.len().min(self.input.len() - self.consumed);
            output[..count].copy_from_slice(&self.input[self.consumed..self.consumed + count]);
            self.consumed += count;
            Ok(count)
        }
        fn write(&mut self, bytes: &[u8]) {
            self.bytes.extend_from_slice(bytes);
        }
    }
    fn setup() -> (Box<FileSystem>, ProcessFs, Memory, Console) {
        let mut fs = Box::new(FileSystem::new());
        fs.initialize().unwrap();
        fs.add_init_file(b"greeting", b"hello").unwrap();
        let mut process = ProcessFs::new();
        fs.attach_process(&mut process).unwrap();
        (fs, process, Memory::new(), Console::default())
    }
    fn invoke(
        services: &mut FileServices<'_, Memory, Console>,
        number: u64,
        arguments: [u64; 6],
    ) -> i64 {
        let mut frame = ExceptionContext {
            registers: [0; 31],
            elr_el1: BASE + 4,
            spsr_el1: 0,
            esr_el1: (syscall::AARCH64_SVC_CLASS << 26) | (1 << 25),
        };
        frame.registers[..6].copy_from_slice(&arguments);
        frame.registers[8] = number;
        assert_eq!(
            syscall::dispatch(&mut frame, services),
            Ok(syscall::Action::Resume)
        );
        assert_eq!(frame.elr_el1, BASE + 4);
        frame.registers[0] as i64
    }

    #[test]
    fn dispatcher_file_lifecycle_serializes_fixed_width_stat_cwd_and_directory_records() {
        let (mut fs, mut process, memory, mut terminal) = setup();
        let name = b"/tmp/file";
        let path = memory.store(0, name);
        let data = memory.store(512, b"contents");
        let mut services = FileServices {
            fs: &mut fs,
            process: &mut process,
            memory: &memory,
            terminal: &mut terminal,
        };
        let opened = invoke(
            &mut services,
            abi::SYS_OPEN,
            [path, name.len() as u64, abi::O_RDWR | abi::O_CREAT, 0, 0, 0],
        );
        assert_eq!(opened, 3);
        assert_eq!(
            invoke(&mut services, abi::SYS_WRITE, [3, data, 8, 0, 0, 0]),
            8
        );
        assert_eq!(invoke(&mut services, abi::SYS_LSEEK, [3, 0, 0, 0, 0, 0]), 0);
        assert_eq!(
            invoke(&mut services, abi::SYS_DUP2, [3, 12, 0, 0, 0, 0]),
            12
        );
        assert_eq!(
            invoke(&mut services, abi::SYS_READ, [12, BASE + 1024, 8, 0, 0, 0]),
            8
        );
        assert_eq!(memory.load(1024, 8), b"contents");
        assert_eq!(
            invoke(&mut services, abi::SYS_FSTAT, [3, BASE + 1200, 0, 0, 0, 0]),
            0
        );
        let stat = memory.load(1200, abi::STAT_BYTES);
        assert_ne!(u64::from_le_bytes(stat[..8].try_into().unwrap()), 0);
        assert_eq!(u64::from_le_bytes(stat[8..16].try_into().unwrap()), 8);
        assert_eq!(u32::from_le_bytes(stat[16..20].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_le_bytes(stat[20..24].try_into().unwrap()),
            0o100644
        );
        assert_eq!(u64::from_le_bytes(stat[24..].try_into().unwrap()), 1);
        let tmp = memory.store(128, b"/tmp");
        assert_eq!(
            invoke(&mut services, abi::SYS_CHDIR, [tmp, 4, 0, 0, 0, 0]),
            0
        );
        assert_eq!(
            invoke(&mut services, abi::SYS_GETCWD, [BASE + 1300, 5, 0, 0, 0, 0]),
            5
        );
        assert_eq!(memory.load(1300, 5), b"/tmp\0");
        assert_eq!(
            invoke(&mut services, abi::SYS_GETCWD, [BASE + 1300, 4, 0, 0, 0, 0]),
            Errno::Range.result() as i64
        );
        let directory = invoke(
            &mut services,
            abi::SYS_OPEN,
            [tmp, 4, abi::O_DIRECTORY, 0, 0, 0],
        ) as u64;
        assert_eq!(
            invoke(
                &mut services,
                abi::SYS_GETDENTS,
                [directory, BASE + 1400, 80, 0, 0, 0]
            ),
            80
        );
        let entry = memory.load(1400, 80);
        assert_eq!(u32::from_le_bytes(entry[8..12].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(entry[12..16].try_into().unwrap()), 1);
        assert_eq!(entry[16], b'.');
        assert!(entry[17..].iter().all(|&byte| byte == 0));
        assert_eq!(invoke(&mut services, abi::SYS_CLOSE, [3, 0, 0, 0, 0, 0]), 0);
        assert_eq!(
            invoke(&mut services, abi::SYS_CLOSE, [12, 0, 0, 0, 0, 0]),
            0
        );
    }

    #[test]
    fn invalid_read_destination_does_not_consume_file_offsets_or_terminal_input() {
        let (mut fs, mut process, memory, mut terminal) = setup();
        terminal.input.extend_from_slice(b"input");
        let file = fs
            .open(&mut process, b"/init/greeting", OpenFlags::READ)
            .unwrap() as u64;
        let mut services = FileServices {
            fs: &mut fs,
            process: &mut process,
            memory: &memory,
            terminal: &mut terminal,
        };
        memory.deny_write.set(Some((4096, 8192)));
        assert_eq!(
            invoke(
                &mut services,
                abi::SYS_READ,
                [file, BASE + 4094, 5, 0, 0, 0]
            ),
            Errno::Fault.result() as i64
        );
        assert_eq!(
            invoke(&mut services, abi::SYS_LSEEK, [file, 0, 1, 0, 0, 0]),
            0
        );
        assert_eq!(
            invoke(&mut services, abi::SYS_READ, [0, BASE + 4094, 5, 0, 0, 0]),
            Errno::Fault.result() as i64
        );
        assert_eq!(services.terminal.consumed, 0);
        assert_eq!(memory.writes.get(), 0);
        memory.deny_write.set(None);
        assert_eq!(
            invoke(
                &mut services,
                abi::SYS_READ,
                [file, BASE + 1024, 5, 0, 0, 0]
            ),
            5
        );
        assert_eq!(memory.load(1024, 5), b"hello");
        assert_eq!(
            invoke(&mut services, abi::SYS_READ, [0, BASE + 1030, 5, 0, 0, 0]),
            5
        );
        assert_eq!(memory.load(1030, 5), b"input");
    }

    #[test]
    fn invalid_directory_destination_preserves_cookie_and_zero_bytes() {
        let (mut fs, mut process, memory, mut terminal) = setup();
        let directory = fs
            .open(
                &mut process,
                b"/tmp",
                OpenFlags::READ.union(OpenFlags::DIRECTORY),
            )
            .unwrap() as u64;
        let mut services = FileServices {
            fs: &mut fs,
            process: &mut process,
            memory: &memory,
            terminal: &mut terminal,
        };
        memory.deny_write.set(Some((4096, 8192)));
        // Even an invalid tail beyond the emitted 80 bytes rejects the whole request.
        assert_eq!(
            invoke(
                &mut services,
                abi::SYS_GETDENTS,
                [directory, BASE + 3900, 256, 0, 0, 0]
            ),
            Errno::Fault.result() as i64
        );
        assert_eq!(memory.writes.get(), 0);
        memory.deny_write.set(None);
        assert_eq!(
            invoke(
                &mut services,
                abi::SYS_GETDENTS,
                [directory, BASE + 3900, 256, 0, 0, 0]
            ),
            80
        );
        assert_eq!(memory.load(3916, 2), [b'.', 0]);
    }

    #[test]
    fn invalid_paths_or_write_sources_cannot_create_truncate_rename_or_emit_bytes() {
        let (mut fs, mut process, memory, mut terminal) = setup();
        let file = fs
            .open(
                &mut process,
                b"/tmp/keep",
                OpenFlags::READ
                    .union(OpenFlags::WRITE)
                    .union(OpenFlags::CREATE),
            )
            .unwrap() as u64;
        fs.write(&mut process, file as usize, b"keep").unwrap();
        let create = memory.store(4090, b"/tmp/new");
        let keep = memory.store(0, b"/tmp/keep");
        let mut services = FileServices {
            fs: &mut fs,
            process: &mut process,
            memory: &memory,
            terminal: &mut terminal,
        };
        memory.deny_read.set(Some((4096, 8192)));
        assert_eq!(
            invoke(
                &mut services,
                abi::SYS_OPEN,
                [create, 8, abi::O_RDWR | abi::O_CREAT, 0, 0, 0]
            ),
            Errno::Fault.result() as i64
        );
        assert_eq!(
            services.fs.stat(services.process, b"/tmp/new"),
            Err(FsError::NotFound)
        );
        assert_eq!(memory.reads.get(), 0);
        memory.deny_read.set(Some((0, 9)));
        assert_eq!(
            invoke(
                &mut services,
                abi::SYS_OPEN,
                [keep, 9, abi::O_WRONLY | abi::O_TRUNC, 0, 0, 0]
            ),
            Errno::Fault.result() as i64
        );
        assert_eq!(
            services
                .fs
                .fstat(services.process, file as usize)
                .unwrap()
                .size,
            4
        );
        memory.deny_read.set(Some((4096, 8192)));
        assert_eq!(
            invoke(&mut services, abi::SYS_RENAME, [keep, 9, create, 8, 0, 0]),
            Errno::Fault.result() as i64
        );
        assert_eq!(
            services
                .fs
                .fstat(services.process, file as usize)
                .unwrap()
                .size,
            4
        );
        assert_eq!(
            invoke(
                &mut services,
                abi::SYS_WRITE,
                [file, BASE + 4094, 5, 0, 0, 0]
            ),
            Errno::Fault.result() as i64
        );
        assert_eq!(
            invoke(&mut services, abi::SYS_WRITE, [1, BASE + 4094, 5, 0, 0, 0]),
            Errno::Fault.result() as i64
        );
        assert!(services.terminal.bytes.is_empty());
        assert_eq!(
            invoke(&mut services, abi::SYS_LSEEK, [file, 0, 1, 0, 0, 0]),
            4
        );
    }

    #[test]
    fn bad_descriptor_precedes_length_and_pointer_and_empty_io_never_dereferences() {
        let (mut fs, mut process, memory, mut terminal) = setup();
        let mut services = FileServices {
            fs: &mut fs,
            process: &mut process,
            memory: &memory,
            terminal: &mut terminal,
        };
        for number in [abi::SYS_READ, abi::SYS_WRITE] {
            assert_eq!(
                invoke(&mut services, number, [99, u64::MAX, u64::MAX, 0, 0, 0]),
                Errno::BadFileDescriptor.result() as i64
            );
        }
        for (number, fd) in [(abi::SYS_READ, 0), (abi::SYS_WRITE, 1), (abi::SYS_WRITE, 2)] {
            assert_eq!(invoke(&mut services, number, [fd, u64::MAX, 0, 0, 0, 0]), 0);
            assert_eq!(
                invoke(&mut services, number, [fd, u64::MAX, 4097, 0, 0, 0]),
                Errno::InvalidArgument.result() as i64
            );
        }
        assert_eq!(
            invoke(&mut services, abi::SYS_READ, [1, 0, 0, 0, 0, 0]),
            Errno::BadFileDescriptor.result() as i64
        );
        assert_eq!(
            invoke(&mut services, abi::SYS_WRITE, [0, 0, 0, 0, 0, 0]),
            Errno::BadFileDescriptor.result() as i64
        );
        assert_eq!(memory.reads.get(), 0);
        assert_eq!(memory.writes.get(), 0);
        assert_eq!(
            invoke(&mut services, abi::SYS_LSEEK, [1, 0, 0, 0, 0, 0]),
            Errno::IllegalSeek.result() as i64
        );
    }

    #[test]
    fn relative_paths_redirection_rename_and_unlinked_open_file_survive_dispatch() {
        let (mut fs, mut process, memory, mut terminal) = setup();
        let directory = memory.store(0, b"/tmp/work");
        let old = memory.store(128, b"old");
        let new = memory.store(256, b"new");
        let data = memory.store(512, b"redirected");
        let mut services = FileServices {
            fs: &mut fs,
            process: &mut process,
            memory: &memory,
            terminal: &mut terminal,
        };
        assert_eq!(
            invoke(&mut services, abi::SYS_MKDIR, [directory, 9, 0, 0, 0, 0]),
            0
        );
        assert_eq!(
            invoke(&mut services, abi::SYS_CHDIR, [directory, 9, 0, 0, 0, 0]),
            0
        );
        assert_eq!(
            invoke(
                &mut services,
                abi::SYS_OPEN,
                [old, 3, abi::O_RDWR | abi::O_CREAT, 0, 0, 0]
            ),
            3
        );
        assert_eq!(invoke(&mut services, abi::SYS_DUP, [3, 0, 0, 0, 0, 0]), 4);
        assert_eq!(invoke(&mut services, abi::SYS_DUP2, [4, 1, 0, 0, 0, 0]), 1);
        assert_eq!(
            invoke(&mut services, abi::SYS_WRITE, [1, data, 10, 0, 0, 0]),
            10
        );
        assert!(services.terminal.bytes.is_empty());
        assert_eq!(
            invoke(&mut services, abi::SYS_RENAME, [old, 3, new, 3, 0, 0]),
            0
        );
        assert_eq!(
            services.fs.stat(services.process, b"old"),
            Err(FsError::NotFound)
        );
        assert_eq!(services.fs.stat(services.process, b"new").unwrap().size, 10);
        assert_eq!(
            invoke(&mut services, abi::SYS_UNLINK, [new, 3, 0, 0, 0, 0]),
            0
        );
        assert_eq!(
            invoke(&mut services, abi::SYS_FSTAT, [3, BASE + 1024, 0, 0, 0, 0]),
            0
        );
        let stat = memory.load(1024, abi::STAT_BYTES);
        assert_eq!(u64::from_le_bytes(stat[24..].try_into().unwrap()), 0);
        assert_eq!(invoke(&mut services, abi::SYS_LSEEK, [4, 0, 0, 0, 0, 0]), 0);
        assert_eq!(
            invoke(&mut services, abi::SYS_READ, [3, BASE + 1100, 10, 0, 0, 0]),
            10
        );
        assert_eq!(memory.load(1100, 10), b"redirected");
        services.fs.cleanup_process(services.process);
    }

    #[test]
    fn unix_open_flags_allow_fixture_readonly_and_rdwr_create_truncate_and_append() {
        let (mut fs, mut process, memory, mut terminal) = setup();
        let greeting = memory.store(0, b"/init/greeting");
        let path = memory.store(128, b"/tmp/public-flags");
        let data = memory.store(512, b"abc");
        let mut services = FileServices {
            fs: &mut fs,
            process: &mut process,
            memory: &memory,
            terminal: &mut terminal,
        };
        let read = invoke(
            &mut services,
            abi::SYS_OPEN,
            [greeting, 14, abi::O_RDONLY, 0, 0, 0],
        ) as u64;
        assert_eq!(
            invoke(
                &mut services,
                abi::SYS_READ,
                [read, BASE + 1024, 5, 0, 0, 0]
            ),
            5
        );
        assert_eq!(memory.load(1024, 5), b"hello");
        assert_eq!(
            invoke(&mut services, abi::SYS_WRITE, [read, data, 3, 0, 0, 0]),
            Errno::BadFileDescriptor.result() as i64
        );
        let file = invoke(
            &mut services,
            abi::SYS_OPEN,
            [path, 17, abi::O_RDWR | abi::O_CREAT | abi::O_TRUNC, 0, 0, 0],
        ) as u64;
        assert_eq!(
            invoke(&mut services, abi::SYS_WRITE, [file, data, 3, 0, 0, 0]),
            3
        );
        assert_eq!(
            invoke(
                &mut services,
                abi::SYS_OPEN,
                [
                    path,
                    17,
                    abi::O_WRONLY | abi::O_CREAT | abi::O_EXCL,
                    0,
                    0,
                    0
                ]
            ),
            Errno::Exists.result() as i64
        );
        let append = invoke(
            &mut services,
            abi::SYS_OPEN,
            [path, 17, abi::O_WRONLY | abi::O_APPEND, 0, 0, 0],
        ) as u64;
        assert_eq!(
            invoke(&mut services, abi::SYS_WRITE, [append, data, 3, 0, 0, 0]),
            3
        );
        assert_eq!(
            invoke(&mut services, abi::SYS_LSEEK, [file, 0, 0, 0, 0, 0]),
            0
        );
        assert_eq!(
            invoke(
                &mut services,
                abi::SYS_READ,
                [file, BASE + 1030, 6, 0, 0, 0]
            ),
            6
        );
        assert_eq!(memory.load(1030, 6), b"abcabc");
        assert!(
            invoke(
                &mut services,
                abi::SYS_OPEN,
                [path, 17, abi::O_WRONLY | abi::O_TRUNC, 0, 0, 0]
            ) >= 0
        );
        assert_eq!(
            services
                .fs
                .fstat(services.process, file as usize)
                .unwrap()
                .size,
            0
        );
        for flags in [
            3,
            abi::O_EXCL,
            abi::O_RDONLY | abi::O_TRUNC,
            abi::O_RDONLY | abi::O_APPEND,
            1 << 63,
            0x100,
        ] {
            assert_eq!(
                invoke(&mut services, abi::SYS_OPEN, [u64::MAX, 1, flags, 0, 0, 0]),
                Errno::InvalidArgument.result() as i64
            );
        }
    }
}

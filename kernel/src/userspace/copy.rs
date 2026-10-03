//! Whole-range user copies; mappings and ownership stay frozen through side effects.
use super::abi::{self, Errno};
use super::space::UserAccess;

pub trait UserMemory {
    fn validate(&self, address: u64, length: usize, access: UserAccess) -> Result<(), Errno>;
    /// Range has already been validated under the same mapping guard.
    fn read_validated(&self, address: u64, output: &mut [u8]);
    /// Writable range has already been validated under the same mapping guard.
    fn write_validated(&self, address: u64, input: &[u8]);
}
pub fn copy_in(memory: &impl UserMemory, address: u64, output: &mut [u8]) -> Result<(), Errno> {
    if !output.is_empty() {
        memory.validate(address, output.len(), UserAccess::Read)?;
        memory.read_validated(address, output);
    }
    Ok(())
}
pub fn copy_out(memory: &impl UserMemory, address: u64, input: &[u8]) -> Result<(), Errno> {
    if !input.is_empty() {
        memory.validate(address, input.len(), UserAccess::Write)?;
        memory.write_validated(address, input);
    }
    Ok(())
}
pub fn path_in(
    memory: &impl UserMemory,
    address: u64,
    length: u64,
    buffer: &mut [u8; abi::MAX_PATH_BYTES],
) -> Result<usize, Errno> {
    if length == 0 {
        return Err(Errno::NoEntry);
    }
    if length > buffer.len() as u64 {
        return Err(Errno::NameTooLong);
    }
    let length = length as usize;
    copy_in(memory, address, &mut buffer[..length])?;
    if buffer[..length].contains(&0) {
        return Err(Errno::InvalidArgument);
    }
    Ok(length)
}
/// Spawn/exec accept counted {address,length} pairs. ELF entry constructs the
/// ordinary Unix argc/argv/envp stack from these bounded copied strings.
pub struct Arguments {
    argv: [[u8; abi::MAX_ARGUMENT_BYTES]; abi::MAX_ARGUMENTS],
    envp: [[u8; abi::MAX_ARGUMENT_BYTES]; abi::MAX_ARGUMENTS],
    argv_lengths: [usize; abi::MAX_ARGUMENTS],
    envp_lengths: [usize; abi::MAX_ARGUMENTS],
    argc: usize,
    envc: usize,
}
impl Arguments {
    pub const fn new() -> Self {
        Self {
            argv: [[0; abi::MAX_ARGUMENT_BYTES]; abi::MAX_ARGUMENTS],
            envp: [[0; abi::MAX_ARGUMENT_BYTES]; abi::MAX_ARGUMENTS],
            argv_lengths: [0; abi::MAX_ARGUMENTS],
            envp_lengths: [0; abi::MAX_ARGUMENTS],
            argc: 0,
            envc: 0,
        }
    }
    pub fn from_user(
        &mut self,
        memory: &impl UserMemory,
        argv: u64,
        argc: u64,
        envp: u64,
        envc: u64,
    ) -> Result<(), Errno> {
        if argc > abi::MAX_ARGUMENTS as u64 || envc > abi::MAX_ARGUMENTS as u64 {
            return Err(Errno::InvalidArgument);
        }
        fn vector(
            memory: &impl UserMemory,
            address: u64,
            count: usize,
            strings: &mut [[u8; abi::MAX_ARGUMENT_BYTES]; abi::MAX_ARGUMENTS],
            lengths: &mut [usize; abi::MAX_ARGUMENTS],
        ) -> Result<(), Errno> {
            let mut pairs = [0u8; abi::MAX_ARGUMENTS * 16];
            copy_in(memory, address, &mut pairs[..count * 16])?;
            for index in 0..count {
                let pair = &pairs[index * 16..index * 16 + 16];
                let pointer = u64::from_le_bytes(pair[..8].try_into().unwrap());
                let length = u64::from_le_bytes(pair[8..].try_into().unwrap());
                if length >= abi::MAX_ARGUMENT_BYTES as u64 {
                    return Err(Errno::InvalidArgument);
                }
                lengths[index] = length as usize;
                copy_in(memory, pointer, &mut strings[index][..length as usize])?;
                if strings[index][..length as usize].contains(&0) {
                    return Err(Errno::InvalidArgument);
                }
            }
            Ok(())
        }
        vector(
            memory,
            argv,
            argc as usize,
            &mut self.argv,
            &mut self.argv_lengths,
        )?;
        vector(
            memory,
            envp,
            envc as usize,
            &mut self.envp,
            &mut self.envp_lengths,
        )?;
        self.argc = argc as usize;
        self.envc = envc as usize;
        Ok(())
    }
    pub fn argv(&self) -> ([&[u8]; abi::MAX_ARGUMENTS], usize) {
        (
            core::array::from_fn(|i| &self.argv[i][..self.argv_lengths[i]]),
            self.argc,
        )
    }
    pub fn envp(&self) -> ([&[u8]; abi::MAX_ARGUMENTS], usize) {
        (
            core::array::from_fn(|i| &self.envp[i][..self.envp_lengths[i]]),
            self.envc,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::{Cell, RefCell};

    const BASE: u64 = 0x40_0000_0000;

    struct Memory {
        bytes: RefCell<[u8; 8192]>,
        reads: Cell<usize>,
        writes: Cell<usize>,
        denied: Cell<Option<(usize, usize)>>,
    }

    impl Memory {
        fn new() -> Self {
            Self {
                bytes: RefCell::new([0; 8192]),
                reads: Cell::new(0),
                writes: Cell::new(0),
                denied: Cell::new(None),
            }
        }
        fn store(&self, offset: usize, bytes: &[u8]) -> u64 {
            self.bytes.borrow_mut()[offset..offset + bytes.len()].copy_from_slice(bytes);
            BASE + offset as u64
        }
        fn pair(&self, offset: usize, pointer: u64, length: u64) -> u64 {
            let mut bytes = [0; 16];
            bytes[..8].copy_from_slice(&pointer.to_le_bytes());
            bytes[8..].copy_from_slice(&length.to_le_bytes());
            self.store(offset, &bytes)
        }
    }

    impl UserMemory for Memory {
        fn validate(&self, address: u64, length: usize, _access: UserAccess) -> Result<(), Errno> {
            let start = address
                .checked_sub(BASE)
                .and_then(|start| usize::try_from(start).ok())
                .ok_or(Errno::Fault)?;
            let end = start
                .checked_add(length)
                .filter(|&end| end <= 8192)
                .ok_or(Errno::Fault)?;
            if self
                .denied
                .get()
                .is_some_and(|(lo, hi)| start < hi && end > lo)
            {
                return Err(Errno::Fault);
            }
            Ok(())
        }
        fn read_validated(&self, address: u64, output: &mut [u8]) {
            self.reads.set(self.reads.get() + 1);
            let start = (address - BASE) as usize;
            output.copy_from_slice(&self.bytes.borrow()[start..start + output.len()]);
        }
        fn write_validated(&self, address: u64, input: &[u8]) {
            self.writes.set(self.writes.get() + 1);
            let start = (address - BASE) as usize;
            self.bytes.borrow_mut()[start..start + input.len()].copy_from_slice(input);
        }
    }

    #[test]
    fn counted_argument_pairs_preserve_empty_and_maximum_sized_strings_without_user_writes() {
        let memory = Memory::new();
        let long = memory.store(1024, &[b'x'; abi::MAX_ARGUMENT_BYTES - 1]);
        let env = memory.store(1536, b"X=one");
        let argv = memory.pair(0, long, (abi::MAX_ARGUMENT_BYTES - 1) as u64);
        memory.pair(16, u64::MAX, 0);
        let envp = memory.pair(256, env, 5);
        let before = *memory.bytes.borrow();
        let mut arguments = Arguments::new();
        arguments.from_user(&memory, argv, 2, envp, 1).unwrap();
        let (values, count) = arguments.argv();
        assert_eq!(count, 2);
        assert_eq!(values[0], &[b'x'; abi::MAX_ARGUMENT_BYTES - 1]);
        assert!(values[1].is_empty());
        let (values, count) = arguments.envp();
        assert_eq!(count, 1);
        assert_eq!(values[0], b"X=one");
        assert_eq!(*memory.bytes.borrow(), before);
        assert_eq!(memory.writes.get(), 0);
        assert_eq!(memory.reads.get(), 4);
    }

    #[test]
    fn zero_vectors_skip_pointers_and_count_limits_precede_all_memory_access() {
        let memory = Memory::new();
        let mut arguments = Arguments::new();
        arguments
            .from_user(&memory, u64::MAX, 0, u64::MAX, 0)
            .unwrap();
        assert_eq!(arguments.argv().1, 0);
        assert_eq!(arguments.envp().1, 0);
        for (argc, envc) in [(9, 0), (0, 9), (u64::MAX, 1), (1, u64::MAX)] {
            assert_eq!(
                arguments.from_user(&memory, u64::MAX, argc, u64::MAX, envc),
                Err(Errno::InvalidArgument)
            );
        }
        assert_eq!(memory.reads.get(), 0);
        assert_eq!(memory.writes.get(), 0);
    }

    #[test]
    fn complete_pair_vector_is_validated_before_reading_even_its_valid_prefix() {
        let memory = Memory::new();
        let string = memory.store(1024, b"safe");
        let argv = memory.pair(4080, string, 4);
        memory.pair(4096, string, 4);
        memory.denied.set(Some((4096, 8192)));
        let before = *memory.bytes.borrow();
        let mut arguments = Arguments::new();
        assert_eq!(
            arguments.from_user(&memory, argv, 2, 0, 0),
            Err(Errno::Fault)
        );
        assert_eq!(memory.reads.get(), 0);
        assert_eq!(memory.writes.get(), 0);
        assert_eq!(*memory.bytes.borrow(), before);
        assert_eq!(arguments.argv().1, 0);
        assert_eq!(arguments.envp().1, 0);
    }

    #[test]
    fn invalid_string_ranges_lengths_and_embedded_null_cannot_publish_arguments() {
        let memory = Memory::new();
        let good = memory.store(1024, b"safe");
        let nul = memory.store(1056, b"bad\0value");
        for (pointer, length, error) in [
            (u64::MAX, 4, Errno::Fault),
            (BASE + 8190, 4, Errno::Fault),
            (good, 256, Errno::InvalidArgument),
            (good, u64::MAX, Errno::InvalidArgument),
            (nul, 9, Errno::InvalidArgument),
        ] {
            let argv = memory.pair(0, pointer, length);
            let before = *memory.bytes.borrow();
            let mut arguments = Arguments::new();
            assert_eq!(arguments.from_user(&memory, argv, 1, 0, 0), Err(error));
            assert_eq!(arguments.argv().1, 0);
            assert_eq!(arguments.envp().1, 0);
            assert_eq!(memory.writes.get(), 0);
            assert_eq!(*memory.bytes.borrow(), before);
        }
        let argv = memory.pair(0, good, 4);
        let mut arguments = Arguments::new();
        assert_eq!(
            arguments.from_user(&memory, argv, 1, u64::MAX, 1),
            Err(Errno::Fault)
        );
        assert_eq!(arguments.argv().1, 0);
        assert_eq!(arguments.envp().1, 0);
    }

    #[test]
    fn copy_and_path_failures_have_no_partial_output_or_user_memory_effects() {
        let memory = Memory::new();
        let source = memory.store(4088, b"valid-and-denied");
        memory.denied.set(Some((4096, 8192)));
        let before = *memory.bytes.borrow();
        let mut output = [0xa5; 16];
        assert_eq!(copy_in(&memory, source, &mut output), Err(Errno::Fault));
        assert_eq!(output, [0xa5; 16]);
        assert_eq!(copy_out(&memory, source, &[1; 16]), Err(Errno::Fault));
        assert_eq!(*memory.bytes.borrow(), before);
        assert_eq!(memory.reads.get(), 0);
        assert_eq!(memory.writes.get(), 0);
        let mut path = [0xa5; abi::MAX_PATH_BYTES];
        assert_eq!(
            path_in(&memory, u64::MAX, 0, &mut path),
            Err(Errno::NoEntry)
        );
        assert_eq!(
            path_in(&memory, u64::MAX, 257, &mut path),
            Err(Errno::NameTooLong)
        );
        assert_eq!(path_in(&memory, source, 16, &mut path), Err(Errno::Fault));
        assert_eq!(path, [0xa5; abi::MAX_PATH_BYTES]);
        memory.denied.set(None);
        let path_address = memory.store(0, b"/tmp\0suffix");
        assert_eq!(
            path_in(&memory, path_address, 11, &mut path),
            Err(Errno::InvalidArgument)
        );
        let path_address = memory.store(0, &[b'x'; abi::MAX_PATH_BYTES]);
        assert_eq!(path_in(&memory, path_address, 256, &mut path), Ok(256));
        assert_eq!(memory.writes.get(), 0);
    }
}

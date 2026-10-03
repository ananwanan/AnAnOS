//! Bounded CPU0 process metadata and cooperative scheduling policy.
//!
//! This table owns no pages, descriptors or architectural context. The runtime
//! must retain those resources until it has switched away from a process root.
//! Reservations become runnable only after the runtime commits construction;
//! wait and orphan cleanup similarly require an explicit metadata commit.

pub const MAX_PROCESSES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessState {
    Reserved,
    Ready,
    Running,
    Waiting(Option<u32>),
    /// The runtime supplies the Unix wait status, not the raw exit argument.
    Zombie(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub parent: Option<u32>,
    pub state: ProcessState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessError {
    InvalidSlot,
    InvalidPid,
    InvalidState,
    NoChild,
    NoSlots,
    PidExhausted,
}

pub struct ProcessTable {
    entries: [Option<Process>; MAX_PROCESSES],
    next_pid: u32,
    next_slot: usize,
}

impl ProcessTable {
    pub const fn new() -> Self {
        Self {
            entries: [None; MAX_PROCESSES],
            next_pid: 1,
            next_slot: 0,
        }
    }

    pub fn process(&self, slot: usize) -> Option<Process> {
        self.entries.get(slot).copied().flatten()
    }

    pub fn getpid(&self, slot: usize) -> Result<u32, ProcessError> {
        Ok(self.entry(slot)?.pid)
    }

    pub fn slot_for_pid(&self, pid: u32) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.pid == pid))
    }

    /// Reserve metadata before allocating process resources. PIDs are never
    /// reused, including after rollback, and zero is never a valid PID.
    pub fn reserve_spawn(&mut self, parent: Option<u32>) -> Result<(usize, u32), ProcessError> {
        if let Some(parent) = parent {
            self.live_parent(parent)?;
        }
        let slot = self
            .entries
            .iter()
            .position(Option::is_none)
            .ok_or(ProcessError::NoSlots)?;
        if self.next_pid == 0 {
            return Err(ProcessError::PidExhausted);
        }
        let pid = self.next_pid;
        self.next_pid = self.next_pid.checked_add(1).unwrap_or(0);
        self.entries[slot] = Some(Process {
            pid,
            parent,
            state: ProcessState::Reserved,
        });
        Ok((slot, pid))
    }

    pub fn commit_spawn(&mut self, slot: usize) -> Result<(), ProcessError> {
        let entry = self.entry_mut(slot)?;
        if entry.state != ProcessState::Reserved {
            return Err(ProcessError::InvalidState);
        }
        entry.state = ProcessState::Ready;
        Ok(())
    }

    /// Call only after releasing all resources from the failed construction.
    pub fn rollback_spawn(&mut self, slot: usize) -> Result<(), ProcessError> {
        if self.entry(slot)?.state != ProcessState::Reserved {
            return Err(ProcessError::InvalidState);
        }
        self.entries[slot] = None;
        Ok(())
    }

    pub fn mark_running(&mut self, slot: usize) -> Result<(), ProcessError> {
        if self.entry(slot)?.state != ProcessState::Ready
            || self
                .entries
                .iter()
                .flatten()
                .any(|entry| entry.state == ProcessState::Running)
        {
            return Err(ProcessError::InvalidState);
        }
        self.entry_mut(slot)?.state = ProcessState::Running;
        Ok(())
    }

    pub fn yield_ready(&mut self, slot: usize) -> Result<(), ProcessError> {
        let entry = self.entry_mut(slot)?;
        if entry.state != ProcessState::Running {
            return Err(ProcessError::InvalidState);
        }
        entry.state = ProcessState::Ready;
        Ok(())
    }

    /// Select ready slots fairly. Selection does not mark a slot running, so
    /// the runtime can install its root/context before committing that state.
    pub fn next_ready(&mut self) -> Option<usize> {
        for offset in 0..MAX_PROCESSES {
            let slot = (self.next_slot + offset) % MAX_PROCESSES;
            if self.entries[slot].is_some_and(|entry| entry.state == ProcessState::Ready) {
                self.next_slot = (slot + 1) % MAX_PROCESSES;
                return Some(slot);
            }
        }
        None
    }

    /// Record termination and wake a parent waiting for this child. Children
    /// become parentless on parent exit. A parentless zombie remains occupied
    /// until the runtime frees its resources and calls `reap_orphan`.
    pub fn exit(&mut self, slot: usize, status: i32) -> Result<(), ProcessError> {
        let process = *self.entry(slot)?;
        if matches!(
            process.state,
            ProcessState::Reserved | ProcessState::Zombie(_)
        ) {
            return Err(ProcessError::InvalidState);
        }
        self.entry_mut(slot)?.state = ProcessState::Zombie(status);
        for entry in self.entries.iter_mut().flatten() {
            if entry.parent == Some(process.pid) {
                entry.parent = None;
            }
            if Some(entry.pid) == process.parent
                && matches!(entry.state, ProcessState::Waiting(target)
                    if target.is_none() || target == Some(process.pid))
            {
                entry.state = ProcessState::Ready;
            }
        }
        Ok(())
    }

    /// Peek a completed child without reaping it. `Ok(None)` means matching
    /// children exist but are still live. Validate/copy the status to userspace
    /// and release runtime resources before calling `commit_wait`.
    pub fn wait(
        &self,
        parent: u32,
        pid: Option<u32>,
    ) -> Result<Option<(u32, i32, usize)>, ProcessError> {
        self.live_parent(parent)?;
        if pid == Some(0) {
            return Err(ProcessError::InvalidPid);
        }
        let mut has_child = false;
        for (slot, entry) in self.entries.iter().enumerate() {
            let Some(entry) = entry else { continue };
            if entry.parent != Some(parent)
                || pid.is_some_and(|pid| entry.pid != pid)
                || entry.state == ProcessState::Reserved
            {
                continue;
            }
            has_child = true;
            if let ProcessState::Zombie(status) = entry.state {
                return Ok(Some((entry.pid, status, slot)));
            }
        }
        if has_child {
            Ok(None)
        } else {
            Err(ProcessError::NoChild)
        }
    }

    pub fn arm_wait(&mut self, parent: u32, pid: Option<u32>) -> Result<(), ProcessError> {
        if self.wait(parent, pid)?.is_some() {
            return Err(ProcessError::InvalidState);
        }
        let slot = self.slot_for_pid(parent).ok_or(ProcessError::InvalidPid)?;
        let entry = self.entry_mut(slot)?;
        if entry.state != ProcessState::Running {
            return Err(ProcessError::InvalidState);
        }
        entry.state = ProcessState::Waiting(pid);
        Ok(())
    }

    pub fn commit_wait(&mut self, parent: u32, slot: usize) -> Result<(), ProcessError> {
        self.live_parent(parent)?;
        let child = self.entry(slot)?;
        if child.parent != Some(parent) || !matches!(child.state, ProcessState::Zombie(_)) {
            return Err(ProcessError::InvalidState);
        }
        self.entries[slot] = None;
        Ok(())
    }

    pub fn next_orphan_zombie(&self) -> Option<usize> {
        self.entries.iter().position(|entry| {
            entry.is_some_and(|entry| {
                entry.parent.is_none() && matches!(entry.state, ProcessState::Zombie(_))
            })
        })
    }

    /// Call only after root switching/TLBI and runtime resource reclamation.
    pub fn reap_orphan(&mut self, slot: usize) -> Result<(), ProcessError> {
        let entry = self.entry(slot)?;
        if entry.parent.is_some() || !matches!(entry.state, ProcessState::Zombie(_)) {
            return Err(ProcessError::InvalidState);
        }
        self.entries[slot] = None;
        Ok(())
    }

    /// Terminal runner failure/shutdown only, after every root, page and FD
    /// has been released. Also removes incomplete construction reservations;
    /// retaining next_pid prevents PID reuse if the owner later starts again.
    pub fn clear_after_reclaim(&mut self) {
        self.entries.fill(None);
        self.next_slot = 0;
    }

    fn live_parent(&self, pid: u32) -> Result<&Process, ProcessError> {
        let slot = self.slot_for_pid(pid).ok_or(ProcessError::InvalidPid)?;
        let entry = self.entry(slot)?;
        if matches!(
            entry.state,
            ProcessState::Reserved | ProcessState::Zombie(_)
        ) {
            return Err(ProcessError::InvalidState);
        }
        Ok(entry)
    }

    fn entry(&self, slot: usize) -> Result<&Process, ProcessError> {
        self.entries
            .get(slot)
            .and_then(Option::as_ref)
            .ok_or(ProcessError::InvalidSlot)
    }

    fn entry_mut(&mut self, slot: usize) -> Result<&mut Process, ProcessError> {
        self.entries
            .get_mut(slot)
            .and_then(Option::as_mut)
            .ok_or(ProcessError::InvalidSlot)
    }
}

impl Default for ProcessTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn(table: &mut ProcessTable, parent: Option<u32>) -> (usize, u32) {
        let (slot, pid) = table.reserve_spawn(parent).unwrap();
        assert_eq!(table.process(slot).unwrap().state, ProcessState::Reserved);
        table.commit_spawn(slot).unwrap();
        (slot, pid)
    }

    #[test]
    fn round_robin_keeps_one_running_process_and_skips_incomplete_or_waiting_slots() {
        let mut table = ProcessTable::new();
        let (parent, parent_pid) = spawn(&mut table, None);
        let (first, first_pid) = spawn(&mut table, Some(parent_pid));
        let (second, _) = spawn(&mut table, Some(parent_pid));
        let (reserved, _) = table.reserve_spawn(None).unwrap();
        assert_eq!(table.next_ready(), Some(parent));
        table.mark_running(parent).unwrap();
        assert_eq!(table.mark_running(first), Err(ProcessError::InvalidState));
        table.arm_wait(parent_pid, Some(first_pid)).unwrap();
        for expected in [first, second, first, second] {
            assert_eq!(table.next_ready(), Some(expected));
            table.mark_running(expected).unwrap();
            table.yield_ready(expected).unwrap();
        }
        table.rollback_spawn(reserved).unwrap();
        assert_eq!(
            table.process(parent).unwrap().state,
            ProcessState::Waiting(Some(first_pid))
        );
    }

    #[test]
    fn failed_spawn_restores_slots_without_reusing_pids_or_exposing_a_child() {
        let mut table = ProcessTable::new();
        let (_, parent_pid) = spawn(&mut table, None);
        let (slot, pid) = table.reserve_spawn(Some(parent_pid)).unwrap();
        assert_eq!(table.wait(parent_pid, None), Err(ProcessError::NoChild));
        table.rollback_spawn(slot).unwrap();
        let (next_slot, next_pid) = spawn(&mut table, Some(parent_pid));
        assert_eq!(next_slot, slot);
        assert!(next_pid > pid);
        assert_eq!(
            table.rollback_spawn(next_slot),
            Err(ProcessError::InvalidState)
        );
        while table.reserve_spawn(None).is_ok() {}
        assert_eq!(table.reserve_spawn(None), Err(ProcessError::NoSlots));
    }

    #[test]
    fn wait_peek_survives_a_failed_user_copy_and_commit_reaps_exactly_once() {
        let mut table = ProcessTable::new();
        let (parent, parent_pid) = spawn(&mut table, None);
        let (child, child_pid) = spawn(&mut table, Some(parent_pid));
        table.mark_running(parent).unwrap();
        assert_eq!(table.wait(parent_pid, Some(child_pid)), Ok(None));
        table.arm_wait(parent_pid, Some(child_pid)).unwrap();
        table.mark_running(child).unwrap();
        let status = 73 << 8;
        table.exit(child, status).unwrap();
        assert_eq!(table.process(parent).unwrap().state, ProcessState::Ready);
        let completed = Ok(Some((child_pid, status, child)));
        assert_eq!(table.wait(parent_pid, Some(child_pid)), completed);
        // No commit models an EFAULT while writing the user's status pointer.
        assert_eq!(table.wait(parent_pid, Some(child_pid)), completed);
        assert_eq!(table.reap_orphan(child), Err(ProcessError::InvalidState));
        table.commit_wait(parent_pid, child).unwrap();
        assert_eq!(
            table.wait(parent_pid, Some(child_pid)),
            Err(ProcessError::NoChild)
        );
        assert_eq!(
            table.commit_wait(parent_pid, child),
            Err(ProcessError::InvalidSlot)
        );
    }

    #[test]
    fn unrelated_child_exit_does_not_wake_a_targeted_wait_but_any_child_does() {
        let mut table = ProcessTable::new();
        let (parent, parent_pid) = spawn(&mut table, None);
        let (first, first_pid) = spawn(&mut table, Some(parent_pid));
        let (second, _) = spawn(&mut table, Some(parent_pid));
        table.mark_running(parent).unwrap();
        table.arm_wait(parent_pid, Some(first_pid)).unwrap();
        table.exit(second, 5 << 8).unwrap();
        assert_eq!(
            table.process(parent).unwrap().state,
            ProcessState::Waiting(Some(first_pid))
        );
        table.exit(first, 6 << 8).unwrap();
        assert_eq!(table.process(parent).unwrap().state, ProcessState::Ready);
        assert_eq!(
            table.wait(parent_pid, None),
            Ok(Some((first_pid, 6 << 8, first)))
        );
        table.commit_wait(parent_pid, first).unwrap();
        assert_eq!(table.wait(parent_pid, None).unwrap().unwrap().2, second);
        table.commit_wait(parent_pid, second).unwrap();
        let (third, _) = spawn(&mut table, Some(parent_pid));
        table.mark_running(parent).unwrap();
        table.arm_wait(parent_pid, None).unwrap();
        table.exit(third, 0).unwrap();
        assert_eq!(table.process(parent).unwrap().state, ProcessState::Ready);
    }

    #[test]
    fn parent_exit_detaches_live_and_zombie_children_for_explicit_resource_cleanup() {
        let mut table = ProcessTable::new();
        let (parent, parent_pid) = spawn(&mut table, None);
        let (first, _) = spawn(&mut table, Some(parent_pid));
        let (second, _) = spawn(&mut table, Some(parent_pid));
        table.exit(first, 0).unwrap();
        assert_eq!(table.next_orphan_zombie(), None);
        table.exit(parent, 1 << 8).unwrap();
        assert_eq!(table.process(first).unwrap().parent, None);
        assert_eq!(table.process(second).unwrap().parent, None);
        assert_eq!(table.reap_orphan(second), Err(ProcessError::InvalidState));
        assert_eq!(table.next_orphan_zombie(), Some(parent));
        table.reap_orphan(parent).unwrap();
        assert_eq!(table.next_orphan_zombie(), Some(first));
        table.reap_orphan(first).unwrap();
        table.exit(second, 2 << 8).unwrap();
        table.reap_orphan(second).unwrap();
        assert_eq!(table.next_orphan_zombie(), None);
        for _ in 0..MAX_PROCESSES {
            spawn(&mut table, None);
        }
        assert_eq!(table.reserve_spawn(None), Err(ProcessError::NoSlots));
    }

    #[test]
    fn invalid_operations_and_pid_exhaustion_leave_existing_entries_unchanged() {
        let mut table = ProcessTable::new();
        assert_eq!(table.reserve_spawn(Some(1)), Err(ProcessError::InvalidPid));
        let (slot, pid) = table.reserve_spawn(None).unwrap();
        assert_eq!(table.mark_running(slot), Err(ProcessError::InvalidState));
        assert_eq!(table.exit(slot, 0), Err(ProcessError::InvalidState));
        assert_eq!(
            table.reserve_spawn(Some(pid)),
            Err(ProcessError::InvalidState)
        );
        table.commit_spawn(slot).unwrap();
        assert_eq!(table.yield_ready(slot), Err(ProcessError::InvalidState));
        assert_eq!(table.wait(pid, Some(0)), Err(ProcessError::InvalidPid));
        assert_eq!(table.arm_wait(pid, None), Err(ProcessError::NoChild));
        table.next_pid = u32::MAX;
        let (last_slot, last_pid) = table.reserve_spawn(None).unwrap();
        assert_eq!(last_pid, u32::MAX);
        assert_eq!(table.reserve_spawn(None), Err(ProcessError::PidExhausted));
        table.rollback_spawn(last_slot).unwrap();
        assert_eq!(table.reserve_spawn(None), Err(ProcessError::PidExhausted));
        assert_eq!(table.getpid(slot), Ok(pid));
        assert_eq!(table.getpid(MAX_PROCESSES), Err(ProcessError::InvalidSlot));
    }
}

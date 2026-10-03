//! Bounded EL1 timer-IRQ readiness check before any task root is installed.
//! Polling keeps IRQ enabled and holds no task borrow, lock or allocation.

pub const REQUIRED_TIMER_EVENTS: u64 = 2;
pub const IRQ_WAIT_SECONDS: u64 = 3;

// A Pi 4/Cortex-A72 sanity fail-safe for a frozen physical counter. This is a
// count of identical reads, not a calibrated time delay. A progressing counter
// uses CNTFRQ-based elapsed time exclusively; very slow synthetic clocks are
// outside this fast hardware-counter bring-up contract.
const MAX_UNCHANGED_COUNTER_READS: usize = 1_000_000;

pub trait TimerSource {
    fn frequency(&mut self) -> u64;
    fn counter(&mut self) -> u64;
    fn timer_ticks(&mut self) -> u64;
    fn irq_masked(&mut self) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreflightError {
    IrqMasked,
    InvalidFrequency,
    TimerTimeout,
    CounterStalled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub timer_events: u64,
    pub elapsed_counter_ticks: u64,
}

/// Require two new handler events within three physical-counter seconds. Old
/// ticks cannot pass this gate; two events exercise the handler's one-shot
/// timer rearm as well as initial delivery. This proves EL1 delivery only.
/// Failure neither acknowledges the GIC nor reconfigures the timer/IRQ mask.
pub fn wait_for_timer_irq(source: &mut impl TimerSource) -> Result<Report, PreflightError> {
    wait_with_stall_limit(source, MAX_UNCHANGED_COUNTER_READS)
}

fn wait_with_stall_limit(
    source: &mut impl TimerSource,
    stall_limit: usize,
) -> Result<Report, PreflightError> {
    if source.irq_masked() {
        return Err(PreflightError::IrqMasked);
    }
    let frequency = source.frequency();
    let budget = frequency
        .checked_mul(IRQ_WAIT_SECONDS)
        // wrapping elapsed comparisons are unambiguous within half a range.
        .filter(|ticks| *ticks != 0 && *ticks < (1 << 63))
        .ok_or(PreflightError::InvalidFrequency)?;
    let initial_ticks = source.timer_ticks();
    let start = source.counter();
    let mut previous_counter = start;
    let mut unchanged_reads = 0;
    loop {
        if source.irq_masked() {
            return Err(PreflightError::IrqMasked);
        }
        let timer_events = source.timer_ticks().wrapping_sub(initial_ticks);
        // Read time after events so a late IRQ cannot pass using an earlier,
        // pre-deadline counter sample. Deadline failure precedes success.
        let now = source.counter();
        let elapsed_counter_ticks = now.wrapping_sub(start);
        if elapsed_counter_ticks >= budget {
            return Err(PreflightError::TimerTimeout);
        }
        if timer_events >= REQUIRED_TIMER_EVENTS && elapsed_counter_ticks != 0 {
            return Ok(Report {
                timer_events,
                elapsed_counter_ticks,
            });
        }
        if now == previous_counter {
            unchanged_reads += 1;
            if unchanged_reads >= stall_limit {
                return Err(PreflightError::CounterStalled);
            }
        } else {
            unchanged_reads = 0;
        }
        previous_counter = now;
        core::hint::spin_loop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    struct Sample {
        counter: u64,
        ticks: u64,
        masked: bool,
    }

    struct Source<'a> {
        frequency: u64,
        samples: &'a [Sample],
        index: usize,
    }
    impl Source<'_> {
        fn sample(&self) -> Sample {
            self.samples[self.index.min(self.samples.len() - 1)]
        }
    }
    impl TimerSource for Source<'_> {
        fn frequency(&mut self) -> u64 {
            self.frequency
        }
        fn counter(&mut self) -> u64 {
            let counter = self.sample().counter;
            self.index += 1;
            counter
        }
        fn timer_ticks(&mut self) -> u64 {
            self.sample().ticks
        }
        fn irq_masked(&mut self) -> bool {
            self.sample().masked
        }
    }

    fn check(frequency: u64, samples: &[(u64, u64, bool)]) -> Result<Report, PreflightError> {
        let samples: std::vec::Vec<_> = samples
            .iter()
            .map(|&(counter, ticks, masked)| Sample {
                counter,
                ticks,
                masked,
            })
            .collect();
        wait_with_stall_limit(
            &mut Source {
                frequency,
                samples: &samples,
                index: 0,
            },
            3,
        )
    }

    #[test]
    fn two_new_events_pass_but_stale_ticks_and_one_event_do_not() {
        assert_eq!(
            check(
                10,
                &[(100, 99, false), (110, 100, false), (120, 101, false)]
            ),
            Ok(Report {
                timer_events: 2,
                elapsed_counter_ticks: 20
            })
        );
        for ticks in [99, 100] {
            assert_eq!(
                check(
                    10,
                    &[(100, 99, false), (110, ticks, false), (130, ticks, false)]
                ),
                Err(PreflightError::TimerTimeout)
            );
        }
    }

    #[test]
    fn deadline_takes_precedence_over_a_second_event() {
        for now in [130, 131] {
            assert_eq!(
                check(10, &[(100, 0, false), (now, 2, false)]),
                Err(PreflightError::TimerTimeout)
            );
        }
        assert!(check(10, &[(100, 0, false), (129, 2, false)]).is_ok());
    }

    #[test]
    fn masked_irq_at_entry_or_during_polling_cannot_pass() {
        assert_eq!(check(10, &[(0, 0, true)]), Err(PreflightError::IrqMasked));
        assert_eq!(
            check(10, &[(0, 0, false), (1, 2, true)]),
            Err(PreflightError::IrqMasked)
        );
    }

    #[test]
    fn zero_overflowing_and_ambiguous_frequency_are_rejected() {
        for frequency in [0, u64::MAX, u64::MAX / IRQ_WAIT_SECONDS, 1 << 62] {
            assert_eq!(
                check(frequency, &[(0, 0, false)]),
                Err(PreflightError::InvalidFrequency)
            );
        }
    }

    #[test]
    fn counter_and_irq_tick_wrap_preserve_elapsed_and_event_counts() {
        assert_eq!(
            check(10, &[(u64::MAX - 10, u64::MAX - 1, false), (9, 0, false)]),
            Ok(Report {
                timer_events: 2,
                elapsed_counter_ticks: 20
            })
        );
    }

    #[test]
    fn frozen_counter_fails_without_waiting_for_an_unreachable_deadline() {
        assert_eq!(
            check(10, &[(100, 0, false)]),
            Err(PreflightError::CounterStalled)
        );
        // Implausible IRQ progress must not turn a stationary clock into success.
        assert_eq!(
            check(10, &[(100, 0, false), (100, 2, false)]),
            Err(PreflightError::CounterStalled)
        );
    }

    #[test]
    fn counter_progress_resets_the_stall_read_count() {
        assert_eq!(
            check(
                10,
                &[
                    (100, 0, false),
                    (100, 0, false),
                    (100, 0, false),
                    (101, 1, false),
                    (101, 1, false),
                    (101, 1, false),
                    (102, 2, false),
                ]
            ),
            Ok(Report {
                timer_events: 2,
                elapsed_counter_ticks: 2
            })
        );
    }

    #[test]
    fn production_entry_uses_the_same_readiness_policy() {
        let samples = [
            Sample {
                counter: 100,
                ticks: 1,
                masked: false,
            },
            Sample {
                counter: 110,
                ticks: 2,
                masked: false,
            },
            Sample {
                counter: 120,
                ticks: 3,
                masked: false,
            },
        ];
        assert_eq!(
            wait_for_timer_irq(&mut Source {
                frequency: 10,
                samples: &samples,
                index: 0
            }),
            Ok(Report {
                timer_events: 2,
                elapsed_counter_ticks: 20
            })
        );
    }
}

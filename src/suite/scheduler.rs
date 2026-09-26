use super::{Clock, Found};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

/// A `Waker` whose every operation is a no-op.
fn noop_waker() -> Waker {
    fn clone(_: *const ()) -> RawWaker {
        raw()
    }
    fn noop(_: *const ()) {}
    fn raw() -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    // SAFETY: every function in the vtable ignores the data pointer, so the
    // null pointer is never dereferenced, and `clone` returns a `RawWaker`
    // built from the same vtable and the same null pointer.
    unsafe { Waker::from_raw(raw()) }
}

/// Drive one future to completion, polling it in a tight loop.
pub(crate) fn block_on<F: Future>(clock: &Clock, future: F) -> F::Output {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        clock.begin_poll();
        let polled = future.as_mut().poll(&mut cx);
        clock.end_poll();
        if let Poll::Ready(value) = polled {
            return value;
        }
    }
}

/// One benchmark in flight.
pub(crate) struct Task<'a> {
    future: Pin<Box<dyn Future<Output = Found> + 'a>>,
    clock: Rc<Clock>,
    result: Option<Found>,
}

/// Round-robin over a set of benchmarks, one sample each per round.
pub(crate) struct Scheduler<'a> {
    tasks: Vec<Task<'a>>,
    /// Xorshift state for the per-round starting offset.
    seed: u64,
}

impl<'a> Scheduler<'a> {
    pub(crate) fn new(seed: u64) -> Self {
        Scheduler {
            tasks: Vec::new(),
            // Zero is a fixed point of xorshift, and would leave every round
            // starting at position zero.
            seed: seed | 1,
        }
    }

    pub(crate) fn push(
        &mut self,
        clock: Rc<Clock>,
        future: Pin<Box<dyn Future<Output = Found> + 'a>>,
    ) {
        self.tasks.push(Task {
            future,
            clock,
            result: None,
        });
    }

    fn next_rand(&mut self) -> u64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        self.seed
    }

    /// Put `live` into a fresh uniformly random order, Fisher-Yates.
    fn shuffle(&mut self, live: &mut [usize]) {
        for i in (1..live.len()).rev() {
            let j = (self.next_rand() % (i as u64 + 1)) as usize;
            live.swap(i, j);
        }
    }

    /// Poll every benchmark once per round until all of them finish.
    pub(crate) fn run(mut self) -> Vec<Found> {
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut live: Vec<usize> = (0..self.tasks.len()).collect();
        while !live.is_empty() {
            self.shuffle(&mut live);
            for &i in &live {
                let task = &mut self.tasks[i];
                task.clock.begin_poll();
                if let Poll::Ready(value) = task.future.as_mut().poll(&mut cx) {
                    task.result = Some(value);
                }
                task.clock.end_poll();
            }
            live.retain(|&i| self.tasks[i].result.is_none());
        }
        self.tasks
            .into_iter()
            .map(|t| t.result.expect("every task finished"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::Stats;
    use std::cell::RefCell;
    use std::time::Duration;

    fn nothing() -> Found {
        Found::Stats(Stats {
            ns_per_iter: 0.0,
            std_error: 0.0,
            iterations: 0,
            samples: 0,
            hit_limit: false,
            untrustworthy: false,
        })
    }

    async fn scripted(
        id: usize,
        yields: usize,
        log: Rc<RefCell<Vec<usize>>>,
        clock: Rc<Clock>,
    ) -> Found {
        for _ in 0..yields {
            log.borrow_mut().push(id);
            clock.yield_now().await;
        }
        log.borrow_mut().push(id);
        nothing()
    }

    fn scheduler_of(yields: &[usize], seed: u64) -> (Scheduler<'static>, Rc<RefCell<Vec<usize>>>) {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut scheduler = Scheduler::new(seed);
        for (id, &count) in yields.iter().enumerate() {
            let clock = Rc::new(Clock::new(Duration::from_secs(3600)));
            let future = scripted(id, count, log.clone(), clock.clone());
            scheduler.push(clock, Box::pin(future));
        }
        (scheduler, log)
    }

    #[test]
    fn every_round_polls_everyone_exactly_once() {
        const N: usize = 5;
        const ROUNDS: usize = 4;
        let (scheduler, log) = scheduler_of(&[ROUNDS; N], 0x243f_6a88_85a3_08d3);
        scheduler.run();
        let log = log.borrow();
        assert_eq!(log.len(), N * (ROUNDS + 1));
        for round in log.chunks(N) {
            let mut seen = round.to_vec();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), N, "a round polled someone twice: {round:?}");
        }
    }

    #[test]
    fn the_starting_position_moves_between_rounds() {
        let (scheduler, log) = scheduler_of(&[20; 4], 0x9e37_79b9_7f4a_7c15);
        scheduler.run();
        let log = log.borrow();
        let firsts: Vec<usize> = log.chunks(4).map(|round| round[0]).collect();
        let distinct = {
            let mut firsts = firsts.clone();
            firsts.sort_unstable();
            firsts.dedup();
            firsts.len()
        };
        assert!(distinct > 1);
    }

    #[test]
    fn the_neighbour_order_varies_too() {
        const N: usize = 3;
        let (scheduler, log) = scheduler_of(&[40; N], 0x2545_f491_4f6c_dd1d);
        scheduler.run();
        let log = log.borrow();
        let mut predecessors: Vec<usize> = log
            .windows(2)
            .filter(|window| window[1] == 0)
            .map(|window| window[0])
            .collect();
        predecessors.sort_unstable();
        predecessors.dedup();
        assert!(predecessors.contains(&1) && predecessors.contains(&2));
    }

    #[test]
    fn retiring_early_does_not_skip_a_neighbour() {
        let (scheduler, log) = scheduler_of(&[0, 3, 7], 0x2545_f491_4f6c_dd1d);
        scheduler.run();
        let log = log.borrow();
        let polls = |id: usize| log.iter().filter(|&&value| value == id).count();
        assert_eq!(polls(0), 1);
        assert_eq!(polls(1), 4);
        assert_eq!(polls(2), 8);
    }

    #[test]
    fn an_empty_scheduler_finishes() {
        let (scheduler, log) = scheduler_of(&[], 1);
        scheduler.run();
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn block_on_drives_a_yielding_future() {
        let clock = Rc::new(Clock::new(Duration::from_secs(3600)));
        let inner = clock.clone();
        let rounds = block_on(&clock, async move {
            let mut rounds = 0;
            while rounds < 5 {
                rounds += 1;
                inner.yield_now().await;
            }
            rounds
        });
        assert_eq!(rounds, 5);
    }
}

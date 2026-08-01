use std::num::NonZeroUsize;
use std::thread;

use rayon::prelude::*;
use rayon::{ThreadPool, ThreadPoolBuilder};
use thiserror::Error;
use tokio::sync::oneshot;

/// Bounded worker counts for the asynchronous and compute execution planes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Parallelism {
    io_workers: NonZeroUsize,
    compute_workers: NonZeroUsize,
    max_processes: NonZeroUsize,
    max_filesystem_ops: NonZeroUsize,
}

impl Parallelism {
    /// Detects limits from the CPU parallelism made available by the host.
    #[must_use]
    pub fn detected() -> Self {
        let available = thread::available_parallelism()
            .map(NonZeroUsize::get)
            .unwrap_or(1);
        Self::for_available_cpus(available)
    }

    /// Derives deterministic defaults from an explicit available CPU count.
    ///
    /// This is public so launchers and benchmarks can preview the effective
    /// limits without mutating global process state.
    #[must_use]
    pub fn for_available_cpus(available: usize) -> Self {
        let available = available.max(1);
        let io_workers = if available == 1 {
            1
        } else {
            available.div_ceil(4).clamp(2, 8)
        };

        Self {
            io_workers: non_zero(io_workers),
            compute_workers: non_zero(available),
            max_processes: non_zero(available),
            max_filesystem_ops: non_zero(available.saturating_mul(2)),
        }
    }

    #[must_use]
    pub const fn io_workers(self) -> NonZeroUsize {
        self.io_workers
    }

    #[must_use]
    pub const fn compute_workers(self) -> NonZeroUsize {
        self.compute_workers
    }

    #[must_use]
    pub const fn max_processes(self) -> NonZeroUsize {
        self.max_processes
    }

    #[must_use]
    pub const fn max_filesystem_ops(self) -> NonZeroUsize {
        self.max_filesystem_ops
    }
}

const fn non_zero(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => unreachable!(),
    }
}

/// A fixed, named work-stealing pool for splittable CPU operations.
pub struct ComputePool {
    pool: ThreadPool,
    workers: NonZeroUsize,
}

impl ComputePool {
    /// Builds the compute plane with exactly the configured number of workers.
    pub fn new(parallelism: Parallelism) -> Result<Self, ParallelismError> {
        let workers = parallelism.compute_workers();
        let pool = ThreadPoolBuilder::new()
            .num_threads(workers.get())
            .thread_name(|index| format!("ash-cpu-{index}"))
            .build()?;
        Ok(Self { pool, workers })
    }

    #[must_use]
    pub const fn workers(&self) -> NonZeroUsize {
        self.workers
    }

    /// Maps indexed input in parallel and returns results in input order.
    ///
    /// Rayon may schedule partitions in any order, but an indexed parallel
    /// iterator collects by index. Operation-specific code can then apply its
    /// stable key merge before ASON encoding.
    pub fn map_ordered<T, U, F>(&self, input: &[T], map: F) -> Vec<U>
    where
        T: Sync,
        U: Send,
        F: Fn(&T) -> U + Send + Sync,
    {
        self.pool.install(|| input.par_iter().map(map).collect())
    }

    /// Runs one splittable CPU closure on the dedicated compute plane.
    pub fn install<R, F>(&self, operation: F) -> R
    where
        R: Send,
        F: FnOnce() -> R + Send,
    {
        self.pool.install(operation)
    }

    /// Schedules owned CPU work without blocking a Tokio I/O worker.
    pub async fn run<R, F>(&self, operation: F) -> Result<R, ParallelismError>
    where
        R: Send + 'static,
        F: FnOnce() -> R + Send + 'static,
    {
        let (sender, receiver) = oneshot::channel();
        self.pool.spawn(move || {
            let _ = sender.send(operation());
        });
        receiver.await.map_err(|_| ParallelismError::WorkerLost)
    }

    /// Maps owned indexed input on the compute plane and preserves input order.
    pub async fn map_ordered_owned<T, U, F>(
        &self,
        input: Vec<T>,
        map: F,
    ) -> Result<Vec<U>, ParallelismError>
    where
        T: Send + Sync + 'static,
        U: Send + 'static,
        F: Fn(&T) -> U + Send + Sync + 'static,
    {
        self.run(move || input.par_iter().map(map).collect()).await
    }
}

#[derive(Debug, Error)]
pub enum ParallelismError {
    #[error("failed to create the ash compute pool: {0}")]
    Build(#[from] rayon::ThreadPoolBuildError),
    #[error("ash compute worker ended before returning its result")]
    WorkerLost,
}

#[cfg(test)]
mod tests {
    use super::{ComputePool, Parallelism};

    #[test]
    fn defaults_use_all_available_cpus_for_compute() {
        let one = Parallelism::for_available_cpus(0);
        assert_eq!(one.compute_workers().get(), 1);
        assert_eq!(one.io_workers().get(), 1);

        let many = Parallelism::for_available_cpus(32);
        assert_eq!(many.compute_workers().get(), 32);
        assert_eq!(many.io_workers().get(), 8);
        assert_eq!(many.max_processes().get(), 32);
        assert_eq!(many.max_filesystem_ops().get(), 64);
    }

    #[test]
    fn parallel_map_preserves_input_order() {
        let pool = ComputePool::new(Parallelism::for_available_cpus(4))
            .expect("the test compute pool should build");
        let input: Vec<u64> = (0..10_000).rev().collect();

        let output = pool.map_ordered(&input, |value| value * value);
        let expected: Vec<u64> = input.iter().map(|value| value * value).collect();

        assert_eq!(pool.workers().get(), 4);
        assert_eq!(output, expected);
    }
}

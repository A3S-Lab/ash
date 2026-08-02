use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::future::Future;

use futures::stream::{FuturesUnordered, StreamExt};

/// One owned job plus dependency indexes in the submitted stable node order.
pub struct DagNode<J> {
    dependencies: Vec<usize>,
    job: J,
}

impl<J> DagNode<J> {
    #[must_use]
    pub fn new(dependencies: Vec<usize>, job: J) -> Self {
        Self { dependencies, job }
    }
}

/// A completed job and the success bit used for dependency eligibility.
pub struct DagCompletion<R> {
    value: R,
    succeeded: bool,
}

impl<R> DagCompletion<R> {
    #[must_use]
    pub const fn new(value: R, succeeded: bool) -> Self {
        Self { value, succeeded }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum DagOutcome<R> {
    Completed(R),
    Skipped,
}

#[derive(Debug, Eq, PartialEq)]
pub enum DagError<E> {
    InvalidGraph,
    Task(E),
}

impl<E: fmt::Display> fmt::Display for DagError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGraph => formatter.write_str("dependency graph is invalid"),
            Self::Task(error) => error.fmt(formatter),
        }
    }
}

impl<E: Error + 'static> Error for DagError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidGraph => None,
            Self::Task(error) => Some(error),
        }
    }
}

/// Executes every ready job concurrently and returns results in input order.
///
/// A failed job is still a completed result, but its downstream jobs are
/// skipped. Independent branches continue. The graph is fully validated
/// before `run` is called, so invalid input cannot produce side effects. Task
/// errors fail their branch without dropping already-running work; after the
/// graph settles, the error from the lowest input index is returned.
pub async fn execute_dag<J, R, E, F, Fut>(
    nodes: Vec<DagNode<J>>,
    mut run: F,
) -> Result<Vec<DagOutcome<R>>, DagError<E>>
where
    F: FnMut(J) -> Fut,
    Fut: Future<Output = Result<DagCompletion<R>, E>>,
{
    validate(&nodes)?;
    let mut remaining = nodes
        .iter()
        .map(|node| node.dependencies.len())
        .collect::<Vec<_>>();
    let mut dependents = vec![Vec::new(); nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        for dependency in &node.dependencies {
            dependents[*dependency].push(index);
        }
    }
    let mut jobs = nodes
        .into_iter()
        .map(|node| Some(node.job))
        .collect::<Vec<_>>();
    let mut outcomes = (0..jobs.len()).map(|_| None).collect::<Vec<_>>();
    let mut errors = (0..jobs.len()).map(|_| None).collect::<Vec<_>>();
    let mut failed_dependency = vec![false; jobs.len()];
    let mut running = FuturesUnordered::new();
    for (index, count) in remaining.iter().enumerate() {
        if *count == 0 {
            let job = jobs[index].take().ok_or(DagError::InvalidGraph)?;
            running.push(indexed(index, run(job)));
        }
    }

    let mut settled = 0_usize;
    while settled < jobs.len() {
        let Some((index, result)) = running.next().await else {
            return Err(DagError::InvalidGraph);
        };
        let succeeded = match result {
            Ok(completion) => {
                outcomes[index] = Some(DagOutcome::Completed(completion.value));
                completion.succeeded
            }
            Err(error) => {
                errors[index] = Some(error);
                false
            }
        };
        settled += 1;

        let mut terminal = VecDeque::from([(index, succeeded)]);
        while let Some((completed, succeeded)) = terminal.pop_front() {
            for dependent in &dependents[completed] {
                remaining[*dependent] = remaining[*dependent]
                    .checked_sub(1)
                    .ok_or(DagError::InvalidGraph)?;
                failed_dependency[*dependent] |= !succeeded;
                if remaining[*dependent] != 0 {
                    continue;
                }
                if failed_dependency[*dependent] {
                    outcomes[*dependent] = Some(DagOutcome::Skipped);
                    settled += 1;
                    terminal.push_back((*dependent, false));
                } else {
                    let job = jobs[*dependent].take().ok_or(DagError::InvalidGraph)?;
                    running.push(indexed(*dependent, run(job)));
                }
            }
        }
    }

    if let Some(error) = errors.into_iter().flatten().next() {
        return Err(DagError::Task(error));
    }

    outcomes
        .into_iter()
        .map(|outcome| outcome.ok_or(DagError::InvalidGraph))
        .collect()
}

fn validate<J, E>(nodes: &[DagNode<J>]) -> Result<(), DagError<E>> {
    if nodes.is_empty() {
        return Err(DagError::InvalidGraph);
    }
    let mut incoming = Vec::with_capacity(nodes.len());
    let mut dependents = vec![Vec::new(); nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        if !node.dependencies.windows(2).all(|pair| pair[0] < pair[1])
            || node
                .dependencies
                .iter()
                .any(|dependency| *dependency >= nodes.len() || *dependency == index)
        {
            return Err(DagError::InvalidGraph);
        }
        incoming.push(node.dependencies.len());
        for dependency in &node.dependencies {
            dependents[*dependency].push(index);
        }
    }
    let mut ready = incoming
        .iter()
        .enumerate()
        .filter_map(|(index, count)| (*count == 0).then_some(index))
        .collect::<VecDeque<_>>();
    let mut visited = 0_usize;
    while let Some(index) = ready.pop_front() {
        visited += 1;
        for dependent in &dependents[index] {
            incoming[*dependent] -= 1;
            if incoming[*dependent] == 0 {
                ready.push_back(*dependent);
            }
        }
    }
    if visited == nodes.len() {
        Ok(())
    } else {
        Err(DagError::InvalidGraph)
    }
}

async fn indexed<Fut, R, E>(index: usize, future: Fut) -> (usize, Result<DagCompletion<R>, E>)
where
    Fut: Future<Output = Result<DagCompletion<R>, E>>,
{
    (index, future.await)
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use futures::channel::oneshot;
    use tokio::sync::Barrier;

    use super::{DagCompletion, DagError, DagNode, DagOutcome, execute_dag};

    #[tokio::test]
    async fn ready_nodes_overlap_and_failure_skips_only_descendants() {
        let barrier = Arc::new(Barrier::new(2));
        let nodes = vec![
            DagNode::new(vec![], (0, true)),
            DagNode::new(vec![], (1, false)),
            DagNode::new(vec![0], (2, true)),
            DagNode::new(vec![1], (3, true)),
            DagNode::new(vec![], (4, true)),
        ];
        let outcomes = execute_dag(nodes, |(index, succeeds)| {
            let barrier = Arc::clone(&barrier);
            async move {
                if index < 2 {
                    barrier.wait().await;
                }
                Ok::<_, Infallible>(DagCompletion::new(index, succeeds))
            }
        })
        .await
        .expect("schedule");

        assert_eq!(
            outcomes,
            vec![
                DagOutcome::Completed(0),
                DagOutcome::Completed(1),
                DagOutcome::Completed(2),
                DagOutcome::Skipped,
                DagOutcome::Completed(4),
            ]
        );
    }

    #[tokio::test]
    async fn invalid_graph_is_rejected_before_work_starts() {
        let nodes = vec![DagNode::new(vec![1], 0), DagNode::new(vec![0], 1)];
        let result = execute_dag(nodes, |value| async move {
            Ok::<_, Infallible>(DagCompletion::new(value, true))
        })
        .await;
        assert_eq!(result, Err(DagError::InvalidGraph));
    }

    #[tokio::test]
    async fn every_invalid_graph_shape_is_rejected_without_side_effects() {
        let graphs = [
            Vec::new(),
            vec![DagNode::new(vec![0], 0)],
            vec![DagNode::new(vec![1], 0)],
            vec![
                DagNode::new(vec![], 0),
                DagNode::new(vec![], 1),
                DagNode::new(vec![1, 0], 2),
            ],
            vec![DagNode::new(vec![], 0), DagNode::new(vec![0, 0], 1)],
            vec![
                DagNode::new(vec![1], 0),
                DagNode::new(vec![0], 1),
                DagNode::new(vec![], 2),
            ],
        ];
        for nodes in graphs {
            let calls = Arc::new(AtomicUsize::new(0));
            let calls_for_run = Arc::clone(&calls);
            let result = execute_dag(nodes, move |value| {
                calls_for_run.fetch_add(1, Ordering::Relaxed);
                async move { Ok::<_, Infallible>(DagCompletion::new(value, true)) }
            })
            .await;
            assert_eq!(result, Err(DagError::InvalidGraph));
            assert_eq!(calls.load(Ordering::Relaxed), 0);
        }
    }

    #[tokio::test]
    async fn completion_permutations_cannot_change_stable_output() {
        let first = execute_in_order(&[2, 1, 4, 0, 3, 5]).await;
        let second = execute_in_order(&[0, 1, 3, 2, 4, 5]).await;
        let expected = (0..6).map(DagOutcome::Completed).collect::<Vec<_>>();
        assert_eq!(first, expected);
        assert_eq!(second, expected);
    }

    #[tokio::test]
    async fn all_four_node_dags_and_success_masks_match_the_dependency_oracle() {
        const NODES: usize = 4;
        const EDGES: usize = NODES * (NODES - 1) / 2;
        for edge_mask in 0_u32..(1 << EDGES) {
            let mut dependencies = vec![Vec::new(); NODES];
            let mut edge = 0_u32;
            for (node, node_dependencies) in dependencies.iter_mut().enumerate() {
                for dependency in 0..node {
                    if edge_mask & (1 << edge) != 0 {
                        node_dependencies.push(dependency);
                    }
                    edge += 1;
                }
            }
            for success_mask in 0_u32..(1 << NODES) {
                let nodes = dependencies
                    .iter()
                    .enumerate()
                    .map(|(index, node_dependencies)| {
                        DagNode::new(node_dependencies.clone(), (index, success_mask))
                    })
                    .collect();
                let actual = execute_dag(nodes, |(index, mask)| async move {
                    Ok::<_, Infallible>(DagCompletion::new(index, mask & (1 << index) != 0))
                })
                .await
                .expect("generated DAG is valid");

                let mut succeeded = [false; NODES];
                let expected = dependencies
                    .iter()
                    .enumerate()
                    .map(|(index, node_dependencies)| {
                        let runnable = node_dependencies
                            .iter()
                            .all(|dependency| succeeded[*dependency]);
                        if runnable {
                            succeeded[index] = success_mask & (1 << index) != 0;
                            DagOutcome::Completed(index)
                        } else {
                            DagOutcome::Skipped
                        }
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    actual, expected,
                    "edge mask {edge_mask:06b}, success mask {success_mask:04b}"
                );
            }
        }
    }

    #[tokio::test]
    async fn task_errors_drain_independent_work_and_choose_lowest_input_index() {
        let (send_zero, receive_zero) = oneshot::channel();
        let (send_one, receive_one) = oneshot::channel();
        let (send_two, receive_two) = oneshot::channel();
        let receivers = Arc::new(Mutex::new(vec![
            Some(receive_zero),
            Some(receive_one),
            Some(receive_two),
            None,
        ]));
        let completed = Arc::new(AtomicUsize::new(0));
        let scheduler = {
            let receivers = Arc::clone(&receivers);
            let completed = Arc::clone(&completed);
            tokio::spawn(async move {
                execute_dag(
                    vec![
                        DagNode::new(vec![], 0),
                        DagNode::new(vec![], 1),
                        DagNode::new(vec![], 2),
                        DagNode::new(vec![0], 3),
                    ],
                    move |index| {
                        let receiver = receivers.lock().expect("receivers")[index]
                            .take()
                            .expect("started once");
                        let completed = Arc::clone(&completed);
                        async move {
                            receiver.await.expect("release job");
                            completed.fetch_add(1, Ordering::SeqCst);
                            match index {
                                0 => Err("zero"),
                                1 => Err("one"),
                                _ => Ok(DagCompletion::new(index, true)),
                            }
                        }
                    },
                )
                .await
            })
        };

        send_one.send(()).expect("release one");
        wait_for_count(&completed, 1).await;
        send_two.send(()).expect("release independent work");
        wait_for_count(&completed, 2).await;
        send_zero.send(()).expect("release zero last");
        let result = scheduler.await.expect("scheduler task");

        assert_eq!(result, Err(DagError::Task("zero")));
        assert_eq!(completed.load(Ordering::SeqCst), 3);
    }

    async fn execute_in_order(order: &[usize]) -> Vec<DagOutcome<usize>> {
        let mut senders = Vec::new();
        let mut receivers = Vec::new();
        for _ in 0..6 {
            let (sender, receiver) = oneshot::channel();
            senders.push(Some(sender));
            receivers.push(Some(receiver));
        }
        let receivers = Arc::new(Mutex::new(receivers));
        let started = Arc::new(Mutex::new(Vec::new()));
        let scheduler = {
            let receivers = Arc::clone(&receivers);
            let started = Arc::clone(&started);
            tokio::spawn(async move {
                execute_dag(
                    vec![
                        DagNode::new(vec![], 0),
                        DagNode::new(vec![], 1),
                        DagNode::new(vec![], 2),
                        DagNode::new(vec![0, 1], 3),
                        DagNode::new(vec![1], 4),
                        DagNode::new(vec![2, 3, 4], 5),
                    ],
                    move |index| {
                        started.lock().expect("started").push(index);
                        let receiver = receivers.lock().expect("receivers")[index]
                            .take()
                            .expect("started once");
                        async move {
                            receiver.await.expect("release job");
                            Ok::<_, Infallible>(DagCompletion::new(index, true))
                        }
                    },
                )
                .await
            })
        };
        for index in order {
            wait_for_start(&started, *index).await;
            senders[*index]
                .take()
                .expect("sender used once")
                .send(())
                .expect("release ordered job");
            tokio::task::yield_now().await;
        }
        scheduler
            .await
            .expect("scheduler task")
            .expect("valid graph")
    }

    async fn wait_for_start(started: &Mutex<Vec<usize>>, index: usize) {
        for _ in 0..1_000 {
            if started.lock().expect("started").contains(&index) {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("node {index} did not start");
    }

    async fn wait_for_count(completed: &AtomicUsize, expected: usize) {
        for _ in 0..1_000 {
            if completed.load(Ordering::SeqCst) >= expected {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("only {} jobs completed", completed.load(Ordering::SeqCst));
    }
}

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
/// before `run` is called, so invalid input cannot produce side effects.
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
        let completion = result.map_err(DagError::Task)?;
        let succeeded = completion.succeeded;
        outcomes[index] = Some(DagOutcome::Completed(completion.value));
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
    use std::sync::Arc;

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
}

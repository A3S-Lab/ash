use std::collections::HashMap;

use crate::{OperationError, PortableOperations, projection};
use ash_engine::{DagCompletion, DagError, DagNode, DagOutcome, PermitKind, Program, execute_dag};
use ash_protocol::request::{BatchArgs, BatchNode, Budget, Request};
use ash_protocol::response::{
    BatchNodeResult, BatchNodeState, ErrorCode, ErrorRecord, ErrorStage, FinalResponse,
    RESULT_PARTIAL, RESULT_RETAINED, ResultData, RetryClass, Status,
};

struct Executed {
    state: BatchNodeState,
    status: Status,
    encoded: Vec<u8>,
}

pub async fn execute(
    operations: &PortableOperations,
    request: &Request,
    arguments: &BatchArgs,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    let nodes = arguments.nodes();
    let node_count_u32 = u32::try_from(nodes.len()).map_err(|_| OperationError::WorkLimit)?;
    let node_count_u64 = u64::from(node_count_u32);
    let child_tokens = request.budget().tokens() / node_count_u32;
    let child_records = request.budget().records() / node_count_u32;
    let child_output = program.budget().remaining().output_bytes / node_count_u64;
    if child_tokens == 0 || child_records == 0 || child_output == 0 {
        return Err(OperationError::OutputBudget);
    }
    let child_budget = Budget::new(child_tokens, child_records, request.budget().millis())
        .map_err(|_| OperationError::InvalidArgument)?;
    preflight_response(request, nodes, program)?;

    let positions = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id(), index))
        .collect::<HashMap<_, _>>();
    let graph = nodes
        .iter()
        .map(|node| {
            let dependencies = node
                .dependencies()
                .iter()
                .map(|dependency| {
                    positions
                        .get(dependency)
                        .copied()
                        .ok_or(OperationError::InvalidArgument)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(DagNode::new(dependencies, node))
        })
        .collect::<Result<Vec<_>, OperationError>>()?;
    let mut outcomes = execute_dag(graph, |node| async move {
        let outcome = execute_node(operations, node, child_budget, child_output, program).await?;
        let succeeded = outcome.state == BatchNodeState::Succeeded;
        Ok(DagCompletion::new(outcome, succeeded))
    })
    .await
    .map_err(|error| match error {
        DagError::InvalidGraph => OperationError::InvalidArgument,
        DagError::Task(error) => error,
    })?;

    let executed = outcomes
        .iter_mut()
        .filter_map(|outcome| match outcome {
            DagOutcome::Completed(Executed { encoded, .. }) => Some(std::mem::take(encoded)),
            DagOutcome::Skipped => None,
        })
        .collect::<Vec<_>>();
    let aliases = program.store().retain_many(executed)?;
    let mut aliases = aliases.into_iter();
    let mut all_succeeded = true;
    let results = nodes
        .iter()
        .zip(outcomes)
        .map(|(node, outcome)| match outcome {
            DagOutcome::Completed(Executed { state, status, .. }) => {
                all_succeeded &= state == BatchNodeState::Succeeded;
                Ok(BatchNodeResult {
                    id: node.id(),
                    operation: node.arguments().operation(),
                    state,
                    status: Some(status),
                    reference: Some(aliases.next().ok_or(OperationError::InvalidArgument)?),
                })
            }
            DagOutcome::Skipped => {
                all_succeeded = false;
                Ok(BatchNodeResult {
                    id: node.id(),
                    operation: node.arguments().operation(),
                    state: BatchNodeState::Skipped,
                    status: None,
                    reference: None,
                })
            }
        })
        .collect::<Result<Vec<_>, OperationError>>()?;
    if aliases.next().is_some() {
        return Err(OperationError::InvalidArgument);
    }

    let data = ResultData::Batch(results);
    let response = if all_succeeded {
        FinalResponse::success(request.id(), vec![], data, RESULT_RETAINED, None)?
    } else {
        FinalResponse::failure(
            request.id(),
            Status::Failed,
            ErrorRecord {
                code: ErrorCode::BatchFailed,
                retry: RetryClass::Never,
                stage: ErrorStage::Execute,
                evidence: None,
                argument: None,
            },
            vec![],
            Some(data),
            RESULT_RETAINED | RESULT_PARTIAL,
            None,
        )?
    };
    projection::charge(program, &response, nodes.len())?;
    Ok(response)
}

async fn execute_node(
    operations: &PortableOperations,
    node: &BatchNode,
    budget: Budget,
    output_limit: u64,
    parent: &Program,
) -> Result<Executed, OperationError> {
    let response = match execute_node_response(operations, node, budget, output_limit, parent).await
    {
        Ok(response) => response,
        Err(error) => error.into_response(node.id())?,
    };
    let status = response.status();
    let state = match status {
        Status::Success => BatchNodeState::Succeeded,
        Status::Cancelled => BatchNodeState::Cancelled,
        _ => BatchNodeState::Failed,
    };
    let encoded = response.encode()?.encode().into_bytes();
    Ok(Executed {
        state,
        status,
        encoded,
    })
}

async fn execute_node_response(
    operations: &PortableOperations,
    node: &BatchNode,
    budget: Budget,
    output_limit: u64,
    parent: &Program,
) -> Result<FinalResponse, OperationError> {
    let request = Request::new(node.id(), node.arguments().clone(), budget)
        .map_err(|_| OperationError::InvalidArgument)?;
    let program = parent.child(node.id(), budget, output_limit)?;
    let _permit = program.acquire(PermitKind::Node).await?;
    operations.execute_leaf_response(&request, &program).await
}

fn preflight_response(
    request: &Request,
    nodes: &[BatchNode],
    program: &Program,
) -> Result<(), OperationError> {
    let results = nodes
        .iter()
        .map(|node| BatchNodeResult {
            id: node.id(),
            operation: node.arguments().operation(),
            state: BatchNodeState::Failed,
            status: Some(Status::Internal),
            reference: Some(u64::MAX),
        })
        .collect();
    let response = FinalResponse::failure(
        request.id(),
        Status::Failed,
        ErrorRecord {
            code: ErrorCode::BatchFailed,
            retry: RetryClass::Never,
            stage: ErrorStage::Execute,
            evidence: None,
            argument: None,
        },
        vec![],
        Some(ResultData::Batch(results)),
        RESULT_RETAINED | RESULT_PARTIAL,
        None,
    )?;
    if response.encode()?.encode().len() > projection::presentation_limit(program) {
        Err(OperationError::OutputBudget)
    } else {
        Ok(())
    }
}

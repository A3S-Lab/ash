use ash_platform::{
    FileAction, FileTransactionLimits, FileTransactionOutcome, MAX_FILE_TRANSACTION_FILE_BYTES,
    MAX_FILE_TRANSACTION_TOTAL_BYTES, TransactionControl, Workspace,
};

use crate::semantic::SemanticError;

/// Raw semantic service shared by ASH/1 filesystem actions and human-shell
/// portable mutations.
///
/// Callers may supply protocol-owned preimages directly or derive one from the
/// current workspace immediately before an interactive mutation. Execution
/// always revalidates those preimages inside the durable transaction boundary.
#[derive(Clone, Debug)]
pub struct SemanticMutationServices {
    workspace: Workspace,
    limits: FileTransactionLimits,
}

impl SemanticMutationServices {
    #[must_use]
    pub fn new(workspace: Workspace) -> Self {
        Self {
            workspace,
            limits: FileTransactionLimits {
                max_file_bytes: MAX_FILE_TRANSACTION_FILE_BYTES,
                max_total_bytes: MAX_FILE_TRANSACTION_TOTAL_BYTES,
            },
        }
    }

    pub fn validate(&self, actions: &[FileAction]) -> Result<(), SemanticError> {
        Ok(self.workspace.validate_file_actions(actions)?)
    }

    /// Prepares a no-overwrite empty-file creation.
    #[must_use]
    pub fn prepare_touch(&self, path: impl Into<String>) -> FileAction {
        FileAction::create(path, Vec::new())
    }

    /// Derives the source preimage immediately before a no-overwrite copy.
    pub fn prepare_copy(
        &self,
        source: impl Into<String>,
        destination: impl Into<String>,
    ) -> Result<FileAction, SemanticError> {
        let source = source.into();
        let digest = self.current_digest(&source)?;
        Ok(FileAction::copy(source, destination, digest))
    }

    /// Derives the source preimage immediately before a no-overwrite move.
    pub fn prepare_move(
        &self,
        source: impl Into<String>,
        destination: impl Into<String>,
    ) -> Result<FileAction, SemanticError> {
        let source = source.into();
        let digest = self.current_digest(&source)?;
        Ok(FileAction::move_file(source, destination, digest))
    }

    /// Derives the source preimage immediately before a regular-file removal.
    pub fn prepare_remove(&self, source: impl Into<String>) -> Result<FileAction, SemanticError> {
        let source = source.into();
        let digest = self.current_digest(&source)?;
        Ok(FileAction::remove(source, digest))
    }

    pub fn execute<F>(
        &self,
        actions: Vec<FileAction>,
        control: F,
    ) -> Result<FileTransactionOutcome, SemanticError>
    where
        F: FnMut() -> TransactionControl,
    {
        self.validate(&actions)?;
        Ok(self
            .workspace
            .file_transaction(actions, self.limits, control)?)
    }

    fn current_digest(&self, logical: &str) -> Result<[u8; 32], SemanticError> {
        let path = self.workspace.resolve_existing(logical)?;
        Ok(self
            .workspace
            .hash_file_limited_sync(&path, self.limits.max_file_bytes)?
            .digest)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use ash_platform::{FileActionState, FileTransactionFailure, TransactionControl, Workspace};

    use super::SemanticMutationServices;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("ash-semantic-mutation-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn prepared_actions_share_one_durable_transaction() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("copy-source"), b"copy\n").expect("copy source");
        fs::write(directory.0.join("move-source"), b"move\n").expect("move source");
        fs::write(directory.0.join("remove-source"), b"remove\n").expect("remove source");
        let services = SemanticMutationServices::new(
            Workspace::new(&directory.0).expect("mutation workspace"),
        );
        let actions = vec![
            services
                .prepare_copy("copy-source", "copy-destination")
                .expect("prepare copy"),
            services
                .prepare_move("move-source", "move-destination")
                .expect("prepare move"),
            services
                .prepare_remove("remove-source")
                .expect("prepare remove"),
            services.prepare_touch("created"),
        ];

        let outcome = services
            .execute(actions, || TransactionControl::Continue)
            .expect("execute transaction");

        assert_eq!(outcome.failure, None);
        assert!(
            outcome
                .actions
                .iter()
                .all(|action| action.state == FileActionState::Committed)
        );
        assert_eq!(
            fs::read(directory.0.join("copy-destination")).expect("copied file"),
            b"copy\n"
        );
        assert_eq!(
            fs::read(directory.0.join("move-destination")).expect("moved file"),
            b"move\n"
        );
        assert!(!directory.0.join("move-source").exists());
        assert!(!directory.0.join("remove-source").exists());
        assert_eq!(
            fs::read(directory.0.join("created")).expect("created file"),
            b""
        );
    }

    #[test]
    fn destination_conflict_never_overwrites_existing_content() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("source"), b"source\n").expect("source");
        fs::write(directory.0.join("destination"), b"keep\n").expect("destination");
        let services = SemanticMutationServices::new(
            Workspace::new(&directory.0).expect("mutation workspace"),
        );
        let action = services
            .prepare_copy("source", "destination")
            .expect("prepare copy");

        let outcome = services
            .execute(vec![action], || TransactionControl::Continue)
            .expect("execute transaction");

        assert_eq!(outcome.failure, Some(FileTransactionFailure::Conflict));
        assert_eq!(
            fs::read(directory.0.join("destination")).expect("destination remains"),
            b"keep\n"
        );
    }

    #[test]
    fn stale_interactive_preimage_is_reported_without_retry() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("source"), b"before\n").expect("source");
        let services = SemanticMutationServices::new(
            Workspace::new(&directory.0).expect("mutation workspace"),
        );
        let action = services.prepare_remove("source").expect("prepare remove");
        fs::write(directory.0.join("source"), b"after\n").expect("external update");

        let outcome = services
            .execute(vec![action], || TransactionControl::Continue)
            .expect("execute transaction");

        assert_eq!(outcome.failure, Some(FileTransactionFailure::Conflict));
        assert_eq!(
            fs::read(directory.0.join("source")).expect("updated source remains"),
            b"after\n"
        );
    }
}

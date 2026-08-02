#![forbid(unsafe_code)]

#[tokio::main(flavor = "multi_thread")]
async fn main() -> std::process::ExitCode {
    a3s_ash::entrypoint().await
}

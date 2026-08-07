use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Exact native launch plan for one explicit command delegated through WSL.
///
/// The Linux command and each of its arguments remain separate operating-system
/// strings after `--exec`; no host or Linux command shell is inserted. The
/// Windows working directory is passed to `wsl.exe --cd`, which lets the
/// selected distribution apply its own automount configuration when entering
/// the command. General command-argument path conversion is deliberately not
/// inferred by this adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WslLaunchPlan {
    launcher: PathBuf,
    argv: Vec<OsString>,
}

impl WslLaunchPlan {
    #[must_use]
    pub fn new(
        launcher: impl Into<PathBuf>,
        distribution: Option<&str>,
        cwd: &Path,
        command: &str,
        arguments: &[OsString],
    ) -> Self {
        let mut argv = Vec::with_capacity(arguments.len().saturating_add(7));
        if let Some(distribution) = distribution {
            argv.push(OsString::from("--distribution"));
            argv.push(OsString::from(distribution));
        }
        argv.push(OsString::from("--cd"));
        argv.push(cwd.as_os_str().to_owned());
        argv.push(OsString::from("--exec"));
        argv.push(OsString::from(command));
        argv.extend(arguments.iter().cloned());
        Self {
            launcher: launcher.into(),
            argv,
        }
    }

    #[must_use]
    pub fn launcher(&self) -> &Path {
        &self.launcher
    }

    #[must_use]
    pub fn argv(&self) -> &[OsString] {
        &self.argv
    }

    #[must_use]
    pub fn into_parts(self) -> (PathBuf, Vec<OsString>) {
        (self.launcher, self.argv)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::path::{Path, PathBuf};

    use super::WslLaunchPlan;

    #[test]
    fn launch_plan_preserves_direct_argv_and_selected_distribution() {
        let plan = WslLaunchPlan::new(
            PathBuf::from(r"C:\Windows\System32\wsl.exe"),
            Some("Ubuntu 24.04"),
            Path::new(r"D:\work tree\项目"),
            "printf",
            &[
                OsString::from("%s\\n"),
                OsString::from("two words"),
                OsString::from("$HOME; exit 9"),
            ],
        );

        assert_eq!(plan.launcher(), Path::new(r"C:\Windows\System32\wsl.exe"));
        assert_eq!(
            plan.argv(),
            [
                "--distribution",
                "Ubuntu 24.04",
                "--cd",
                r"D:\work tree\项目",
                "--exec",
                "printf",
                "%s\\n",
                "two words",
                "$HOME; exit 9",
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn launch_plan_omits_distribution_selector_for_user_default() {
        let plan = WslLaunchPlan::new("wsl.exe", None, Path::new(r"C:\workspace"), "true", &[]);

        assert_eq!(
            plan.argv(),
            ["--cd", r"C:\workspace", "--exec", "true"].map(OsString::from)
        );
    }

    #[test]
    fn launch_plan_parts_round_trip_without_reencoding() {
        let plan = WslLaunchPlan::new(
            "wsl.exe",
            Some("Ubuntu"),
            Path::new(r"C:\workspace"),
            "cat",
            &[OsString::from("fixture.bin")],
        );

        let (launcher, argv) = plan.into_parts();
        assert_eq!(launcher, PathBuf::from("wsl.exe"));
        assert_eq!(
            argv.last().map(OsString::as_os_str),
            Some(OsStr::new("fixture.bin"))
        );
    }
}

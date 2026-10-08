//! A forwarding Git shim counts executable invocations separately from timings.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub(super) struct Counting {
    directory: PathBuf,
    log: PathBuf,
}
impl Counting {
    pub(super) fn installed(root: &Path) -> Self {
        let directory = root.join("counting");
        let log = root.join("counting.log");
        std::fs::create_dir(&directory).expect("counting shim directory");
        let real = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|directory| directory.join("git"))
            .find(|path| {
                std::fs::metadata(path).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
            .expect("real Git executable");
        let real = std::fs::canonicalize(real).expect("absolute Git");
        let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
        let shim = directory.join("git");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\nprintf 'git\\n' >> {}\nexec {} \"$@\"\n",
                quote(&log),
                quote(&real)
            ),
        )
        .expect("forwarding Git shim");
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
            .expect("executable shim");
        Self { directory, log }
    }
    pub(super) fn clear(&self) {
        if self.log.exists() {
            std::fs::remove_file(&self.log).expect("clear this invocation's counter");
        }
    }
    pub(super) fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .expect("actual executable calls were recorded")
            .lines()
            .map(str::to_owned)
            .collect()
    }
    pub(super) fn with_program(&self, program: impl AsRef<std::ffi::OsStr>) -> assert_cmd::Command {
        let mut command = std::process::Command::new(program);
        let paths = std::iter::once(self.directory.clone())
            .chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ))
            .collect::<Vec<_>>();
        command
            .env_clear()
            .env("PATH", std::env::join_paths(paths).expect("shim PATH"));
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        assert_cmd::Command::from_std(command)
    }
}

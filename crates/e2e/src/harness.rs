//! Process-wide, once-guarded setup shared by every scenario: a unique run id,
//! the built container images, and the host-built `faultforge` CLI path.
//!
//! Image build and CLI build are expensive and must happen exactly once no
//! matter how many scenarios run in parallel — hence the [`OnceLock`] guards.
//! A build failure is cached and re-surfaced to every scenario (a loud,
//! actionable panic beats a silent skip; spec: "explicit run without podman
//! fails loudly").

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::podman;

/// The built master and agent image tags for this run.
#[derive(Debug, Clone)]
pub struct Images {
    /// Tag of the master runtime image.
    pub master: String,
    /// Tag of the agent runtime image.
    pub agent: String,
}

/// A short id unique to this test process, mixed into every resource name so
/// concurrent or repeated runs cannot collide (spec: isolation requirement).
pub fn run_id() -> &'static str {
    static RUN_ID: OnceLock<String> = OnceLock::new();
    RUN_ID.get_or_init(|| {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        format!("{}-{}", std::process::id(), ms % 1_000_000)
    })
}

/// The repository root (two levels above this crate's manifest dir).
fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .map_or(manifest.clone(), std::path::Path::to_path_buf)
}

/// Build the master and agent images once, sharing one builder stage so the
/// baked catalog is byte-identical across both (required for digest agreement:
/// the master's `RunFault.plugin_digest` must match the agent's local catalog).
///
/// # Panics
///
/// Panics with the failing `podman build` diagnostic if either image cannot be
/// built — there is no meaningful way to run scenarios without images.
pub fn images() -> &'static Images {
    static IMAGES: OnceLock<Result<Images, String>> = OnceLock::new();
    let result = IMAGES.get_or_init(|| {
        let root = repo_root();
        let containerfile = root.join("containers/Containerfile");
        let context = root.to_string_lossy().into_owned();
        let containerfile = containerfile.to_string_lossy().into_owned();
        let master = format!("localhost/ff-e2e-master:{}", run_id());
        let agent = format!("localhost/ff-e2e-agent:{}", run_id());

        podman::build(&containerfile, &context, "master-runtime", &master)
            .map_err(|e| e.to_string())?;
        podman::build(&containerfile, &context, "agent-runtime", &agent)
            .map_err(|e| e.to_string())?;
        Ok(Images { master, agent })
    });
    match result {
        Ok(images) => images,
        Err(e) => panic!("failed to build e2e container images: {e}"),
    }
}

/// The path to the host-built `faultforge` CLI, building it once if needed.
///
/// The CLI is a separate crate, so `CARGO_BIN_EXE_faultforge` is not defined
/// here; instead build it explicitly and locate it next to this test binary in
/// the target directory (robust to `CARGO_TARGET_DIR`).
///
/// # Panics
///
/// Panics if `cargo build -p faultforge` fails or the binary cannot be located.
pub fn faultforge_cli() -> &'static PathBuf {
    static CLI: OnceLock<PathBuf> = OnceLock::new();
    CLI.get_or_init(|| {
        // Package is `faultforge-cli`; its bin is named `faultforge`.
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "faultforge-cli"])
            .current_dir(repo_root())
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => panic!("`cargo build -p faultforge-cli` failed with {s}"),
            Err(e) => panic!("could not run cargo to build the CLI: {e}"),
        }

        // current_exe: <target>/debug/deps/<test-bin>; the CLI sits at
        // <target>/debug/faultforge (two levels up from deps).
        let exe = std::env::current_exe().expect("current_exe available in tests");
        let dir = exe
            .parent()
            .and_then(std::path::Path::parent)
            .expect("test binary lives under target/<profile>/deps");
        let cli = dir.join("faultforge");
        assert!(
            cli.exists(),
            "faultforge CLI not found at {} after build",
            cli.display()
        );
        cli
    })
}

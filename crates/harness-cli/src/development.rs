//! Source executables use their own workspace and host settings, never live defaults.
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(crate) struct Development {
    pub workspace: PathBuf,
    pub settings: PathBuf,
}

fn for_executable(executable: &Path, source: &Path, profile: &Path) -> Option<Development> {
    if !executable.starts_with(source) && !executable.starts_with(profile) {
        return None;
    }
    for_source(source)
}

fn for_source(source: &Path) -> Option<Development> {
    let name = source.file_name()?.to_string_lossy();
    let directory = source.parent()?.join(format!("{name}-development"));
    Some(Development {
        workspace: directory.join("workspace"),
        settings: directory.join("host-settings/job-harness/settings.json"),
    })
}

fn checkout_at(directory: &Path) -> Option<PathBuf> {
    directory.ancestors().find_map(|path| {
        let manifest = path.join("crates/harness-cli/Cargo.toml");
        if path.join("Cargo.toml").is_file()
            && path
                .join("resources/skills/harness-maintain/SKILL.md")
                .is_file()
            && std::fs::read_to_string(manifest)
                .is_ok_and(|text| text.contains("name = \"job-harness\""))
        {
            path.canonicalize().ok()
        } else {
            None
        }
    })
}

pub(crate) fn current() -> Option<Development> {
    // Even an installed CLI invoked while working inside a checkout is development.
    if let Some(source) = std::env::current_dir()
        .ok()
        .and_then(|path| checkout_at(&path))
    {
        return for_source(&source);
    }
    let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .canonicalize()
        .ok()?;
    let profile = Path::new(env!("JOB_HARNESS_BUILD_PROFILE_DIR"))
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("JOB_HARNESS_BUILD_PROFILE_DIR")));
    for_executable(&executable, &source, &profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_debug_release_and_desktop_share_an_external_development_folder() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("toolkit");
        let profile = source.join("target/debug");
        for relative in [
            "target/debug/job-harness",
            "target/release/jh.exe",
            "apps/desktop/target/debug/job-harness-desktop",
        ] {
            let dev = for_executable(&source.join(relative), &source, &profile).unwrap();
            assert_eq!(
                dev.workspace,
                root.path().join("toolkit-development/workspace")
            );
            assert!(!dev.workspace.starts_with(&source));
        }
    }

    #[test]
    fn external_cargo_outputs_are_development_but_copied_installs_are_not() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("toolkit");
        let profile = root.path().join("shared-target/debug");
        assert!(for_executable(&profile.join("jh"), &source, &profile).is_some());
        assert!(for_executable(&root.path().join("installed/jh"), &source, &profile).is_none());
        assert!(for_executable(&root.path().join("toolkit-other/jh"), &source, &profile).is_none());
    }

    #[test]
    fn checkout_detection_follows_nested_directories_not_unrelated_projects() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("toolkit");
        std::fs::create_dir_all(source.join("crates/harness-cli")).unwrap();
        std::fs::create_dir_all(source.join("resources/skills/harness-maintain")).unwrap();
        std::fs::write(source.join("Cargo.toml"), "[workspace]").unwrap();
        std::fs::write(
            source.join("crates/harness-cli/Cargo.toml"),
            "name = \"job-harness\"",
        )
        .unwrap();
        std::fs::write(
            source.join("resources/skills/harness-maintain/SKILL.md"),
            "Generic development",
        )
        .unwrap();
        assert_eq!(
            checkout_at(&source.join("crates/harness-cli")).unwrap(),
            source.canonicalize().unwrap()
        );
        assert!(checkout_at(root.path()).is_none());
    }
}

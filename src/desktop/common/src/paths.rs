use std::{
    env,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Component, Path, PathBuf},
};

use uuid::Uuid;

pub const APP_DIR_NAME: &str = "desktopctl";

const DEFAULT_AGENT_INSTRUCTIONS: &str = include_str!("../../../../agent/AGENTS.md");
const OBSIDIAN_SKILL: &str = include_str!("../../../../agent/skills/obsidian/SKILL.md");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSupportPaths {
    pub agent_dir: PathBuf,
    pub skills_dir: PathBuf,
    pub instructions_file: PathBuf,
    pub instructions_created: bool,
    pub obsidian_skill_created: bool,
}

impl AppPaths {
    pub fn resolve() -> io::Result<Self> {
        let home = env::var_os("HOME").or_else(|| {
            #[cfg(windows)]
            {
                env::var_os("USERPROFILE")
            }
            #[cfg(not(windows))]
            {
                None
            }
        });
        Self::resolve_from(
            env::var_os("DESKTOPCTL_HOME"),
            env::var_os("XDG_DATA_HOME"),
            home,
        )
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "unable to resolve DesktopCtl home: set DESKTOPCTL_HOME or HOME",
            )
        })
    }

    pub fn resolve_from(
        desktopctl_home: Option<OsString>,
        xdg_data_home: Option<OsString>,
        home: Option<OsString>,
    ) -> Option<Self> {
        let root = nonempty(desktopctl_home)
            .map(PathBuf::from)
            .or_else(|| nonempty(xdg_data_home).map(|base| PathBuf::from(base).join(APP_DIR_NAME)))
            .or_else(|| {
                nonempty(home).map(|base| {
                    PathBuf::from(base)
                        .join(".local")
                        .join("share")
                        .join(APP_DIR_NAME)
                })
            })?;
        Some(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    pub fn workspaces_dir(&self) -> PathBuf {
        self.root.join("workspaces")
    }

    pub fn agent_dir(&self) -> PathBuf {
        self.root.join("agent")
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.agent_dir().join("skills")
    }

    pub fn agent_instructions_file(&self) -> PathBuf {
        self.agent_dir().join("AGENTS.md")
    }

    /// Create the shared agent instructions and skills locations, preserving
    /// any files that already exist there as user-owned content.
    pub fn ensure_agent_support(&self) -> io::Result<AgentSupportPaths> {
        self.ensure_root()?;
        let agent_dir = self.agent_dir();
        let skills_dir = self.skills_dir();
        ensure_private_dir_without_symlink(&agent_dir)?;
        ensure_private_dir_without_symlink(&skills_dir)?;

        let instructions_file = self.agent_instructions_file();
        let instructions_created =
            ensure_user_file(&instructions_file, DEFAULT_AGENT_INSTRUCTIONS.as_bytes())?;

        let obsidian_dir = skills_dir.join("obsidian");
        ensure_private_dir_without_symlink(&obsidian_dir)?;
        let obsidian_skill_created =
            ensure_user_file(&obsidian_dir.join("SKILL.md"), OBSIDIAN_SKILL.as_bytes())?;

        Ok(AgentSupportPaths {
            agent_dir,
            skills_dir,
            instructions_file,
            instructions_created,
            obsidian_skill_created,
        })
    }

    /// Filesystem workspace assigned to one DesktopCtl agent session.
    pub fn agent_workspace_dir(&self, session_id: &str) -> io::Result<PathBuf> {
        let id = Uuid::parse_str(session_id).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid agent session ID {session_id:?}: {error}"),
            )
        })?;
        if id.to_string() != session_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("agent session ID is not a canonical UUID: {session_id:?}"),
            ));
        }
        Ok(self.workspaces_dir().join(session_id))
    }

    pub fn ensure_agent_workspace_dir(&self, session_id: &str) -> io::Result<PathBuf> {
        let support = self.ensure_agent_support()?;
        self.ensure_workspaces_dir()?;
        let path = self.agent_workspace_dir(session_id)?;
        ensure_private_dir_without_symlink(&path)?;
        ensure_workspace_link(&path.join("AGENTS.md"), &support.instructions_file, false)?;
        ensure_workspace_link(&path.join("skills"), &support.skills_dir, true)?;
        Ok(path)
    }

    pub fn agent_sessions_file(&self) -> PathBuf {
        self.workspaces_dir().join("agent-sessions.json")
    }

    pub fn daemon_log_file(&self) -> PathBuf {
        self.logs_dir().join("desktopctld.log")
    }

    pub fn ensure_root(&self) -> io::Result<()> {
        ensure_private_dir(&self.root)
    }

    pub fn ensure_state_dir(&self) -> io::Result<PathBuf> {
        self.ensure_subdir(self.state_dir())
    }

    pub fn ensure_logs_dir(&self) -> io::Result<PathBuf> {
        self.ensure_subdir(self.logs_dir())
    }

    pub fn ensure_cache_dir(&self) -> io::Result<PathBuf> {
        self.ensure_subdir(self.cache_dir())
    }

    pub fn ensure_cache_subdir(&self, name: &str) -> io::Result<PathBuf> {
        let path = self.ensure_cache_dir()?.join(name);
        ensure_private_dir(&path)?;
        Ok(path)
    }

    pub fn ensure_logs_subdir(&self, name: &str) -> io::Result<PathBuf> {
        let path = self.ensure_logs_dir()?.join(name);
        ensure_private_dir(&path)?;
        Ok(path)
    }

    pub fn ensure_workspaces_dir(&self) -> io::Result<PathBuf> {
        self.ensure_root()?;
        let path = self.workspaces_dir();
        ensure_private_dir_without_symlink(&path)?;
        Ok(path)
    }

    fn ensure_subdir(&self, path: PathBuf) -> io::Result<PathBuf> {
        self.ensure_root()?;
        ensure_private_dir(&path)?;
        Ok(path)
    }
}

fn nonempty(value: Option<OsString>) -> Option<OsString> {
    value.filter(|value| !value.is_empty() && value != OsStr::new(""))
}

fn ensure_user_file(path: &Path, contents: &[u8]) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("expected a file but found a directory: {}", path.display()),
                ));
            }
            if metadata.file_type().is_symlink() {
                match fs::metadata(path) {
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        return Err(io::Error::new(
                            io::ErrorKind::NotFound,
                            format!("user file symlink is broken: {}", path.display()),
                        ));
                    }
                    Err(error) => return Err(error),
                }
            }
            Ok(false)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?;
            use std::io::Write;
            file.write_all(contents)?;
            file.sync_all()?;
            set_private_file_permissions(&file)?;
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

fn ensure_workspace_link(link: &Path, target: &Path, target_is_dir: bool) -> io::Result<()> {
    let desired = relative_path(link.parent().unwrap_or_else(|| Path::new(".")), target)?;
    match fs::symlink_metadata(link) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if workspace_link_points_to(link, target) {
                return Ok(());
            }
            match fs::metadata(link) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    fs::remove_file(link)?;
                    create_symlink(&desired, link, target_is_dir)?;
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "workspace symlink points to a different target: {}",
                    link.display()
                ),
            ))
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("workspace path is already occupied: {}", link.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_symlink(&desired, link, target_is_dir)
        }
        Err(error) => Err(error),
    }
}

fn workspace_link_points_to(link: &Path, target: &Path) -> bool {
    let Ok(destination) = fs::read_link(link) else {
        return false;
    };
    let resolved = if destination.is_absolute() {
        destination
    } else {
        link.parent()
            .unwrap_or_else(|| Path::new("."))
            .join(destination)
    };
    normalize_path(&resolved) == normalize_path(target)
}

fn relative_path(from: &Path, to: &Path) -> io::Result<PathBuf> {
    let from = normalize_path(from);
    let to = normalize_path(to);
    let from_components: Vec<_> = from.components().collect();
    let to_components: Vec<_> = to.components().collect();
    let from_prefix = from_components
        .iter()
        .find_map(|component| match component {
            Component::Prefix(prefix) => Some(prefix),
            _ => None,
        });
    let to_prefix = to_components.iter().find_map(|component| match component {
        Component::Prefix(prefix) => Some(prefix),
        _ => None,
    });
    if from_prefix != to_prefix || from.is_absolute() != to.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "cannot make relative symlink from {} to {}",
                from.display(),
                to.display()
            ),
        ));
    }
    let common = from_components
        .iter()
        .zip(&to_components)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for component in &from_components[common..] {
        if matches!(component, Component::Normal(_)) {
            relative.push("..");
        }
    }
    for component in &to_components[common..] {
        relative.push(component.as_os_str());
    }
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    Ok(relative)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(
                    normalized.components().next_back(),
                    Some(Component::RootDir)
                ) {
                    normalized.pop();
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path, _target_is_dir: bool) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_symlink(target: &Path, link: &Path, target_is_dir: bool) -> io::Result<()> {
    if target_is_dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

#[cfg(not(any(unix, windows)))]
fn create_symlink(_target: &Path, _link: &Path, _target_is_dir: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "workspace symlinks are unsupported on this platform",
    ))
}

pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    set_private_dir_permissions(path)
}

fn ensure_private_dir_without_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => match fs::create_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        },
        Err(error) => return Err(error),
    }

    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("refusing symlinked workspace directory: {}", path.display()),
        ));
    }
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("workspace path is not a directory: {}", path.display()),
        ));
    }
    set_private_dir_permissions(path)
}

#[cfg(unix)]
fn set_private_file_permissions(file: &fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_file: &fs::File) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktopctl_home_has_highest_precedence() {
        let paths = AppPaths::resolve_from(
            Some("/custom/root".into()),
            Some("/xdg".into()),
            Some("/home/test".into()),
        )
        .unwrap();
        assert_eq!(paths.root(), Path::new("/custom/root"));
    }

    #[test]
    fn xdg_data_home_precedes_home_fallback() {
        let paths =
            AppPaths::resolve_from(None, Some("/xdg".into()), Some("/home/test".into())).unwrap();
        assert_eq!(paths.root(), Path::new("/xdg/desktopctl"));
    }

    #[test]
    fn home_fallback_is_linux_style_on_every_platform() {
        let paths = AppPaths::resolve_from(None, None, Some("/home/test".into())).unwrap();
        assert_eq!(
            paths.root(),
            Path::new("/home/test/.local/share/desktopctl")
        );
        assert_eq!(
            paths.config_file(),
            Path::new("/home/test/.local/share/desktopctl/config.toml")
        );
        assert_eq!(
            paths.state_dir(),
            Path::new("/home/test/.local/share/desktopctl/state")
        );
        assert_eq!(
            paths.logs_dir(),
            Path::new("/home/test/.local/share/desktopctl/logs")
        );
        assert_eq!(
            paths.cache_dir(),
            Path::new("/home/test/.local/share/desktopctl/cache")
        );
        assert_eq!(
            paths.workspaces_dir(),
            Path::new("/home/test/.local/share/desktopctl/workspaces")
        );
    }

    #[test]
    fn empty_overrides_are_ignored() {
        let paths = AppPaths::resolve_from(
            Some(OsString::new()),
            Some(OsString::new()),
            Some("/home/test".into()),
        )
        .unwrap();
        assert_eq!(
            paths.root(),
            Path::new("/home/test/.local/share/desktopctl")
        );
    }

    #[cfg(unix)]
    #[test]
    fn lazily_created_directories_are_user_only() {
        use std::os::unix::fs::PermissionsExt;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "desktopctl-paths-{}-{timestamp}",
            std::process::id()
        ));
        let paths = AppPaths { root: root.clone() };
        let cache = paths.ensure_cache_dir().unwrap();
        assert_eq!(
            fs::metadata(paths.root()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(cache).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn agent_workspace_requires_canonical_uuid() {
        let paths = AppPaths {
            root: PathBuf::from("/tmp/desktopctl-paths-test"),
        };
        assert_eq!(
            paths
                .agent_workspace_dir("../../outside")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            paths
                .agent_workspace_dir("550e8400-e29b-41d4-a716-446655440000")
                .unwrap(),
            PathBuf::from(
                "/tmp/desktopctl-paths-test/workspaces/550e8400-e29b-41d4-a716-446655440000"
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn agent_workspace_rejects_symlink() {
        use std::os::unix::fs::symlink;

        let root = env::temp_dir().join(format!(
            "desktopctl-paths-symlink-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = AppPaths { root: root.clone() };
        paths.ensure_workspaces_dir().unwrap();
        let outside = root.join("outside");
        fs::create_dir(&outside).unwrap();
        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        symlink(&outside, paths.agent_workspace_dir(session_id).unwrap()).unwrap();

        assert_eq!(
            paths
                .ensure_agent_workspace_dir(session_id)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        let _ = fs::remove_dir_all(root);
    }

    fn support_test_paths(name: &str) -> (AppPaths, PathBuf) {
        let root = env::temp_dir().join(format!(
            "desktopctl-agent-support-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        (AppPaths { root: root.clone() }, root)
    }

    #[test]
    fn fresh_agent_support_install_creates_defaults() {
        let (paths, root) = support_test_paths("fresh");
        let support = paths.ensure_agent_support().unwrap();

        assert_eq!(
            fs::read_to_string(support.instructions_file).unwrap(),
            DEFAULT_AGENT_INSTRUCTIONS
        );
        assert_eq!(
            fs::read_to_string(paths.skills_dir().join("obsidian/SKILL.md")).unwrap(),
            OBSIDIAN_SKILL
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn agent_support_initialization_is_idempotent() {
        let (paths, root) = support_test_paths("idempotent");
        paths.ensure_agent_support().unwrap();
        let instructions_mtime = fs::metadata(paths.agent_instructions_file())
            .unwrap()
            .modified()
            .unwrap();
        paths.ensure_agent_support().unwrap();

        assert_eq!(
            fs::read_to_string(paths.agent_instructions_file()).unwrap(),
            DEFAULT_AGENT_INSTRUCTIONS
        );
        assert_eq!(
            fs::metadata(paths.agent_instructions_file())
                .unwrap()
                .modified()
                .unwrap(),
            instructions_mtime
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn user_changes_to_shared_files_are_preserved() {
        let (paths, root) = support_test_paths("preserve");
        paths.ensure_agent_support().unwrap();
        let instructions = "# My instructions\n";
        let skill = "# My Obsidian workflow\n";
        fs::write(paths.agent_instructions_file(), instructions).unwrap();
        fs::write(paths.skills_dir().join("obsidian/SKILL.md"), skill).unwrap();

        paths.ensure_agent_support().unwrap();

        assert_eq!(
            fs::read_to_string(paths.agent_instructions_file()).unwrap(),
            instructions
        );
        assert_eq!(
            fs::read_to_string(paths.skills_dir().join("obsidian/SKILL.md")).unwrap(),
            skill
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn workspace_gets_relative_shared_links_and_keeps_correct_links() {
        let (paths, root) = support_test_paths("links");
        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let workspace = paths.ensure_agent_workspace_dir(session_id).unwrap();
        let agents_link = workspace.join("AGENTS.md");
        let skills_link = workspace.join("skills");
        let agents_destination = fs::read_link(&agents_link).unwrap();
        let skills_destination = fs::read_link(&skills_link).unwrap();

        assert_eq!(
            fs::canonicalize(&agents_link).unwrap(),
            fs::canonicalize(paths.agent_instructions_file()).unwrap()
        );
        assert_eq!(
            fs::canonicalize(&skills_link).unwrap(),
            fs::canonicalize(paths.skills_dir()).unwrap()
        );
        paths.ensure_agent_workspace_dir(session_id).unwrap();
        assert_eq!(fs::read_link(agents_link).unwrap(), agents_destination);
        assert_eq!(fs::read_link(skills_link).unwrap(), skills_destination);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn broken_workspace_links_are_repaired() {
        use std::os::unix::fs::symlink;

        let (paths, root) = support_test_paths("broken");
        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        paths.ensure_agent_support().unwrap();
        let workspace = paths.ensure_workspaces_dir().unwrap().join(session_id);
        fs::create_dir(&workspace).unwrap();
        symlink("missing-agents", workspace.join("AGENTS.md")).unwrap();
        symlink("missing-skills", workspace.join("skills")).unwrap();

        paths.ensure_agent_workspace_dir(session_id).unwrap();

        assert_eq!(
            fs::canonicalize(workspace.join("AGENTS.md")).unwrap(),
            fs::canonicalize(paths.agent_instructions_file()).unwrap()
        );
        assert_eq!(
            fs::canonicalize(workspace.join("skills")).unwrap(),
            fs::canonicalize(paths.skills_dir()).unwrap()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn regular_workspace_file_is_not_deleted() {
        let (paths, root) = support_test_paths("file-conflict");
        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        paths.ensure_agent_support().unwrap();
        let workspace = paths.ensure_workspaces_dir().unwrap().join(session_id);
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("AGENTS.md"), "keep me").unwrap();

        let error = paths.ensure_agent_workspace_dir(session_id).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read_to_string(workspace.join("AGENTS.md")).unwrap(),
            "keep me"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn regular_workspace_directory_is_not_deleted() {
        let (paths, root) = support_test_paths("directory-conflict");
        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        paths.ensure_agent_support().unwrap();
        let workspace = paths.ensure_workspaces_dir().unwrap().join(session_id);
        fs::create_dir_all(workspace.join("skills")).unwrap();

        let error = paths.ensure_agent_workspace_dir(session_id).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(
            fs::symlink_metadata(workspace.join("skills"))
                .unwrap()
                .is_dir()
        );
        let _ = fs::remove_dir_all(root);
    }
}

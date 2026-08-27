use std::{
    env,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
};

use uuid::Uuid;

pub const APP_DIR_NAME: &str = "desktopctl";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    root: PathBuf,
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
        self.ensure_workspaces_dir()?;
        let path = self.agent_workspace_dir(session_id)?;
        ensure_private_dir_without_symlink(&path)?;
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
}

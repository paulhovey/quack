//! Shell detection: `$SHELL`/bash/zsh on Unix, pwsh → powershell on Windows.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellKind {
    Bash(PathBuf),
    Zsh(PathBuf),
    Sh(PathBuf),
    Pwsh(PathBuf),
    PowerShell(PathBuf),
    Cmd(PathBuf),
}

impl ShellKind {
    /// Resolve `[tools].shell`: `auto`, a shell name, or a path.
    pub fn detect(setting: &str, env_shell: Option<&str>) -> Self {
        match setting {
            "" | "auto" => Self::auto(env_shell),
            name => Self::from_path(&which::which(name).unwrap_or_else(|_| PathBuf::from(name)))
                .unwrap_or_else(|| Self::auto(env_shell)),
        }
    }

    fn auto(env_shell: Option<&str>) -> Self {
        if cfg!(windows) {
            for name in ["pwsh", "powershell"] {
                if let Some(k) = which::which(name).ok().and_then(|p| Self::from_path(&p)) {
                    return k;
                }
            }
            return Self::Cmd(PathBuf::from("cmd.exe"));
        }
        // Only shells whose `-c` semantics we know; fish, nu etc. fall through to bash.
        if let Some(k) = env_shell
            .map(Path::new)
            .and_then(Self::from_path)
            .filter(|k| matches!(k, Self::Bash(_) | Self::Zsh(_)))
        {
            return k;
        }
        for name in ["bash", "zsh", "sh"] {
            if let Some(k) = which::which(name).ok().and_then(|p| Self::from_path(&p)) {
                return k;
            }
        }
        Self::Sh(PathBuf::from("/bin/sh"))
    }

    fn from_path(p: &Path) -> Option<Self> {
        // Split on both separators so Windows paths from config work on every host.
        let name = p.to_str()?.rsplit(['/', '\\']).next()?.to_ascii_lowercase();
        let stem = name.strip_suffix(".exe").unwrap_or(&name);
        let p = p.to_path_buf();
        Some(match stem {
            "bash" => Self::Bash(p),
            "zsh" => Self::Zsh(p),
            "sh" | "dash" => Self::Sh(p),
            "pwsh" => Self::Pwsh(p),
            "powershell" => Self::PowerShell(p),
            "cmd" => Self::Cmd(p),
            _ => return None,
        })
    }

    /// Short name for prompts and the UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Bash(_) => "bash",
            Self::Zsh(_) => "zsh",
            Self::Sh(_) => "sh",
            Self::Pwsh(_) => "pwsh",
            Self::PowerShell(_) => "powershell",
            Self::Cmd(_) => "cmd",
        }
    }

    pub fn program(&self) -> &Path {
        match self {
            Self::Bash(p)
            | Self::Zsh(p)
            | Self::Sh(p)
            | Self::Pwsh(p)
            | Self::PowerShell(p)
            | Self::Cmd(p) => p,
        }
    }

    /// Arguments that run `script` non-interactively.
    pub fn args<'a>(&self, script: &'a str) -> Vec<&'a str> {
        match self {
            Self::Bash(_) | Self::Zsh(_) | Self::Sh(_) => vec!["-c", script],
            Self::Pwsh(_) | Self::PowerShell(_) => {
                vec![
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    script,
                ]
            }
            Self::Cmd(_) => vec!["/D", "/S", "/C", script],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_map_to_kinds() {
        assert_eq!(
            ShellKind::from_path(Path::new("/usr/bin/zsh")).map(|k| k.label()),
            Some("zsh")
        );
        assert_eq!(
            ShellKind::from_path(Path::new(r"C:\Program Files\PowerShell\7\pwsh.exe"))
                .map(|k| k.label()),
            Some("pwsh")
        );
        assert_eq!(ShellKind::from_path(Path::new("/usr/bin/fish")), None);
    }
}

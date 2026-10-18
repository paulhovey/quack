//! The minimal environment for servers the harness starts (MCP, LSP).

use std::collections::BTreeMap;
use std::ffi::OsString;

/// Variables a server gets from the harness's environment unless its file says otherwise:
/// enough to find programs, write temp files and reach a proxy.
const INHERIT: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TMPDIR",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
    "https_proxy",
    "http_proxy",
    "no_proxy",
];

/// What many Node and Python servers fail without on Windows.
#[cfg(windows)]
const INHERIT_WINDOWS: &[&str] = &[
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "TEMP",
    "TMP",
    "APPDATA",
    "LOCALAPPDATA",
    "USERPROFILE",
    "USERNAME",
    "HOMEDRIVE",
    "HOMEPATH",
    "PATHEXT",
    "COMSPEC",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMDATA",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
];

#[cfg(not(windows))]
const INHERIT_WINDOWS: &[&str] = &[];

/// The default variables (or `names`) that are set in this process's environment.
pub fn inherited_env(names: Option<&[String]>) -> BTreeMap<String, OsString> {
    let names: Vec<&str> = match names {
        Some(list) => list.iter().map(String::as_str).collect(),
        None => INHERIT.iter().chain(INHERIT_WINDOWS).copied().collect(),
    };
    names
        .into_iter()
        .filter_map(|k| std::env::var_os(k).map(|v| (k.to_owned(), v)))
        .collect()
}

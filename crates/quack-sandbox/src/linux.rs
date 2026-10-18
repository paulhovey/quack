//! Landlock (writes) and seccomp (network) on Linux.
//!
//! The Landlock ruleset is built in the parent: it opens each writable path and adds a
//! rule for it. The child then only makes three system calls before `exec`
//! (`PR_SET_NO_NEW_PRIVS`, `landlock_restrict_self`, `PR_SET_SECCOMP`), which allocate
//! nothing and are safe between `fork` and `exec`. The restrictions are inherited by
//! everything the program starts and cannot be lifted.
#![allow(unsafe_code)]

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use tokio::process::Command;

use crate::Policy;

// Landlock filesystem rights (include/uapi/linux/landlock.h).
const WRITE_FILE: u64 = 1 << 1;
const REMOVE_DIR: u64 = 1 << 4;
const REMOVE_FILE: u64 = 1 << 5;
const MAKE_CHAR: u64 = 1 << 6;
const MAKE_DIR: u64 = 1 << 7;
const MAKE_REG: u64 = 1 << 8;
const MAKE_SOCK: u64 = 1 << 9;
const MAKE_FIFO: u64 = 1 << 10;
const MAKE_BLOCK: u64 = 1 << 11;
const MAKE_SYM: u64 = 1 << 12;
/// ABI 2: linking or renaming between folders.
const REFER: u64 = 1 << 13;
/// ABI 3: truncating (`>` on an existing file).
const TRUNCATE: u64 = 1 << 14;
/// Rights that apply to a single file rather than a folder.
const FILE_RIGHTS: u64 = WRITE_FILE | TRUNCATE;

const CREATE_RULESET_VERSION: u32 = 1;
const RULE_PATH_BENEATH: u32 = 1;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// Devices programs commonly write to.
const DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
    "/dev/shm",
];

fn abi() -> Result<i64, String> {
    // SAFETY: the version query takes no attribute pointer and returns a number.
    let v = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            CREATE_RULESET_VERSION,
        )
    };
    if v < 0 {
        let e = io::Error::last_os_error();
        return Err(match e.raw_os_error() {
            Some(libc::ENOSYS) => "the kernel has no Landlock support (Linux 5.13+ needed)".into(),
            Some(libc::EOPNOTSUPP) => {
                "Landlock is turned off in this kernel (add `landlock` to the lsm= boot option)"
                    .into()
            }
            _ => format!("Landlock is not available: {e}"),
        });
    }
    Ok(v)
}

pub(crate) fn available() -> Result<&'static str, String> {
    abi()?;
    filter(false)?;
    Ok("Landlock + seccomp")
}

fn handled(abi: i64) -> u64 {
    let mut rights = WRITE_FILE
        | REMOVE_DIR
        | REMOVE_FILE
        | MAKE_CHAR
        | MAKE_DIR
        | MAKE_REG
        | MAKE_SOCK
        | MAKE_FIFO
        | MAKE_BLOCK
        | MAKE_SYM;
    if abi >= 2 {
        rights |= REFER;
    }
    if abi >= 3 {
        rights |= TRUNCATE;
    }
    rights
}

/// The ruleset: writes allowed beneath each writable path and to common devices.
fn ruleset(policy: &Policy) -> Result<OwnedFd, String> {
    let rights = handled(abi()?);
    let attr = RulesetAttr {
        handled_access_fs: rights,
    };
    // SAFETY: `attr` is a valid ruleset attribute of the size passed.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &raw const attr,
            size_of::<RulesetAttr>(),
            0u32,
        )
    };
    if fd < 0 {
        return Err(format!(
            "cannot create the Landlock ruleset: {}",
            io::Error::last_os_error()
        ));
    }
    let fd = RawFd::try_from(fd).map_err(|e| e.to_string())?;
    // SAFETY: the kernel just returned this descriptor and nothing else owns it.
    let ruleset = unsafe { OwnedFd::from_raw_fd(fd) };
    let paths = policy
        .writable
        .iter()
        .map(|p| p.as_path())
        .chain(DEVICES.iter().map(Path::new));
    for path in paths {
        // Missing paths are fine: nothing can be written there anyway.
        let _ = add_rule(&ruleset, path, rights);
    }
    Ok(ruleset)
}

fn add_rule(ruleset: &OwnedFd, path: &Path, rights: u64) -> io::Result<()> {
    let c = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `c` is a valid C string; O_PATH opens without reading.
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: freshly opened and owned here.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let is_dir = std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false);
    let attr = PathBeneathAttr {
        allowed_access: if is_dir { rights } else { rights & FILE_RIGHTS },
        parent_fd: fd.as_raw_fd(),
    };
    // SAFETY: both descriptors are open and `attr` is a valid path-beneath rule.
    let r = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset.as_raw_fd(),
            RULE_PATH_BENEATH,
            &raw const attr,
            0u32,
        )
    };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// Classic BPF for seccomp (include/uapi/linux/filter.h, seccomp.h).
/// `BPF_LD | BPF_W | BPF_ABS`
const BPF_LD_W_ABS: u16 = 0x20;
/// `BPF_JMP | BPF_JEQ | BPF_K`
const BPF_JMP_JEQ_K: u16 = 0x15;
/// `BPF_JMP | BPF_JGE | BPF_K`
const BPF_JMP_JGE_K: u16 = 0x35;
/// `BPF_RET | BPF_K`
const BPF_RET_K: u16 = 0x06;
const RET_ALLOW: u32 = 0x7fff_0000;
const RET_ERRNO: u32 = 0x0005_0000;
const OFFSET_NR: u32 = 0;
const OFFSET_ARCH: u32 = 4;
const OFFSET_ARG0: u32 = 16;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

fn stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

/// The seccomp program: refuse `socket()` for anything but Unix-domain sockets, and
/// `io_uring` (which can open sockets on its own). Other architectures' system calls
/// are refused outright, so 32-bit entry points can't get around it. Empty when the
/// network is allowed.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn filter(network: bool) -> Result<Vec<libc::sock_filter>, String> {
    if network {
        return Ok(Vec::new());
    }
    let eperm = RET_ERRNO | libc::EPERM as u32;
    let socket = u32::try_from(libc::SYS_socket).map_err(|e| e.to_string())?;
    let uring = u32::try_from(libc::SYS_io_uring_setup).map_err(|e| e.to_string())?;
    let af_unix = u32::try_from(libc::AF_UNIX).map_err(|e| e.to_string())?;
    // Jump offsets count instructions after the jump.
    let mut f = vec![
        stmt(BPF_LD_W_ABS, OFFSET_ARCH),
        jump(BPF_JMP_JEQ_K, AUDIT_ARCH, 1, 0),
        stmt(BPF_RET_K, eperm),
        stmt(BPF_LD_W_ABS, OFFSET_NR),
    ];
    if cfg!(target_arch = "x86_64") {
        // x32 system calls have this bit set.
        f.push(jump(BPF_JMP_JGE_K, 0x4000_0000, 0, 1));
        f.push(stmt(BPF_RET_K, eperm));
    }
    f.extend([
        jump(BPF_JMP_JEQ_K, uring, 0, 1),
        stmt(BPF_RET_K, eperm),
        jump(BPF_JMP_JEQ_K, socket, 1, 0),
        stmt(BPF_RET_K, RET_ALLOW),
        // socket(domain, …): the low 32 bits of the first argument.
        stmt(BPF_LD_W_ABS, OFFSET_ARG0),
        jump(BPF_JMP_JEQ_K, af_unix, 0, 1),
        stmt(BPF_RET_K, RET_ALLOW),
        stmt(BPF_RET_K, eperm),
    ]);
    Ok(f)
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn filter(network: bool) -> Result<Vec<libc::sock_filter>, String> {
    if network {
        return Ok(Vec::new());
    }
    Err("the network filter supports x86-64 and ARM64 only".into())
}

pub(crate) fn command<I, S>(policy: &Policy, program: &OsStr, args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let ruleset = ruleset(policy)?;
    let program_filter = filter(policy.network)?;
    let mut cmd = Command::new(program);
    cmd.args(args);
    // SAFETY: the closure only makes system calls (no allocation, no locks) on data
    // prepared above, which is what is allowed between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0u32) != 0 {
                return Err(io::Error::last_os_error());
            }
            if !program_filter.is_empty() {
                let prog = libc::sock_fprog {
                    len: program_filter.len() as u16,
                    filter: program_filter.as_ptr().cast_mut(),
                };
                if libc::prctl(
                    libc::PR_SET_SECCOMP,
                    libc::SECCOMP_MODE_FILTER,
                    &raw const prog,
                ) != 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    Ok(cmd)
}

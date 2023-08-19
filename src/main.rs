//! unpak, the modern package manager
//! unpak needs a bootstrap GCC to compile the sandboxed-GCC
//! |bootstrap GCC and programs| -> |stage1 GCC| -> |stage2 GCC| -> coreutils and build deps, etc
//! stage1 is cross-compiled by the bootstrapper, and
//! stage2 is compiled by stage1 to ensure full sandboxing.

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Child, Stdio};
use std::{io, io::ErrorKind, path::PathBuf, process::Command};

// com.github.osten.unpak
#[derive(PartialEq, Eq, Hash, Clone, Debug, Serialize, Deserialize)]
struct ProjectId(String);

impl From<&str> for ProjectId {
    fn from(other: &str) -> Self {
        ProjectId(other.to_string())
    }
}

impl From<String> for ProjectId {
    fn from(other: String) -> Self {
        ProjectId(other)
    }
}

#[derive(Serialize, Deserialize)]
struct SourceProject {
    id: ProjectId,
    build: BuildProcess,
    rdeps: Vec<ProjectId>,
    bdeps: Vec<ProjectId>,
}

#[allow(dead_code)]
impl SourceProject {
    pub fn build(&self) {
        println!("[unpak] building project...");
        match &self.build {
            BuildProcess::Cmds(cmds) => {
                for cmd in cmds {
                    println!(
                        "[unpak] executing '{} {}'",
                        cmd.program.to_string_lossy(),
                        cmd.arguments.join(" ")
                    );
                    // Execute command
                    Command::new(cmd.program.as_os_str())
                        .args(&cmd.arguments)
                        .spawn()
                        .expect("failed to spawn command")
                        .wait()
                        .unwrap();
                }
            }
        }
    }
}
const INTERPRETER_HOST: &str = "/lib64/ld-linux-x86-64.so.2";
const SBX_LD_LINUX: &str = "/usr/lib/ld-linux-x86-64.so.2";

#[allow(dead_code)]
fn patch_noncompliant(program: &Path) {
    let mut command = Command::new("patchelf");
    command.args(["--set-interpreter", SBX_LD_LINUX]);
    command.arg(program);

    let mut proc = match command.spawn() {
        Ok(proc) => proc,
        Err(e) => {
            match e.kind() {
                ErrorKind::NotFound => {
                    eprintln!("error: patchelf not found. Is patchelf installed?")
                }
                _ => eprintln!("error: unknown error: {e:?}"),
            };
            return;
        }
    };

    let exit = proc.wait().unwrap();
    eprintln!("[unpak] patchelf exited with code {exit}");
}

/* unpak/bdeps */
/* unpak/rdeps */

enum StdMountLocation {
    UserExe,
    UserSo,
}

const FHS_EXE: &str = "/usr/bin";
const FHS_SO: &str = "/usr/lib";

#[allow(dead_code)]
impl StdMountLocation {
    fn into_absolute_path(self) -> PathBuf {
        match self {
            StdMountLocation::UserExe => FHS_EXE.into(),
            StdMountLocation::UserSo => FHS_SO.into(),
        }
    }

    fn to_absolute_path(&self) -> PathBuf {
        match self {
            StdMountLocation::UserExe => FHS_EXE.into(),
            StdMountLocation::UserSo => FHS_SO.into(),
        }
    }
}

struct HostPath(pub PathBuf);
struct SbxPath(pub PathBuf);

impl<T: Into<PathBuf>> From<T> for HostPath {
    fn from(value: T) -> Self {
        HostPath(value.into())
    }
}

impl<T: Into<PathBuf>> From<T> for SbxPath {
    fn from(value: T) -> Self {
        SbxPath(value.into())
    }
}

/// Creates a symlink from `dest -> src`
struct Symlink {
    /// Where to create the symlink
    dest: SbxPath,
    /// Where the symlink points to
    src: SbxPath,
}

enum Mount {
    Touch {
        sbx_path: SbxPath,
    },
    Fs {
        readonly: bool,
        host_path: HostPath,
        sbx_path: SbxPath,
    },
}

impl<A: Into<HostPath>, B: Into<SbxPath>> From<(A, B)> for Mount {
    fn from((host, sbx): (A, B)) -> Self {
        Mount::Fs {
            readonly: true,
            host_path: host.into(),
            sbx_path: sbx.into(),
        }
    }
}

impl<T: Into<HostPath>> From<(T, StdMountLocation)> for Mount {
    fn from((host, base_sbx): (T, StdMountLocation)) -> Self {
        let host_path = host.into();
        let filename = host_path.0.file_name().unwrap();
        let sbx_path = base_sbx.into_absolute_path().join(filename).into();

        Mount::Fs {
            host_path,
            sbx_path,
            readonly: true,
        }
    }
}

struct Bubblewrap {
    mounts: Vec<Mount>,
    symlinks: Vec<Symlink>,
    path: Option<OsString>,
    chdir: Option<PathBuf>,
    unshare_pid: bool,

    new_session: bool,
    detach_output: bool,

    program: Option<PathBuf>,
    envvars: EnvVars,
}

enum EnvVars {
    Inherit,
    Set(Vec<(OsString, OsString)>),
}

impl EnvVars {
    fn set_mut(&mut self) -> &mut Vec<(OsString, OsString)> {
        match self {
            EnvVars::Inherit => panic!("cannot add new environment variables if `inherit` is set."),
            EnvVars::Set(list) => list,
        }
    }
}

#[allow(dead_code)]
impl Bubblewrap {
    fn new() -> Self {
        Self {
            mounts: Vec::new(),
            symlinks: Vec::new(),
            path: None,
            chdir: None,
            unshare_pid: false,
            new_session: false,
            detach_output: false,
            program: None,
            envvars: EnvVars::Inherit,
        }
    }

    fn add_mount(&mut self, mount: Mount) -> &mut Self {
        self.mounts.push(mount);
        self
    }

    fn with_mount(mut self, mount: Mount) -> Self {
        self.add_mount(mount);
        self
    }

    fn add_mounts(&mut self, mounts: impl IntoIterator<Item = Mount>) {
        self.mounts.extend(mounts);
    }

    fn with_mounts(mut self, mounts: impl IntoIterator<Item = Mount>) -> Self {
        self.add_mounts(mounts);
        self
    }

    fn add_symlink(&mut self, symlink: Symlink) -> &mut Self {
        self.symlinks.push(symlink);
        self
    }

    fn with_symlink(mut self, symlink: Symlink) -> Self {
        self.add_symlink(symlink);
        self
    }

    fn add_symlinks(&mut self, symlinks: impl IntoIterator<Item = Symlink>) -> &mut Self {
        self.symlinks.extend(symlinks);
        self
    }

    fn with_symlinks(mut self, symlinks: impl IntoIterator<Item = Symlink>) -> Self {
        self.add_symlinks(symlinks);
        self
    }

    fn set_program(&mut self, program: PathBuf) -> &mut Self {
        self.program = Some(program);
        self
    }

    fn with_program(mut self, program: PathBuf) -> Self {
        self.set_program(program);
        self
    }

    fn with_detach_stdout(mut self, detach_stdout: bool) -> Self {
        self.detach_output = detach_stdout;
        self
    }

    fn with_new_session(mut self, setsid: bool) -> Self {
        self.new_session = setsid;
        self
    }

    fn with_inherit_env(mut self, inherit: bool) -> Self {
        self.envvars = if inherit {
            EnvVars::Inherit
        } else {
            EnvVars::Set(Vec::new())
        };
        self
    }

    fn add_envvar(&mut self, id: OsString, value: OsString) -> &mut Self {
        self.envvars.set_mut().push((id, value));
        self
    }

    fn with_envvar(mut self, id: OsString, value: OsString) -> Self {
        self.add_envvar(id, value);
        self
    }

    fn spawn(self) -> io::Result<Child> {
        let mut cmd = Command::new("bwrap");
        for mount in self.mounts {
            match mount {
                Mount::Touch { sbx_path } => {
                    cmd.args([OsStr::new("--dir"), sbx_path.0.as_os_str()]);
                }
                Mount::Fs {
                    readonly,
                    host_path,
                    sbx_path,
                } => {
                    let bind_flag: &OsStr = if readonly {
                        OsStr::new("--ro-bind")
                    } else {
                        OsStr::new("--bind")
                    };
                    cmd.args([
                        bind_flag,
                        host_path.0.as_os_str(),
                        sbx_path.0.as_os_str(),
                    ]);
                }
            }
        }

        for symlink in self.symlinks {
            cmd.args([
                OsStr::new("--symlink"),
                symlink.src.0.as_os_str(),
                symlink.dest.0.as_os_str(),
            ]);
        }

        if let Some(path) = self.path {
            cmd.args([
                OsStr::new("--set-env"),
                OsStr::new("PATH"),
                path.as_os_str(),
            ]);
        }

        if let Some(chdir) = self.chdir {
            cmd.args([OsStr::new("--chdir"), chdir.as_os_str()]);
        }

        if self.unshare_pid {
            cmd.arg("--unshare-pid");
        }

        if self.new_session {
            eprintln!("[unpak] WARNING: setsid will break job control.");
            cmd.arg("--new-session");
        }

        match self.envvars {
            EnvVars::Inherit => eprintln!("[unpak] WARNING: environment variables are inherited"),
            EnvVars::Set(list) => {
                if list.is_empty() {
                    cmd.arg("--clearenv");
                } else {
                    cmd.args(
                        list.iter()
                            .flat_map(|(id, value)| [OsStr::new("--setenv"), id, value]),
                    );
                }
            }
        }

        if !self.new_session && !self.detach_output {
            eprintln!("[unpak] WARNING: sandbox escape may be possible because process can control terminal.");
        }

        cmd.arg(
            self.program
                .expect("a program to run in the sandbox is required"),
        );

        if self.detach_output {
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());
        }

        cmd.spawn()
    }
}

fn launch_bubblewrap(proc: &Path, mounts: impl IntoIterator<Item = Mount>) {
    let mut builder = Bubblewrap::new();

    // essential directories, even if empty.
    builder.add_mount(Mount::Touch { sbx_path: "/usr/sbin".into() });
    builder.add_mount(Mount::Touch { sbx_path: "/usr/bin".into() });

    builder.add_mounts(mounts);

    // ld-linux
    builder.add_mount(Mount::Fs {
        readonly: true,
        host_path: INTERPRETER_HOST.into(),
        sbx_path: SBX_LD_LINUX.into(),
    });

    builder.add_symlinks([
        Symlink {
            src: SBX_LD_LINUX.into(),
            dest: "/usr/lib64/ld-linux-x86-64.so.2".into(),
        },
        Symlink {
            src: "/usr/lib".into(),
            dest: "/lib".into(),
        },
        Symlink {
            src: "/usr/lib64".into(),
            dest: "/lib64".into(),
        },
        Symlink {
            src: "/usr/bin".into(),
            dest: "/bin".into(),
        },
        Symlink {
            src: "/usr/sbin".into(),
            dest: "/sbin".into(),
        },
    ]);

    let mut proc = builder
        .with_program(proc.to_path_buf())
        .with_inherit_env(false)
        .spawn()
        .unwrap();

    let exit_code = proc.wait().unwrap();
    eprintln!("[unpak] sandbox exited with code {exit_code}");
}

#[derive(Serialize, Deserialize)]
struct BuildCmd {
    program: PathBuf,
    arguments: Vec<String>,
}

#[derive(Serialize, Deserialize)]
enum BuildProcess {
    Cmds(Vec<BuildCmd>),
}

#[derive(Subcommand, Debug)]
enum Action {
    Build {
        /// The project manifest file
        project: PathBuf,
    },
}

/// unpak, the source-based package manager without dependency hell
#[derive(Parser, Debug)]
struct Arguments {
    #[command(subcommand)]
    action: Action,
}

fn main() {
    // let args = Arguments::parse();

    //patch_bootstrap(Path::new("./bash"));
    // TODO: Get ELF interpreter for current binary

    #[rustfmt::skip]
    let mounts = [
	// begin shared libraries
	(   // libtinfo, dependency of bash
	    PathBuf::from("/usr/lib/x86_64-linux-gnu/libtinfo.so.6"),
	    StdMountLocation::UserSo,
	).into(),
	(   // libc, dependency of bash
	    PathBuf::from("/usr/lib/x86_64-linux-gnu/libc.so.6"),
	    StdMountLocation::UserSo,
	).into(),
	(   // libselinux, dependency of ls
	    PathBuf::from("/usr/lib/x86_64-linux-gnu/libselinux.so.1"),
	    StdMountLocation::UserSo,
	).into(),
	(   // libpcre2-8
	    PathBuf::from("/usr/lib/x86_64-linux-gnu/libpcre2-8.so.0"),
	    StdMountLocation::UserSo,
	).into(),

	// begin executables
	(   // bash
	    PathBuf::from("/usr/bin/bash"),
	    StdMountLocation::UserExe,
	).into(),
	(   // ls
	    PathBuf::from("/usr/bin/ls"),
	    StdMountLocation::UserExe,
	).into()
    ];

    launch_bubblewrap(Path::new("/usr/bin/bash"), mounts);

    /*match args.action {
        Action::Build {
            project: project_path,
        } => {
            let project: SourceProject =
                toml::from_str(&std::fs::read_to_string(project_path).unwrap()).unwrap();
            project.build();
        }
    }

    let project = SourceProject {
        id: ProjectId("com.github.osten.unpak".to_string()),
        build: BuildProcess::Cmds(vec![]),
        libraries: vec![ProjectId("org.gnu.glibc".to_string())],
    };
    eprintln!("{}", toml::to_string_pretty(&project).unwrap());*/
}

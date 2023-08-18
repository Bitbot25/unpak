//! unpak, the modern package manager
//! unpak needs a bootstrap GCC to compile the sandboxed-GCC
//! |bootstrap GCC and programs| -> |stage1 GCC| -> |stage2 GCC| -> coreutils and build deps, etc
//! stage1 is cross-compiled by the bootstrapper, and
//! stage2 is compiled by stage1 to ensure full sandboxing.

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::{
    collections::HashMap, ffi::OsString, fs::File, io::ErrorKind, path::PathBuf, process::Command,
};

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

const INTERPRETER: &str = "/lib64/ld-linux-x86-64.so.2";

fn patch_bootstrap(program: &Path) {
    let mut command = Command::new("patchelf");
    command.args(["--set-interpreter", INTERPRETER]);
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

enum MountLocation {
    UserExe,
    UserSo,
}

fn launch_bubblewrap(interpreter: &Path, bdeps: &[(MountLocation, PathBuf)]) {
    let robinds: Vec<String> = bdeps
        .iter()
        .flat_map(|(loc, dep)| {
            dbg!(dep);
            // TODO: Do we need to resolve it all the way down, or does bubblewrap do that itself?
            // in that case, we can just remove the `which` dependency.
            let canon_path = which::which(dep).unwrap();
            let mount_path = match loc {
                MountLocation::UserExe => "/usr/bin",
                MountLocation::UserSo => "/usr/lib",
            };
            [
                "--ro-bind".to_string(),
                canon_path.into_os_string().into_string().unwrap(),
                format!(
                    "{mount_path}/{}",
                    dep.file_name()
                        .unwrap()
                        .to_os_string()
                        .into_string()
                        .unwrap()
                ),
            ]
            .into_iter()
        })
        .collect();

    dbg!(&robinds);

    let mut command = Command::new("bwrap");
    /* Mount dependencies */
    command.args(robinds);
    /* Mount interpreter (avoids "execvp: no such file or directory") */
    command.args([
        "--ro-bind",
        &interpreter.as_os_str().to_string_lossy(),
        INTERPRETER,
    ]);
    /* Set the PATH variable */
    command.args(["--setenv", "PATH", "/usr/bin"]);
    /* Change directory to '/' */
    command.args(["--chdir", "/"]);

    /* special filesystems*/
    command.args(["--proc", "/proc"]);
    command.args(["--dev", "/dev"]);

    /* unshare */
    // --new-session breaks job control with setsid
    //command.args(["--unshare-pid", "--new-session"]);
    command.arg("--unshare-pid");
    
    /* Set build process */
    command.arg("/usr/bin/bash");

    let mut proc = match command.spawn() {
        Ok(proc) => proc,
        Err(e) => {
            match e.kind() {
                ErrorKind::NotFound => {
                    eprintln!("error: bwrap not found. Is bubblewrap installed?")
                }
                _ => eprintln!("error: unknown error: {e:?}"),
            };
            return;
        }
    };

    let exit = proc.wait().unwrap();
    eprintln!("[unpak] build container exited with code {exit}");
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

const PROJ_MINIZ: &str = "com.github.richgel999.miniz";

fn main() {
    // let args = Arguments::parse();

    patch_bootstrap(Path::new("./bash"));
    // TODO: Get ELF interpreter for current binary
    launch_bubblewrap(
        Path::new(
            "/nix/store/ayg065nw0xi1zsyi8glfh5pn4sfqd8xg-glibc-2.37-8/lib/ld-linux-x86-64.so.2",
        ),
        &[
	    // libreadline
            (
                MountLocation::UserSo,
                "/nix/store/4f5dbbbh05f87xi8b3lgs653gs5bpb6d-readline-8.2p1/lib/libreadline.so.8"
                    .into(),
            ),
	    // libhistory
	    (
		MountLocation::UserSo,
		"/nix/store/4f5dbbbh05f87xi8b3lgs653gs5bpb6d-readline-8.2p1/lib/libhistory.so.8".into()
	    ),
	    // libncursesw
	    (
		MountLocation::UserSo,
		"/nix/store/gmx0dj8kvl7agm6azrbgv9w3k4kp844y-ncurses-6.4/lib/libncursesw.so.6".into(),
            ),
	    // libdl
	    (
		MountLocation::UserSo,
		"/nix/store/ayg065nw0xi1zsyi8glfh5pn4sfqd8xg-glibc-2.37-8/lib/libdl.so.2".into()
	    ),
	    // libc
	    (
		MountLocation::UserSo,
		"/nix/store/ayg065nw0xi1zsyi8glfh5pn4sfqd8xg-glibc-2.37-8/lib/libc.so.6".into()
	    ),
	    // bash
            (MountLocation::UserExe, "./bash".into()),
        ],
    )

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

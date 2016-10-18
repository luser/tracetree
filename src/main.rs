#[macro_use] extern crate log;
extern crate env_logger;
extern crate libc;
extern crate nix;
extern crate spawn_ptrace;

use libc::{c_long, pid_t};
use nix::c_void;
use nix::sys::ptrace::{ptrace, ptrace_setoptions};
use nix::sys::ptrace::ptrace::{PTRACE_O_TRACEEXEC, PTRACE_O_TRACEFORK, PTRACE_O_TRACEVFORK, PTRACE_GETEVENTMSG, PTRACE_CONT};
use nix::sys::wait::{waitpid, WaitStatus, PtraceEvent};
use spawn_ptrace::CommandPtraceSpawn;
use std::borrow::Cow;
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::ptr;
use std::str;
use std::time::Instant;

struct ProcessInfo {
    pub children: Vec<pid_t>,
    pub started: Instant,
    // None if still alive.
    pub ended: Option<Instant>,
    pub cmdline: Vec<String>,
}

impl Default for ProcessInfo {
    fn default() -> ProcessInfo {
        ProcessInfo {
            children: vec!(),
            started: Instant::now(),
            ended: None,
            cmdline: vec!(),
        }
    }
}

fn continue_process(pid: pid_t) -> nix::Result<c_long> {
    ptrace(PTRACE_CONT, pid, ptr::null_mut(), ptr::null_mut())
}

//TODO: enumerate in proper order
fn enumerate_pids<'a>(_start_pid: pid_t, pids: &'a HashMap<pid_t, ProcessInfo>) -> Box<Iterator<Item=(&'a pid_t, &'a ProcessInfo)> + 'a> {
    Box::new(pids.iter())
}

fn main() {
    env_logger::init().unwrap();

    let args = env::args().skip(1).collect::<Vec<_>>();
    let child = Command::new(&args[0]).args(&args[1..]).spawn_ptrace().unwrap();
    let pid = child.id() as pid_t;
    trace!("Spawned process {}", pid);
    // Setup our ptrace options
    ptrace_setoptions(pid, PTRACE_O_TRACEEXEC | PTRACE_O_TRACEFORK | PTRACE_O_TRACEVFORK).expect("Failed to set ptrace options");
    let mut pids = HashMap::new();
    pids.insert(pid, ProcessInfo { cmdline: args, .. ProcessInfo::default() });
    continue_process(pid).expect("Error continuing process");
    loop {
        match waitpid(-1, None) {
            Ok(WaitStatus::Exited(pid, ret)) => {
                trace!("Process {} exited with status {}", pid, ret);
                match pids.get_mut(&pid) {
                    Some(info) => {
                        assert!(info.ended.is_none());
                        info.ended = Some(Instant::now());
                    }
                    None => panic!("Got an exit event for unknown pid {}", pid),
                }
                if !pids.values().any(|info| info.ended.is_none()) {
                    break
                }
            }
            Ok(WaitStatus::StoppedPtraceEvent(pid, event)) => {
                match event {
                    PtraceEvent::Fork | PtraceEvent::Vfork => {
                        let mut new_pid: pid_t = 0;
                        ptrace(PTRACE_GETEVENTMSG, pid, ptr::null_mut(), &mut new_pid as *mut pid_t as *mut c_void).expect("Failed to get pid of forked process");
                        trace!("Forked new process {}", new_pid);
                        pids.insert(new_pid, ProcessInfo::default());
                        match pids.get_mut(&pid) {
                            Some(info) => {
                                info.children.push(new_pid);
                            }
                            None => panic!("Got a fork event for unknown pid {}", pid),
                        }
                    }
                    PtraceEvent::Exec => {
                        let mut buf = vec!();
                        match pids.get_mut(&pid) {
                            Some(info) => {
                                File::open(format!("/proc/{}/cmdline", pid))
                                    .and_then(|mut f| f.read_to_end(&mut buf))
                                    .and_then(|_| {
                                        info.cmdline = buf.split(|&b| b == 0).map(|bytes| String::from_utf8_lossy(bytes).into_owned()).collect::<Vec<_>>();
                                        debug!("[{}] {}", pid, info.cmdline.join(" "));
                                        Ok(())
                                    })
                                    .expect("Couldn't read cmdline");
                            }
                            None => panic!("Got an exec event for unknown pid {}", pid),
                        }
                    }
                    _ => panic!("Unexpected ptrace event: {:?}", event),
                }
                continue_process(pid).expect("Error continuing process");
            }
            Ok(WaitStatus::Stopped(pid, sig)) => {
                trace!("Process {} is stopped with {:?}", pid, sig);
                continue_process(pid).expect("Error continuing process");
            }
            Ok(s) => panic!("Unexpected status: {:?}", s),
            Err(e) => panic!("ptrace error: {:?}", e),
        }
    }
    let elapsed = Instant::now() - pids.get(&pid).unwrap().started;
    trace!("Done: total time: {}.{:03}s", elapsed.as_secs(), elapsed.subsec_nanos() / 1000);
    for (pid, info) in enumerate_pids(pid, &pids) {
        let p = Path::new(&info.cmdline[0])
            .file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or(Cow::Borrowed("<unknown>"));
        println!("{}: {}", pid, p);
    }
}

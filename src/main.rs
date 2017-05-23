#[macro_use] extern crate log;
extern crate env_logger;
extern crate indextree;
extern crate libc;
extern crate nix;
extern crate spawn_ptrace;

use indextree::{Arena, NodeEdge, NodeId};
use libc::{c_long, pid_t};
use nix::c_void;
use nix::sys::ptrace::{ptrace, ptrace_setoptions};
use nix::sys::ptrace::ptrace::{PTRACE_O_TRACECLONE, PTRACE_O_TRACEEXEC, PTRACE_O_TRACEFORK, PTRACE_O_TRACEVFORK, PTRACE_GETEVENTMSG, PTRACE_CONT};
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
use std::time::{Duration,Instant};

struct ProcessInfo {
    pub pid: pid_t,
    pub started: Instant,
    // None if still alive.
    pub ended: Option<Instant>,
    pub cmdline: Vec<String>,
}

impl Default for ProcessInfo {
    fn default() -> ProcessInfo {
        ProcessInfo {
            pid: 0,
            started: Instant::now(),
            ended: None,
            cmdline: vec!(),
        }
    }
}

fn continue_process(pid: pid_t) -> nix::Result<c_long> {
    ptrace(PTRACE_CONT, pid, ptr::null_mut(), ptr::null_mut())
}

fn fmt_duration(duration: Duration) -> String {
    format!("{}.{:03}s", duration.as_secs(), duration.subsec_nanos() / 1000_000)
}

fn insert_pid(pid: pid_t, arena: &mut Arena<ProcessInfo>, map: &mut HashMap<pid_t, NodeId>) -> NodeId {
    let node = arena.new_node(ProcessInfo { pid: pid, .. ProcessInfo::default() });
    map.insert(pid, node);
    node
}

fn print_process_tree(root: NodeId, arena: &mut Arena<ProcessInfo>) {
    let mut depth = 0;
    for i in root.traverse(arena) {
        match i {
            NodeEdge::Start(node) => {
                let info = &arena[node].data;
                let p = info.cmdline.first()
                    .and_then(|b| Path::new(b)
                              .file_name()
                              .map(|s| s.to_string_lossy()))
                    .unwrap_or(Cow::Borrowed("<unknown>"));
                let cmdline = info.cmdline[1..].join(" ");
                println!("{}{} {} {} [{}]", "\t".repeat(depth), info.pid, p, cmdline, info.ended.map(|e| fmt_duration(e - info.started)).unwrap_or("?".to_owned()));

                depth += 1;
            }
            NodeEdge::End(_) => {
                depth -= 1;
            }
        }
    }
}

fn main() {
    env_logger::init().unwrap();

    let args = env::args().skip(1).collect::<Vec<_>>();
    let child = Command::new(&args[0]).args(&args[1..]).spawn_ptrace().unwrap();
    let pid = child.id() as pid_t;
    trace!("Spawned process {}", pid);
    // Setup our ptrace options
    ptrace_setoptions(pid, PTRACE_O_TRACEEXEC | PTRACE_O_TRACEFORK | PTRACE_O_TRACEVFORK | PTRACE_O_TRACECLONE).expect("Failed to set ptrace options");
    let arena = &mut Arena::new();
    let mut pids: HashMap<pid_t, NodeId> = HashMap::new();
    let root = insert_pid(pid, arena, &mut pids);
    arena[root].data.cmdline = args;
    continue_process(pid).expect("Error continuing process");
    loop {
        match waitpid(-1, None) {
            Ok(WaitStatus::Exited(pid, ret)) => {
                trace!("Process {} exited with status {}", pid, ret);
                let node = pids[&pid];
                arena[node].data.ended = Some(Instant::now());
                if !root.descendants(arena).any(|node| arena[node].data.ended.is_none()) {
                    break
                }
            }
            Ok(WaitStatus::StoppedPtraceEvent(pid, event)) => {
                match event {
                    PtraceEvent::Fork | PtraceEvent::Vfork | PtraceEvent::Clone => {
                        let mut new_pid: pid_t = 0;
                        ptrace(PTRACE_GETEVENTMSG, pid, ptr::null_mut(), &mut new_pid as *mut pid_t as *mut c_void).expect("Failed to get pid of forked process");
                        trace!("{:?} new process {}", event, new_pid);
                        let parent = pids[&pid];
                        let child = insert_pid(new_pid, arena, &mut pids);
                        parent.append(child, arena);
                    }
                    PtraceEvent::Exec => {
                        let mut buf = vec!();
                        match pids.get(&pid) {
                            Some(&node) => {
                                File::open(format!("/proc/{}/cmdline", pid))
                                    .and_then(|mut f| f.read_to_end(&mut buf))
                                    .and_then(|_| {
                                        let mut cmdline = buf.split(|&b| b == 0).map(|bytes| String::from_utf8_lossy(bytes).into_owned()).collect::<Vec<_>>();
                                        cmdline.pop();
                                        debug!("[{}] {:?}", pid, cmdline);
                                        arena[node].data.cmdline = cmdline;
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
    let elapsed = arena[root].data.started.elapsed();
    trace!("Done: total time: {}", fmt_duration(elapsed));
    print_process_tree(root, arena);
}

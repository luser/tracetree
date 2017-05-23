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
use nix::sys::signal;
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
use std::sync::atomic::{AtomicBool, Ordering, ATOMIC_BOOL_INIT};
use std::time::{Duration,Instant};

static SIGNAL_DELIVERED: AtomicBool = ATOMIC_BOOL_INIT;

extern fn handle_signal(_:i32) {
    SIGNAL_DELIVERED.store(true, Ordering::Relaxed);
}

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

fn continue_process(pid: pid_t, signal: Option<signal::Signal>) -> nix::Result<c_long> {
    let data = signal.map(|s| s as i32 as *mut c_void).unwrap_or(ptr::null_mut());
    ptrace(PTRACE_CONT, pid, ptr::null_mut(), data)
}

fn fmt_duration(duration: Duration) -> String {
    format!("{}.{:03}s", duration.as_secs(), duration.subsec_nanos() / 1000_000)
}

fn get_or_insert_pid(pid: pid_t, arena: &mut Arena<ProcessInfo>, map: &mut HashMap<pid_t, NodeId>) -> NodeId {
    *map.entry(pid).or_insert_with(|| {
        arena.new_node(ProcessInfo { pid: pid, .. ProcessInfo::default() })
    })
}

fn print_process_tree<F>(root: NodeId, arena: &mut Arena<ProcessInfo>,
                         filter: F)
    where F: Fn(&ProcessInfo) -> bool,
{
    let mut depth = 0;
    for i in root.traverse(arena) {
        match i {
            NodeEdge::Start(node) => {
                let info = &arena[node].data;
                if filter(info) {
                    let p = info.cmdline.first()
                        .and_then(|b| Path::new(b)
                                  .file_name()
                                  .map(|s| s.to_string_lossy()))
                        .unwrap_or(Cow::Borrowed("<unknown>"));
                    let cmdline = if info.cmdline.len() > 1 {
                        let mut c = info.cmdline[1..].join(" ");
                        c.push(' ');
                        c
                    } else {
                        "".to_string()
                    };
                    println!("{}{} {} {}[{}]", "\t".repeat(depth), info.pid, p, cmdline, info.ended.map(|e| fmt_duration(e - info.started)).unwrap_or("?".to_owned()));
                }
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

    let sig_action = signal::SigAction::new(signal::SigHandler::Handler(handle_signal),
                                            signal::SaFlags::empty(),
                                            signal::SigSet::empty());
    unsafe { signal::sigaction(signal::SIGUSR1, &sig_action).unwrap(); }

    trace!("This pid: {}", nix::unistd::getpid());
    let args = env::args().skip(1).collect::<Vec<_>>();
    let child = Command::new(&args[0]).args(&args[1..]).spawn_ptrace().unwrap();
    let pid = child.id() as pid_t;
    trace!("Spawned process {}", pid);
    // Setup our ptrace options
    ptrace_setoptions(pid, PTRACE_O_TRACEEXEC | PTRACE_O_TRACEFORK | PTRACE_O_TRACEVFORK | PTRACE_O_TRACECLONE).expect("Failed to set ptrace options");
    let arena = &mut Arena::new();
    let mut pids: HashMap<pid_t, NodeId> = HashMap::new();
    let root = get_or_insert_pid(pid, arena, &mut pids);
    arena[root].data.cmdline = args;
    continue_process(pid, None).expect("Error continuing process");
    loop {
        if !root.descendants(arena).any(|node| arena[node].data.ended.is_none()) {
            break
        }
        match waitpid(-1, None) {
            Ok(WaitStatus::Exited(pid, ret)) => {
                trace!("Process {} exited with status {}", pid, ret);
                let node = get_or_insert_pid(pid, arena, &mut pids);
                arena[node].data.ended = Some(Instant::now());
            }
            Ok(WaitStatus::StoppedPtraceEvent(pid, event)) => {
                match event {
                    PtraceEvent::Fork | PtraceEvent::Vfork | PtraceEvent::Clone => {
                        let mut new_pid: pid_t = 0;
                        ptrace(PTRACE_GETEVENTMSG, pid, ptr::null_mut(),
                               &mut new_pid as *mut pid_t as *mut c_void)
                            .expect("Failed to get pid of forked process");
                        trace!("[{}] {:?} new process {}", pid, event, new_pid);
                        match pids.get(&pid) {
                            Some(&parent) => {
                                let cmdline = arena[parent].data.cmdline[..1].to_vec();
                                let child = get_or_insert_pid(new_pid, arena, &mut pids);
                                arena[child].data.cmdline = cmdline;
                                parent.append(child, arena);
                            }
                            None => panic!("Got an {:?} event for unknown parent pid {}", event,
                                           pid),
                        }
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
                                        debug!("[{}] exec {:?}", pid, cmdline);
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
                continue_process(pid, None).expect("Error continuing process");
            }
            Ok(WaitStatus::Stopped(pid, sig)) => {
                trace!("[{}] stopped with {:?}", pid, sig);
                // Sometimes we get the SIGSTOP+exit from a child before we get the clone
                // stop from the parent, so insert any unknown pids here so we have a better
                // approximation of the process start time.
                get_or_insert_pid(pid, arena, &mut pids);
                let continue_sig = if sig == signal::Signal::SIGSTOP { None } else { Some(sig) };
                continue_process(pid, continue_sig).expect("Error continuing process");
            }
            Ok(s) => panic!("Unexpected status: {:?}", s),
            Err(e) => {
                match e {
                    nix::Error::Sys(nix::Errno::EINTR) => {
                        if SIGNAL_DELIVERED.swap(false, Ordering::Relaxed) {
                            println!("Active processes:");
                            print_process_tree(root, arena, |info| info.ended.is_none());
                        }
                    }
                    _ => panic!("ptrace error: {:?}", e),
                }
            }
        }
    }
    let elapsed = arena[root].data.started.elapsed();
    trace!("Done: total time: {}", fmt_duration(elapsed));
    print_process_tree(root, arena, |_| true);
}

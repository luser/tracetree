#[macro_use] extern crate log;
#[macro_use]
extern crate error_chain;
extern crate indextree;
extern crate libc;
extern crate nix;
extern crate spawn_ptrace;

mod errors;

pub use errors::*;
use indextree::{Arena, NodeId};
pub use indextree::NodeEdge;
use libc::{c_long, pid_t};
use nix::c_void;
use nix::sys::ptrace::{ptrace, ptrace_setoptions};
use nix::sys::ptrace::ptrace::{PTRACE_EVENT_FORK, PTRACE_EVENT_VFORK, PTRACE_EVENT_CLONE,
                               PTRACE_EVENT_EXEC};
use nix::sys::ptrace::ptrace::{PTRACE_O_TRACECLONE, PTRACE_O_TRACEEXEC, PTRACE_O_TRACEFORK,
                               PTRACE_O_TRACEVFORK, PTRACE_GETEVENTMSG, PTRACE_CONT};
use nix::sys::signal;
use nix::sys::wait::{waitpid, WaitStatus};
use spawn_ptrace::CommandPtraceSpawn;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::process::Command;
use std::ptr;
use std::time::Instant;

/// Information about a spawned process.
pub struct ProcessInfo {
    /// The process ID.
    pub pid: pid_t,
    /// When the process was started.
    pub started: Instant,
    /// When the process ended, or `None` if it is still running.
    pub ended: Option<Instant>,
    /// The commandline with which this process was executed.
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

/// A tree of processes.
pub struct ProcessTree {
    arena: Arena<ProcessInfo>,
    pids: HashMap<pid_t, NodeId>,
    root: NodeId,
}

impl ProcessTree {
    /// Execute `cmd`, tracking all child processes it spawns, and return a `ProcessTree` listing
    /// them.
    pub fn spawn<T>(mut cmd: Command, cmdline: &[T]) -> Result<ProcessTree>
        where T: AsRef<str>
    {
        let child = cmd.spawn_ptrace().chain_err(|| "Error spawning process")?;
        let pid = child.id() as pid_t;
        trace!("Spawned process {}", pid);
        // Setup our ptrace options
        ptrace_setoptions(pid, PTRACE_O_TRACEEXEC | PTRACE_O_TRACEFORK | PTRACE_O_TRACEVFORK | PTRACE_O_TRACECLONE).chain_err(|| "Error setting ptrace options")?;
        let mut arena = Arena::new();
        let mut pids = HashMap::new();
        let root = get_or_insert_pid(pid, &mut arena, &mut pids);
        //FIXME
        arena[root].data.cmdline = cmdline.iter().map(|s| s.as_ref().to_string()).collect();
        continue_process(pid, None).chain_err(|| "Error continuing process")?;
        loop {
            if !root.descendants(&arena).any(|node| arena[node].data.ended.is_none()) {
                break
            }
            match waitpid(-1, None) {
                Ok(WaitStatus::Exited(pid, ret)) => {
                    trace!("Process {} exited with status {}", pid, ret);
                    let node = get_or_insert_pid(pid, &mut arena, &mut pids);
                    arena[node].data.ended = Some(Instant::now());
                }
                Ok(WaitStatus::PtraceEvent(pid, _sig, event)) => {
                    match event {
                        PTRACE_EVENT_FORK | PTRACE_EVENT_VFORK | PTRACE_EVENT_CLONE => {
                            let mut new_pid: pid_t = 0;
                            ptrace(PTRACE_GETEVENTMSG, pid, ptr::null_mut(),
                                   &mut new_pid as *mut pid_t as *mut c_void)
                                .chain_err(|| "Failed to get pid of forked process")?;
                            let name = match event {
                                PTRACE_EVENT_FORK => "fork",
                                PTRACE_EVENT_VFORK => "vfork",
                                PTRACE_EVENT_CLONE => "clone",
                                _ => unreachable!(),
                            };
                            trace!("[{}] {} new process {}", pid, name, new_pid);
                            match pids.get(&pid) {
                                Some(&parent) => {
                                    let cmdline = {
                                        let parent_data = &arena[parent].data;
                                        if parent_data.cmdline.len() > 1 {
                                            parent_data.cmdline[..1].to_vec()
                                        } else {
                                            vec![]
                                        }
                                    };
                                    let child = get_or_insert_pid(new_pid, &mut arena, &mut pids);
                                    arena[child].data.cmdline = cmdline;
                                    parent.append(child, &mut arena);
                                }
                                None => bail!("Got an {:?} event for unknown parent pid {}", event,
                                              pid),
                            }
                        }
                        PTRACE_EVENT_EXEC => {
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
                                        .chain_err(|| "Couldn't read cmdline")?;
                                }
                                None => bail!("Got an exec event for unknown pid {}", pid),
                            }
                        }
                        _ => panic!("Unexpected ptrace event: {:?}", event),
                    }
                    continue_process(pid, None).chain_err(|| "Error continuing process")?;
                }
                Ok(WaitStatus::Stopped(pid, sig)) => {
                    trace!("[{}] stopped with {:?}", pid, sig);
                    // Sometimes we get the SIGSTOP+exit from a child before we get the clone
                    // stop from the parent, so insert any unknown pids here so we have a better
                    // approximation of the process start time.
                    get_or_insert_pid(pid, &mut arena, &mut pids);
                    let continue_sig = if sig == signal::Signal::SIGSTOP { None } else { Some(sig) };
                    continue_process(pid, continue_sig).chain_err(|| "Error continuing process")?;
                }
                Ok(s) => bail!("Unexpected process status: {:?}", s),
                Err(e) => {
                    match e {
                        nix::Error::Sys(nix::Errno::EINTR) => {
                            /*FIXME
                            if SIGNAL_DELIVERED.swap(false, Ordering::Relaxed) {
                                println!("Active processes:");
                                print_process_tree(root, arena, |info| info.ended.is_none());
                            }
                             */
                        }
                        _ => bail!("ptrace error: {:?}", e),
                    }
                }
            }
        }
        Ok(ProcessTree {
            arena: arena,
            pids: pids,
            root: root,
        })
    }

    /// Iterate over processes in the tree in tree order.
    pub fn traverse<'a>(&'a self) -> Traverse<'a> {
        Traverse {
            inner: self.root.traverse(&self.arena),
            arena: &self.arena,
        }
    }

    /// Look up a process in the tree by pid.
    pub fn get(&self, pid: pid_t) -> Option<&ProcessInfo> {
        match self.pids.get(&pid) {
            None => None,
            Some(&node) => Some(&self.arena[node].data),
        }
    }
}

pub struct Traverse<'a> {
    inner: indextree::Traverse<'a, ProcessInfo>,
    arena: &'a Arena<ProcessInfo>,
}

impl<'a> Iterator for Traverse<'a> {
    type Item = NodeEdge<&'a ProcessInfo>;

    fn next(&mut self) -> Option<NodeEdge<&'a ProcessInfo>> {
        match self.inner.next() {
            None => None,
            Some(NodeEdge::Start(node)) => {
                Some(NodeEdge::Start(&self.arena[node].data))
            }
            Some(NodeEdge::End(node)) => {
                Some(NodeEdge::End(&self.arena[node].data))
            }
        }
    }
}

fn get_or_insert_pid(pid: pid_t, arena: &mut Arena<ProcessInfo>, map: &mut HashMap<pid_t, NodeId>) -> NodeId {
    *map.entry(pid).or_insert_with(|| {
        arena.new_node(ProcessInfo { pid: pid, .. ProcessInfo::default() })
    })
}

fn continue_process(pid: pid_t, signal: Option<signal::Signal>) -> nix::Result<c_long> {
    let data = signal.map(|s| s as i32 as *mut c_void).unwrap_or(ptr::null_mut());
    ptrace(PTRACE_CONT, pid, ptr::null_mut(), data)
}

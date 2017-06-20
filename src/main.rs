#[macro_use] extern crate log;
extern crate env_logger;
extern crate libc;
extern crate nix;
extern crate tracetree;

use nix::sys::signal;
use std::borrow::Cow;
use std::env;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::str;
use std::sync::atomic::{AtomicBool, Ordering, ATOMIC_BOOL_INIT};
use std::time::Duration;
use tracetree::{NodeEdge, ProcessInfo, ProcessTree};

static SIGNAL_DELIVERED: AtomicBool = ATOMIC_BOOL_INIT;

extern fn handle_signal(_:i32) {
    SIGNAL_DELIVERED.store(true, Ordering::Relaxed);
}

fn fmt_duration(duration: Duration) -> String {
    format!("{}.{:03}s", duration.as_secs(), duration.subsec_nanos() / 1000_000)
}

fn print_process_tree<F>(tree: &ProcessTree, filter: F)
    where F: Fn(&ProcessInfo) -> bool,
{
    let mut depth = 0;
    for i in tree.traverse() {
        match i {
            NodeEdge::Start(info) => {
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
    unsafe {
        signal::sigaction(signal::SIGUSR1, &sig_action).expect("Failed to install signal handler!");
    }

    trace!("This pid: {}", nix::unistd::getpid());
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() == 0 {
        drop(writeln!(io::stderr(), "Usage: tracetree command"));
        ::std::process::exit(1);
    }
    let mut cmd = Command::new(&args[0]);
    cmd.args(&args[1..]);
    let tree = ProcessTree::spawn(cmd, &args)
        .expect("Failed to spawn process");
    print_process_tree(&tree, |_| true);
}

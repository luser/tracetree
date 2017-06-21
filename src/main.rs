#[macro_use]
extern crate clap;
#[macro_use] extern crate log;
extern crate env_logger;
extern crate libc;
extern crate nix;
extern crate serde_json;
extern crate tracetree;

use clap::{Arg, App, AppSettings};
use nix::sys::signal;
use std::borrow::Cow;
use std::fs::File;
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

arg_enum!{
    #[derive(Debug)]
    #[allow(non_camel_case_types)]
    pub enum OutputFormat {
        text,
        json
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

    let matches = App::new(env!("CARGO_PKG_NAME"))
        .version(env!("CARGO_PKG_VERSION"))
        .setting(AppSettings::TrailingVarArg)
        .arg(Arg::with_name("out")
             .short("o")
             .long("out")
             .value_name("OUTPUT")
             .help("Write output to this file")
             .takes_value(true))
        .arg(Arg::with_name("format")
             .short("f")
             .long("format")
             .value_name("FORMAT")
             .help("Output format")
             .possible_values(&OutputFormat::variants())
             .default_value("text"))
        .arg(Arg::with_name("cmd")
             .help("Command to run")
             .multiple(true)
             .required(true)
             .use_delimiter(false))
        .after_help("You can visualize the JSON output with this web viewer: https://luser.github.io/tracetree/")
        .get_matches();

    let args = matches.values_of("cmd").unwrap().collect::<Vec<_>>();
    let fmt = value_t_or_exit!(matches, "format", OutputFormat);
    let mut cmd = Command::new(&args[0]);
    cmd.args(&args[1..]);
    let tree = ProcessTree::spawn(cmd, &args)
        .expect("Failed to spawn process");
    let stdout = io::stdout();
    let out: Box<Write> = matches.value_of_os("out")
        .and_then(|o| File::create(o).map(|f| Box::new(f) as Box<Write>).ok())
        .unwrap_or_else(|| Box::new(stdout.lock()) as Box<Write>);
    match fmt {
        OutputFormat::text => print_process_tree(&tree, |_| true),
        OutputFormat::json => serde_json::to_writer(out, &tree)
            .expect("Failed to serialize process tree"),
    }
}

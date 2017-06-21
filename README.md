[![Build Status](https://travis-ci.org/luser/tracetree.svg?branch=master)](https://travis-ci.org/luser/tracetree) [![crates.io](https://img.shields.io/crates/v/tracetree.svg)](https://crates.io/crates/tracetree) [![](https://docs.rs/tracetree/badge.svg)](https://docs.rs/tracetree)

tracetree
=========
Run a process, ptrace'ing it and all of its children, and print the entire process tree at the end.

Examples
========

Print a process tree in text format to stdout:

    tracetree /bin/bash -c /bin/true


Print a process tree in JSON format to `output.json`:

    tracetree -f json -o output.json /bin/bash -c /bin/true

JSON output can be viewed with this [web visualizer](https://luser.github.io/tracetree/).

// Copyright © 2016 Felix Obenhuber
// This program is free software. It comes without any warranty, to the extent
// permitted by applicable law. You can redistribute it and/or modify it under
// the terms of the Do What The Fuck You Want To Public License, Version 2, as
// published by Sam Hocevar. See the COPYING file for more details.

use clap::ArgMatches;
use errors::*;
use futures::{Async, Poll, Stream};
use std::io::{self, BufRead, BufReader};
use std::mem;
use std::process::{Command, Stdio};
use super::adb;
use super::record::Record;
use super::terminal::DIMM_COLOR;
use term_painter::ToStyle;
use tokio_core::reactor::Handle;
use tokio_io::AsyncRead;
use tokio_process::{Child, CommandExt};

pub struct LossyLines<A> {
    io: A,
    buffer: Vec<u8>,
}

pub fn lossy_lines<A>(a: A) -> LossyLines<A>
    where A: AsyncRead + BufRead,
{
    LossyLines {
        io: a,
        buffer: Vec::new(),
    }
}

impl<A> Stream for LossyLines<A>
    where A: AsyncRead + BufRead,
{
    type Item = String;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Option<String>, io::Error> {
        let n = try_nb!(self.io.read_until(b'\n', &mut self.buffer));
        if n == 0 && self.buffer.len() == 0 {
            return Ok(None.into())
        }
        self.buffer.pop();
        let mut s = String::from_utf8_lossy(&self.buffer).into_owned();
        self.buffer.clear();
        Ok(Some(mem::replace(&mut s, String::new())).into())
    }
}

type OutStream = Box<Stream<Item = String, Error = ::std::io::Error>>;

pub struct Runner {
    child: Child,
    cmd: String,
    handle: Handle,
    head: Option<usize>,
    output: OutStream,
    restart: bool,
}

impl<'a> Runner {
    pub fn new(args: &ArgMatches<'a>, handle: Handle) -> Result<Self> {
        let adb = format!("{}", adb()?.display());
        let (cmd, restart) = value_t!(args, "COMMAND", String)
            .map(|s| (s, args.is_present("restart")))
            .unwrap_or({
                let mut logcat_args = vec![];
                let mut restart = true;
                if args.is_present("tail") {
                    let count = value_t!(args, "tail", u32).unwrap_or_else(|e| e.exit());
                    logcat_args.push(format!("-t {}", count));
                    restart = false;
                };

                if args.is_present("dump") {
                    logcat_args.push("-d".to_owned());
                    restart = false;
                }
                let cmd = format!("{} logcat -b all {}", adb, logcat_args.join(" "));
                (cmd, restart)
            });
        let (child, output) = Self::run(&cmd, &handle)?;

        Ok(Runner {
            child,
            cmd: cmd.trim().to_owned(),
            handle,
            head: value_t!(args, "head", usize).ok(),
            output,
            restart,
        })
    }

    fn run(cmd: &str, handle: &Handle) -> Result<(Child, OutStream)> {
        let cmd = cmd.split_whitespace()
            .map(|s| s.to_owned())
            .collect::<Vec<String>>();

        let mut child = Command::new(&cmd[0])
            .args(&cmd[1..])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn_async(&handle)?;

        let stdout = child.stdout().take().ok_or("Failed get stdout")?;
        let stderr = child.stderr().take().ok_or("Failed get stderr")?;
        let stdout_reader = BufReader::new(stdout);
        let stderr_reader = BufReader::new(stderr);
        let output = lossy_lines(stdout_reader).select(lossy_lines(stderr_reader)).boxed();
        Ok((child, output))
    }
}

impl Stream for Runner {
    type Item = Option<Record>;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        loop {
            if let Some(c) = self.head {
                if c == 0 {
                    return Ok(Async::Ready(None));
                }
            }
            match self.output.poll() {
                Ok(Async::Ready(t)) => {
                    if let Some(s) = t {
                        let r = Some(Record {
                            raw: s,
                            ..Default::default()
                        });
                        self.head = self.head.map(|c| c - 1);
                        return Ok(Async::Ready(Some(r)));
                    } else {
                        if self.restart {
                            let text = format!("Restarting \"{}\"", self.cmd);
                            println!("{}", DIMM_COLOR.paint(&text));
                            let (child, output) = Self::run(&self.cmd, &self.handle)?;
                            self.output = output;
                            self.child = child;
                        } else {
                            return Ok(Async::Ready(Some(None)));
                        }
                    }
                }
                Ok(Async::NotReady) => return Ok(Async::NotReady),
                Err(e) => return Err(e.into()),
            }
        }
    }
}

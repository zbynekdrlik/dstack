// SPDX-License-Identifier: MIT OR Apache-2.0

//! `asio-probe`: dstack-asio on a real driver, by hand (dstack #243).
//!
//! - `asio-probe` lists the registered ASIO drivers.
//! - `asio-probe --driver <name> [--seconds <s>] [--outputs 1,2]
//!   [--inputs 1,2]` opens the driver as a host (1-based channels; default
//!   outputs 1,2 and no input), plays silence on the outputs, reads the
//!   inputs' peak, prints the runner's status once a second and releases
//!   the driver after `<s>` seconds (default 10). Exit 0 when it ran with
//!   no fault, 1 otherwise.
//!
//! Windows only (a one-line refusal elsewhere, so the Linux coverage
//! counts no glue it can never run). Binary glue only: its decisions are the library's (each tested), and it
//! is run by hand on Windows (out of the mutation gate).

#[cfg(windows)]
mod probe {
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use dstack_asio::{Block, Channels, Process, RunState, Runner, Status};

    /// Silence out, the inputs' peak in (f64 bits).
    struct Listen {
        peak: Arc<AtomicU64>,
    }

    impl Process for Listen {
        fn process(&mut self, block: &mut Block<'_>) {
            let mut peak = 0.0f64;
            for c in 0..block.inputs() {
                for &x in block.input(c) {
                    peak = peak.max(x.abs());
                }
            }
            self.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
            block.zero_outputs();
        }
    }

    fn channels(list: &str) -> Result<Vec<u32>, String> {
        list.split(',')
            .filter(|s| !s.is_empty())
            .map(|s| match s.trim().parse::<u32>() {
                Ok(n) if n >= 1 => Ok(n - 1),
                _ => Err(format!("not a channel number (from 1): {s}")),
            })
            .collect()
    }

    struct Args {
        driver: Option<String>,
        seconds: u64,
        channels: Channels,
    }

    fn parse() -> Result<Args, String> {
        let mut a = Args {
            driver: None,
            seconds: 10,
            channels: Channels {
                inputs: vec![],
                outputs: vec![0, 1],
            },
        };
        let mut it = std::env::args().skip(1);
        while let Some(flag) = it.next() {
            let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
            match flag.as_str() {
                "--driver" => a.driver = Some(value()?),
                "--seconds" => {
                    a.seconds = value()?.parse().map_err(|_| "--seconds: a whole number")?;
                }
                "--outputs" => a.channels.outputs = channels(&value()?)?,
                "--inputs" => a.channels.inputs = channels(&value()?)?,
                "--version" => {
                    println!("asio-probe {}", env!("CARGO_PKG_VERSION"));
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }
        Ok(a)
    }

    fn line(s: &Status, peak: f64) -> String {
        format!(
            "{:?} reason={} rate={} buffer={} in={:?} out={:?} latency in/out={}/{} frames \
             callbacks={} late={} fast={} overloads={} faults={} resets={} gap_max={:.2} ms \
             input_peak={peak:.4}",
            s.state,
            s.reason.as_ref().map_or("-".into(), |r| r.text()),
            s.rate,
            s.buffer_frames,
            s.input_sample,
            s.output_sample,
            s.input_latency_frames,
            s.output_latency_frames,
            s.callbacks,
            s.late_callbacks,
            s.fast_callbacks,
            s.overloads,
            s.faults,
            s.resets,
            s.gap_max_ms
        )
    }

    fn device() -> dstack_asio::win::WinDevice {
        dstack_asio::win::WinDevice::new()
    }

    pub fn main() -> ExitCode {
        let args = match parse() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("asio-probe: {e}");
                return ExitCode::from(2);
            }
        };
        let Some(driver) = args.driver else {
            return match dstack_asio::drivers() {
                Ok(list) => {
                    for d in list {
                        println!("{d}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("asio-probe: reading the driver list: {e}");
                    ExitCode::FAILURE
                }
            };
        };
        let peak = Arc::new(AtomicU64::new(0));
        let p = Arc::clone(&peak);
        let stop = Arc::new(AtomicBool::new(false));
        let status: Arc<Mutex<Option<Arc<Mutex<Status>>>>> = Arc::new(Mutex::new(None));
        let (st, sp) = (Arc::clone(&status), Arc::clone(&stop));
        let worker = std::thread::spawn(move || {
            // The device lives on this thread (its COM STA).
            let make: dstack_asio::MakeProcess = Box::new(move |_| {
                Ok(Box::new(Listen {
                    peak: Arc::clone(&p),
                }) as Box<dyn Process>)
            });
            let runner = Runner::new(device(), &driver, args.channels, make);
            *st.lock().unwrap() = Some(runner.status());
            runner.run(&sp);
        });
        let mut ok = true;
        let mut ran = false;
        for _ in 0..args.seconds {
            std::thread::sleep(Duration::from_secs(1));
            let Some(s) = status.lock().unwrap().clone() else {
                continue;
            };
            let s = s.lock().unwrap().clone();
            println!("{}", line(&s, f64::from_bits(peak.load(Ordering::Relaxed))));
            ran |= s.state == RunState::Running && s.callbacks > 0;
            ok &= s.faults == 0;
        }
        stop.store(true, Ordering::Relaxed);
        let joined = worker.join().is_ok();
        println!("released");
        if ok && ran && joined {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    probe::main()
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("asio-probe: ASIO runs on Windows only");
    std::process::ExitCode::FAILURE
}

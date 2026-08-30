/*!
Set up (or tear down) a low-noise benchmarking environment on Linux.

```none
sudo `which quiet-bench` reserve 2    # quiesce the machine, reserving CPU 2
sudo `which quiet-bench` reserve 2,3  # reserve CPUs 2 and 3
quiet-bench run cargo test --release
quiet-bench status
sudo `which quiet-bench` restore      # undo everything
```

`reserve` and `restore` need root; `run` and `status` do not.

Note the `` `which quiet-bench` ``. `cargo install` puts binaries in
`~/.cargo/bin`, which is on *your* PATH but not on root's, so a plain
`sudo quiet-bench ...` will usually fail with "command not found".
Substituting the full path sidesteps that. If you would rather type the
short form, copy the binary somewhere root can see it, e.g.
`sudo cp ~/.cargo/bin/quiet-bench /usr/local/bin/`.

Benchmarks built against the `scaling` crate pin themselves to the reserved
CPUs automatically when launched through `quiet-bench run`, which sets
[`scaling::quiet::CPUS_VAR`] in the environment. `run` also sets the
affinity of the command it launches, so programs that know nothing about
`scaling` land on the reserved CPUs too.
*/

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("quiet-bench only works on Linux.");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn main() {
    std::process::exit(linux::run());
}

#[cfg(target_os = "linux")]
mod linux {
    use scaling::quiet::{format_cpu_list, parse_cpu_list, status, Status, CPUS_PATH, CPUS_VAR};
    use std::fmt::Write as _;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    /// Saved original settings, so `restore` can put back what was actually
    /// there rather than guessing at defaults.
    const STATE_PATH: &str = "/run/quiet-bench.state";
    const PSTATE: &str = "/sys/devices/system/cpu/intel_pstate";

    const USAGE: &str = "\
usage:
  sudo `which quiet-bench` reserve <cpu-list>   quiesce the machine (e.g. 2)
  sudo `which quiet-bench` restore              undo it
  quiet-bench run <command> [args...]           run on the reserved CPUs
  quiet-bench status                            is a reservation active?

`cargo install` puts this on your PATH but not on root's, hence the
backticks; or copy it to /usr/local/bin to type the short form.
";

    pub fn run() -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let cmd = args.first().map(String::as_str);
        match (cmd, args.len()) {
            (Some("reserve"), 2) => match cmd_reserve(&args[1]) {
                Ok(()) => 0,
                Err(e) => fail(&e),
            },
            (Some("restore"), 1) => match cmd_restore() {
                Ok(()) => 0,
                Err(e) => fail(&e),
            },
            (Some("status"), 1) => cmd_status(),
            (Some("run"), n) if n >= 2 => match cmd_run(&args[1..]) {
                Ok(code) => code,
                Err(e) => fail(&e),
            },
            _ => {
                eprint!("{USAGE}");
                2
            }
        }
    }

    fn fail(msg: &str) -> i32 {
        eprintln!("quiet-bench: {msg}");
        1
    }

    // ---------------------------------------------------------------- run

    fn cmd_run(argv: &[String]) -> Result<i32, String> {
        let cpus_str = scaling::quiet::reserved_cpus().ok_or_else(|| {
            "machine is not quiesced; run `sudo quiet-bench reserve <cpu-list>` first".to_string()
        })?;
        let cpus = parse_cpu_list(&cpus_str)?;

        // Pin ourselves before spawning: affinity is inherited, so the child
        // and everything it spawns start out on the reserved CPUs. The
        // environment variable additionally lets `scaling` confirm - and
        // re-apply - the pinning from inside the benchmark process.
        scaling::quiet::pin_current_thread(&cpus)
            .map_err(|e| format!("could not pin to CPU(s) {cpus_str}: {e}"))?;

        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .env(CPUS_VAR, &cpus_str)
            .status()
            .map_err(|e| format!("could not run {:?}: {e}", argv[0]))?;
        // Propagate the child's exit status, including death by signal, so
        // this is transparent to whatever is driving it (a test runner, CI).
        Ok(exit_code_of(status))
    }

    fn exit_code_of(status: std::process::ExitStatus) -> i32 {
        use std::os::unix::process::ExitStatusExt;
        status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
    }

    // ------------------------------------------------------------- status

    fn cmd_status() -> i32 {
        let s = status();
        println!("{s}");
        match s {
            Status::Pinned { .. } => 0,
            // Not an error: `status` answers a question, and "no" is a valid
            // answer. But a distinct code lets a script gate on it.
            _ => 1,
        }
    }

    // ------------------------------------------------------------ reserve

    fn cmd_reserve(list: &str) -> Result<(), String> {
        require_root()?;
        let bench_cpus = parse_cpu_list(list)?;
        let ncpu = num_cpus()?;
        for &c in &bench_cpus {
            if !Path::new(&format!("/sys/devices/system/cpu/cpu{c}")).exists() {
                return Err(format!("no such CPU: {c}"));
            }
        }
        // The SMT siblings of the reserved CPUs get offlined in step 3, so
        // they are not really available to the rest of the system even
        // though they aren't in `bench_cpus`. `other` must exclude them
        // *before* the "at least one CPU left" check below, or the check
        // validates the wrong set - one `reserve` on a small SMT machine
        // could offline every remaining CPU while still reporting success.
        let offlined_siblings: Vec<usize> = bench_cpus
            .iter()
            .flat_map(|&c| thread_siblings(c))
            .filter(|sib| !bench_cpus.contains(sib))
            .collect();
        let other: Vec<usize> = (0..ncpu)
            .filter(|c| !bench_cpus.contains(c) && !offlined_siblings.contains(c))
            .collect();
        if other.is_empty() {
            return Err(
                "reserving these CPU(s) would offline every remaining CPU via their SMT \
                 siblings, leaving nothing for the rest of the system; reserve fewer CPUs \
                 or include their siblings in the reservation"
                    .to_string(),
            );
        }

        // Everything we are about to change is a file, so describe the whole
        // reservation as a list of (path, new contents). That list is walked
        // twice - once to record what was there, once to apply - which is
        // what makes `restore` able to be a single loop with no knowledge of
        // any individual setting. Adding a new tweak here is one push, and
        // it is undone automatically.
        let mask = affinity_mask(&other);
        let mut changes: Vec<(String, String)> = Vec::new();
        let mut notes = Vec::new();

        // Pin the clock. On this kind of machine the largest single source of
        // run-to-run drift is dropping out of turbo partway through a
        // benchmark as the package heats up, so we disable turbo outright
        // rather than try to race it.
        if Path::new(PSTATE).is_dir() {
            changes.push((format!("{PSTATE}/no_turbo"), "1".into()));
            changes.push((format!("{PSTATE}/min_perf_pct"), "100".into()));
        } else {
            notes.push("no intel_pstate: turbo and minimum frequency left alone".to_string());
        }

        // Performance governor everywhere, so the housekeeping CPUs don't
        // drag the package clock around either.
        for c in 0..ncpu {
            changes.push((
                format!("/sys/devices/system/cpu/cpu{c}/cpufreq/scaling_governor"),
                "performance".into(),
            ));
        }

        // Offline the SMT siblings of the reserved CPUs: a sibling running
        // someone else's code shares execution resources with ours.
        for &sib in &offlined_siblings {
            changes.push((
                format!("/sys/devices/system/cpu/cpu{sib}/online"),
                "0".into(),
            ));
        }

        // Steer interrupts away. Some IRQs are per-CPU or managed by the
        // kernel and refuse to move; that is expected, not a failure.
        changes.push(("/proc/irq/default_smp_affinity".into(), mask.clone()));
        for irq in numbered_irqs() {
            changes.push((format!("/proc/irq/{irq}/smp_affinity"), mask.clone()));
        }

        // Deterministic address layout, and let unprivileged `perf` read
        // counters, since both matter for reproducing a measurement.
        changes.push(("/proc/sys/kernel/randomize_va_space".into(), "0".into()));
        changes.push(("/proc/sys/kernel/perf_event_paranoid".into(), "-1".into()));

        // Record originals *before* touching anything, so that a failure
        // partway through still leaves a state file `restore` can use.
        save_original_state(&changes)?;

        let _ = Command::new("systemctl").args(["stop", "irqbalance"]).status();
        let applied = changes
            .iter()
            .filter(|(path, value)| fs::write(path, value).is_ok())
            .count();
        notes.push(format!(
            "applied {applied} of {} settings",
            changes.len()
        ));

        // Herd existing tasks (and, by inheritance, their children) onto the
        // housekeeping CPUs. Not part of `changes` because affinity is not a
        // file, and because there is no fixed set of "the affinities" to
        // record: tasks come and go for as long as the reservation lasts.
        //
        // Deliberately plain per-task affinity rather than a cgroup cpuset: a
        // task cannot widen its affinity beyond its cpuset, so a cpuset here
        // would lock the benchmark itself out of the very CPUs we are
        // reserving for it. Plain affinity is advisory in exactly the way we
        // need - everything else is moved aside, but `quiet-bench run` can
        // still claim the reserved CPUs.
        clear_stale_cpusets();
        let moved = move_tasks_to(&other);
        notes.push(format!(
            "moved {moved} tasks onto CPU(s) {}",
            format_cpu_list(&other)
        ));

        // Report what actually happened by reading back, rather than trusting
        // the writes: cpu0 in particular usually has no `online` file at all
        // and cannot be offlined, and it is a realistic SMT sibling of a
        // reserved CPU.
        if !offlined_siblings.is_empty() {
            let off = offlined_siblings
                .iter()
                .filter(|&&sib| {
                    fs::read_to_string(format!("/sys/devices/system/cpu/cpu{sib}/online"))
                        .map(|s| s.trim() == "0")
                        .unwrap_or(false)
                })
                .count();
            notes.push(format!(
                "offlined {off} of {} SMT sibling(s)",
                offlined_siblings.len()
            ));
        }

        // Advertise the reservation. This lives on /run, a tmpfs, so it
        // cannot outlive a reboot and become a lie.
        let canonical = format_cpu_list(&bench_cpus);
        fs::write(CPUS_PATH, format!("{canonical}\n"))
            .map_err(|e| format!("could not write {CPUS_PATH}: {e}"))?;

        for n in &notes {
            println!("{n}");
        }
        println!();
        println!("ready: CPU(s) {canonical} reserved");
        println!();
        println!("Run benchmarks through the pinning wrapper (as your normal user):");
        println!();
        println!("  quiet-bench run cargo test --release");
        println!();
        println!(
            "Anything not run that way lands on the housekeeping CPUs ({}) \nand gains nothing.",
            format_cpu_list(&other)
        );
        println!();
        println!("Undo with:  sudo quiet-bench restore");
        Ok(())
    }

    // ------------------------------------------------------------ restore

    fn cmd_restore() -> Result<(), String> {
        require_root()?;
        let ncpu = num_cpus()?;
        let saved = load_original_state();

        // Put every file back to exactly what was in it. Because the state
        // file is keyed by path, this needs to know nothing about what any
        // individual setting means - and anything `reserve` changed is
        // necessarily in here, because recording is how `reserve` changes it.
        //
        // `online` files go first: a governor write to an offline CPU fails,
        // so a CPU has to come back before the rest of its settings do.
        let (online, rest): (Vec<_>, Vec<_>) = saved
            .iter()
            .filter(|(key, _)| key.starts_with('/'))
            .partition(|(path, _)| path.ends_with("/online"));
        let restored = online
            .into_iter()
            .chain(rest)
            .filter(|(path, value)| fs::write(path, value).is_ok())
            .count();

        // Widen every task back to every CPU. Unlike the settings above this
        // is not a restore to a recorded value - see the note in `reserve` -
        // but erring wide is the safe direction: leaving `reserve`'s
        // restriction in place would strand every long-running task off the
        // reserved CPUs for good.
        clear_stale_cpusets();
        let moved = move_tasks_to(&(0..ncpu).collect::<Vec<_>>());

        // `reserve` stops irqbalance unconditionally, so only start it again
        // if it was actually running beforehand. Absent a state file - after
        // a reboot, say - assume it was, which is the common case.
        if saved.get("irqbalance").map_or(true, |v| v == "active") {
            let _ = Command::new("systemctl")
                .args(["start", "irqbalance"])
                .status();
        }

        let _ = fs::remove_file(STATE_PATH);
        let _ = fs::remove_file(CPUS_PATH);
        // Remove the wrapper the older shell implementation installed, so a
        // machine set up with that script is fully cleaned up by this one.
        let _ = fs::remove_file("/usr/local/bin/bench");

        println!("restored: {restored} settings put back, {moved} tasks unpinned");
        Ok(())
    }

    // ------------------------------------------------------------- state

    /// Record what is in each path `changes` is about to overwrite.
    ///
    /// Keyed by path, so `restore` can replay the whole thing without
    /// knowing what any entry means, and so a setting cannot be changed
    /// without being recorded - the same list drives both. The one non-path
    /// key is `irqbalance`, which is a service rather than a file.
    ///
    /// Write-once: on a second `reserve` (or after a partly-failed one) the
    /// live values are already our own, so overwriting the snapshot would
    /// lose the true originals forever.
    fn save_original_state(changes: &[(String, String)]) -> Result<(), String> {
        if Path::new(STATE_PATH).exists() {
            return Ok(());
        }
        let mut out = String::new();
        for (path, _) in changes {
            // A path that cannot be read cannot be restored either, so skip
            // it. Plenty legitimately do not exist: cpu0 usually has no
            // `online`, and `intel_pstate` is absent on AMD.
            if let Ok(v) = fs::read_to_string(path) {
                let _ = writeln!(out, "{path}={}", v.trim());
            }
        }
        let irqbalance = Command::new("systemctl")
            .args(["is-active", "--quiet", "irqbalance"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let _ = writeln!(
            out,
            "irqbalance={}",
            if irqbalance { "active" } else { "inactive" }
        );
        fs::write(STATE_PATH, out).map_err(|e| format!("could not write {STATE_PATH}: {e}"))
    }

    fn load_original_state() -> std::collections::BTreeMap<String, String> {
        let mut map = std::collections::BTreeMap::new();
        if let Ok(text) = fs::read_to_string(STATE_PATH) {
            for line in text.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    map.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        }
        map
    }

    // ------------------------------------------------------------ helpers

    fn require_root() -> Result<(), String> {
        if unsafe { libc::geteuid() } == 0 {
            Ok(())
        } else {
            Err("must run as root (try `sudo`)".to_string())
        }
    }

    fn num_cpus() -> Result<usize, String> {
        let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) };
        if n < 1 {
            return Err("could not determine the CPU count".to_string());
        }
        Ok(n as usize)
    }

    fn thread_siblings(cpu: usize) -> Vec<usize> {
        fs::read_to_string(format!(
            "/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list"
        ))
        .ok()
        .and_then(|s| parse_cpu_list(s.trim()).ok())
        .unwrap_or_default()
    }

    /// Hex affinity mask, as `/proc/irq/*/smp_affinity` wants it.
    ///
    /// Written as comma-separated 32-bit groups, most significant first,
    /// which is the only format the kernel accepts for machines with more
    /// than 32 CPUs - and it accepts it for smaller ones too.
    fn affinity_mask(cpus: &[usize]) -> String {
        let highest = cpus.iter().copied().max().unwrap_or(0);
        let groups = highest / 32 + 1;
        let mut words = vec![0u32; groups];
        for &c in cpus {
            words[c / 32] |= 1 << (c % 32);
        }
        words
            .iter()
            .rev()
            .map(|w| format!("{w:08x}"))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Set the affinity of every thread of every process. Threads that
    /// refuse to move (kernel threads, mostly) are skipped silently - that
    /// is normal and not worth reporting.
    fn move_tasks_to(cpus: &[usize]) -> usize {
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        unsafe { libc::CPU_ZERO(&mut set) };
        for &c in cpus {
            if c < libc::CPU_SETSIZE as usize {
                unsafe { libc::CPU_SET(c, &mut set) };
            }
        }
        let size = std::mem::size_of::<libc::cpu_set_t>();

        let mut moved = 0;
        let entries = match fs::read_dir("/proc") {
            Ok(e) => e,
            Err(_) => return 0,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = match name.to_str() {
                Some(n) => n,
                None => continue,
            };
            if name.parse::<u32>().is_err() {
                continue;
            }
            // Every thread individually: setting affinity on the process id
            // moves only its main thread, which would leave the rest behind.
            let tasks = match fs::read_dir(format!("/proc/{name}/task")) {
                Ok(t) => t,
                Err(_) => continue,
            };
            for task in tasks.flatten() {
                if let Some(tid) = task.file_name().to_str().and_then(|t| t.parse::<i32>().ok()) {
                    if unsafe { libc::sched_setaffinity(tid, size, &set) } == 0 {
                        moved += 1;
                    }
                }
            }
        }
        moved
    }

    /// The numbered IRQs under `/proc/irq`, each of which has its own
    /// `smp_affinity`.
    fn numbered_irqs() -> Vec<u32> {
        let entries = match fs::read_dir("/proc/irq") {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        entries
            .flatten()
            .filter_map(|e| e.file_name().to_str().and_then(|n| n.parse().ok()))
            .collect()
    }

    /// Clear any cpuset left behind by the older shell implementation of
    /// this tool, which would otherwise silently veto our pinning.
    fn clear_stale_cpusets() {
        for unit in ["system.slice", "user.slice", "init.scope"] {
            let _ = Command::new("systemctl")
                .args(["set-property", "--runtime", unit, "AllowedCPUs="])
                .status();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn affinity_masks_are_grouped_in_32_bit_words() {
            assert_eq!(affinity_mask(&[0]), "00000001");
            assert_eq!(affinity_mask(&[2]), "00000004");
            assert_eq!(affinity_mask(&[0, 1, 3]), "0000000b");
            assert_eq!(affinity_mask(&[31]), "80000000");
            // Crossing into a second word must produce two comma-separated
            // groups, most significant first.
            assert_eq!(affinity_mask(&[32]), "00000001,00000000");
            assert_eq!(affinity_mask(&[0, 32]), "00000001,00000001");
            assert_eq!(affinity_mask(&[64]), "00000001,00000000,00000000");
        }
    }
}

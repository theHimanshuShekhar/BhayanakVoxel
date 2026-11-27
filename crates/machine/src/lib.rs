//! What the machine has, and how the game's threads share it (#264).
//!
//! The rule (`AGENTS.md`, "Use the whole machine"): the game and its tools use the CPU, the memory
//! and the GPU they run on to the full, sized from the machine and not from a constant: a budget
//! derived from the machine has a floor, so a small machine degrades and never fails, and a
//! ceiling, so a big one is used. This crate is where the machine is read, once:
//!
//! * [`profile`] is the machine's [`Profile`]: its threads, its free memory, its video memory and
//!   whether it has ray queries, after the override of [`ENV_PROFILE`] and its siblings, or of
//!   [`constrain`] (a flag). The constrained profile is how the low-end reference machine
//!   ([`Profile::LOW_END`]: 4 threads, 5 GiB free of 8 GB, 4 GiB of video memory, no ray queries) is run on any
//!   machine: it only ever lowers what was found, never raises it.
//!   [`pin_to_profile`] then holds the process to that many logical processors, so that every
//!   thread (the frame thread, the scene thread, the simulation, the driver's) shares them, as on
//!   a real machine of that size.
//! * [`threads`], [`bulk_threads`] and [`loader_workers`] are the thread counts the rest of the
//!   workspace derives from it: no `available_parallelism` call is made elsewhere.
//! * [`par_pieces`], [`par_map`] and [`par_fill`] share work among those threads **deterministically**:
//!   the work is cut into pieces of a size the caller fixes (never one piece a thread), the threads
//!   take the next piece as they finish one (so an uneven piece costs the others no wait at the
//!   tail), and the results come back in piece order, whatever thread ran what. A function that is
//!   a pure function of its piece's index therefore gives the same answer on 1, 4 or all of the
//!   threads, which ADR 0003 needs.

use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// What a machine has that the game sizes itself from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    /// Logical processors the process may use.
    pub threads: usize,
    /// Memory the process may use, in bytes: what is free of the machine's, or of the container's
    /// limit.
    pub memory: u64,
    /// Video memory, in bytes: only what a flag or the environment says (`--video-memory-mib`) or
    /// [`Profile::LOW_END`]'s 4 GiB, since no adapter's figure is read yet (`wgpu` does not give
    /// one: `docs/perf/resource-audit.md`). **Nothing sizes a budget from it yet.** `None` is not
    /// known.
    pub video_memory: Option<u64>,
    /// Whether the adapter has the ray queries that the hardware backend needs. Read by the
    /// renderer, which only ever turns it off here.
    pub ray_queries: bool,
}

/// One mebibyte.
pub const MIB: u64 = 1 << 20;

/// One gibibyte.
pub const GIB: u64 = 1 << 30;

impl Profile {
    /// The low-end machine that the game is sized to run on (the owner's, #264): a GTX 1650 or
    /// RX 570 class PC, 4 threads, 8 GB of RAM, 4 GB of video memory, no ray queries, so the
    /// compute tracer. [`Profile::memory`] is what is free, not what is fitted: 5 GiB of the
    /// 8 GB (Windows at idle leaves about 4.5 to 5 GiB free on one), the rest being the system's.
    pub const LOW_END: Profile = Profile {
        threads: 4,
        memory: 5 * GIB,
        video_memory: Some(4 * GIB),
        ray_queries: false,
    };

    /// This profile held to `limit`: the least of each, so that it is never more than either.
    pub fn limited_to(self, limit: Profile) -> Profile {
        Profile {
            threads: self.threads.min(limit.threads),
            memory: self.memory.min(limit.memory),
            video_memory: match (self.video_memory, limit.video_memory) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            },
            ray_queries: self.ray_queries && limit.ray_queries,
        }
    }

    /// This profile in one line, for the log and the benchmark's notes.
    pub fn describe(&self) -> String {
        format!(
            "{} threads, {:.1} GiB of memory to use, video memory {}, ray queries {}",
            self.threads,
            self.memory as f64 / GIB as f64,
            self.video_memory
                .map_or("not known".to_string(), |v| format!(
                    "{:.1} GiB",
                    v as f64 / GIB as f64
                )),
            if self.ray_queries { "allowed" } else { "off" },
        )
    }
}

/// `low-end` (or `low`) makes the profile [`Profile::LOW_END`], held to what the machine has: the
/// constrained run of the benchmarks, on any machine. Anything else but `auto` or empty is an error
/// that is said once and ignored.
pub const ENV_PROFILE: &str = "BHAYANAK_PROFILE";
/// The most threads to use, whole number: lowers the profile's threads.
pub const ENV_THREADS: &str = "BHAYANAK_THREADS";
/// The most memory to use, in MiB: lowers the profile's memory.
pub const ENV_MEMORY_MIB: &str = "BHAYANAK_MEMORY_MIB";
/// The most video memory to use, in MiB: lowers the profile's video memory.
pub const ENV_VIDEO_MEMORY_MIB: &str = "BHAYANAK_VIDEO_MEMORY_MIB";
/// `0`, `false` or `no`: the renderer is to act as if the adapter had no ray queries (the compute
/// tracer).
pub const ENV_RAY_QUERIES: &str = "BHAYANAK_RAY_QUERIES";

/// The command line's form of the overrides above, for a binary to call first thing with its
/// arguments: `--profile low-end` (or `auto`), `--threads N`, `--memory-mib N`,
/// `--video-memory-mib N` and `--no-ray-queries`, each taken out of `args` wherever it is (so that
/// the argument parser of a subcommand never sees it) and applied with [`constrain`]. Anything that
/// is wrong is an `Err` that says what.
pub fn take_flags(args: Vec<String>) -> Result<Vec<String>, String> {
    let mut limit = Profile {
        threads: usize::MAX,
        memory: u64::MAX,
        video_memory: None,
        ray_queries: true,
    };
    let mut asked = false;
    let mut rest = Vec::with_capacity(args.len());
    let mut args = args.into_iter();
    let number = |flag: &str, value: Option<String>| -> Result<u64, String> {
        value
            .as_deref()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|&n| n >= 1)
            .ok_or_else(|| format!("{flag} needs a whole number of at least 1"))
    };
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_string(), Some(value.to_string()))
            }
            _ => (arg.clone(), None),
        };
        let mut value = || inline.clone().or_else(|| args.next());
        match flag.as_str() {
            "--profile" => match value().as_deref() {
                Some("low-end" | "low" | "constrained") => {
                    limit = limit.limited_to(Profile::LOW_END);
                    asked = true;
                }
                Some("auto") => {}
                _ => return Err("--profile is `low-end` or `auto`".to_string()),
            },
            "--threads" => {
                limit.threads = limit.threads.min(number("--threads", value())? as usize);
                asked = true;
            }
            "--memory-mib" => {
                limit.memory = limit.memory.min(number("--memory-mib", value())? << 20);
                asked = true;
            }
            "--video-memory-mib" => {
                let bytes = number("--video-memory-mib", value())? << 20;
                limit.video_memory = Some(limit.video_memory.map_or(bytes, |v| v.min(bytes)));
                asked = true;
            }
            "--no-ray-queries" if inline.is_none() => {
                limit.ray_queries = false;
                asked = true;
            }
            _ => rest.push(arg),
        }
    }
    if asked && !constrain(limit) {
        return Err("the machine's profile was read before the flags were applied".to_string());
    }
    Ok(rest)
}

static PROFILE: OnceLock<Profile> = OnceLock::new();
static FOUND: OnceLock<Profile> = OnceLock::new();
static LIMIT: OnceLock<Profile> = OnceLock::new();

/// Holds the profile to `limit` for the whole run: the flag form of [`ENV_PROFILE`]. It has to be
/// called before [`profile`] is first read (the first read fixes it); returns whether it took
/// effect.
pub fn constrain(limit: Profile) -> bool {
    LIMIT.set(limit).is_ok() && PROFILE.get().is_none()
}

/// The machine's profile, read once: what was found, held to the limit [`constrain`] set and to the
/// environment's ([`ENV_PROFILE`] and the rest).
pub fn profile() -> Profile {
    *PROFILE.get_or_init(|| {
        let mut found = detect();
        let _ = FOUND.set(found);
        if let Some(limit) = LIMIT.get() {
            found = found.limited_to(*limit);
        }
        from_environment(found, |name| std::env::var(name).ok())
    })
}

/// What the machine has, as [`profile`] found it before any flag or environment variable held it
/// down (read once, with the profile).
pub fn found() -> Profile {
    profile();
    *FOUND.get().expect("profile() sets what it found")
}

/// Whether [`profile`] is different from the machine: a flag or the environment held something
/// down (threads, memory, video memory or ray queries).
pub fn constrained() -> bool {
    profile() != found()
}

/// Whether [`profile`] has fewer threads or less memory than the machine was found to have: the
/// run is on a smaller machine than this one, which is what a benchmark's gates are not budgeted
/// for. (Turning off ray queries or limiting video memory alone leaves the machine as it is.)
pub fn lowered() -> bool {
    let (profile, found) = (profile(), found());
    profile.threads < found.threads || profile.memory < found.memory
}

/// `found` held to what the environment asks, read through `get` (a function, so that a test does
/// not change the process's environment).
pub fn from_environment(found: Profile, get: impl Fn(&str) -> Option<String>) -> Profile {
    let mut profile = found;
    let bad = |name: &str, value: &str| {
        eprintln!("warning: {name}={value:?} is not understood and is ignored");
    };
    match get(ENV_PROFILE).as_deref().map(str::trim) {
        None | Some("") | Some("auto") => {}
        Some("low-end" | "low" | "constrained") => {
            profile = profile.limited_to(Profile::LOW_END);
        }
        Some(other) => bad(ENV_PROFILE, other),
    }
    if let Some(value) = get(ENV_THREADS) {
        match value.trim().parse::<usize>() {
            Ok(n) if n >= 1 => profile.threads = profile.threads.min(n),
            _ => bad(ENV_THREADS, &value),
        }
    }
    let mib = |name: &str, value: &str| match value.trim().parse::<u64>() {
        Ok(n) if n >= 1 => Some(n << 20),
        _ => {
            bad(name, value);
            None
        }
    };
    if let Some(bytes) = get(ENV_MEMORY_MIB).and_then(|v| mib(ENV_MEMORY_MIB, &v)) {
        profile.memory = profile.memory.min(bytes);
    }
    if let Some(bytes) = get(ENV_VIDEO_MEMORY_MIB).and_then(|v| mib(ENV_VIDEO_MEMORY_MIB, &v)) {
        profile.video_memory = Some(profile.video_memory.map_or(bytes, |v| v.min(bytes)));
    }
    if let Some(value) = get(ENV_RAY_QUERIES) {
        match value.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "no" | "off" => profile.ray_queries = false,
            "1" | "true" | "yes" | "on" => {}
            _ => bad(ENV_RAY_QUERIES, &value),
        }
    }
    profile
}

/// What the machine has, before any override: the processors the process may run on (the standard
/// library's answer, which honours a container's CPU limit and the process's affinity), the memory
/// that is free, and ray queries and video memory as not known (the renderer fills those in).
pub fn detect() -> Profile {
    Profile {
        threads: std::thread::available_parallelism().map_or(1, |n| n.get()),
        memory: available_memory().unwrap_or(Profile::LOW_END.memory),
        video_memory: None,
        ray_queries: true,
    }
}

/// Memory that the process can use, in bytes, if the system says: free physical memory (Windows'
/// `ullAvailPhys`, Linux's `MemAvailable`), less than a container's limit if there is one.
pub fn available_memory() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        let free = meminfo_kib(&meminfo, "MemAvailable")? << 10;
        let read = |path: &str| std::fs::read_to_string(path).ok();
        let room = container_room(read, &read("/proc/self/cgroup").unwrap_or_default());
        Some(room.map_or(free, |room| room.min(free)))
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status = MEMORYSTATUSEX {
            dwLength: size_of::<MEMORYSTATUSEX>() as u32,
            dwMemoryLoad: 0,
            ullTotalPhys: 0,
            ullAvailPhys: 0,
            ullTotalPageFile: 0,
            ullAvailPageFile: 0,
            ullTotalVirtual: 0,
            ullAvailVirtual: 0,
            ullAvailExtendedVirtual: 0,
        };
        // SAFETY: `status` is a valid MEMORYSTATUSEX whose `dwLength` is set, as the call needs.
        let read = unsafe { GlobalMemoryStatusEx(&mut status) };
        (read != 0).then_some(status.ullAvailPhys)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    None
}

/// What is left of a memory limit, from the texts of the limit and of the use, less the page cache
/// that the kernel can take back (`inactive_file` of `memory.stat`, `stat` and `cache_key`: it is
/// counted in the use and is not used memory, so a container that has only read files is not
/// short). cgroup v2's `memory.max` is a number or `max` and v1's `memory.limit_in_bytes` is a
/// number near 2^63 when there is no limit. `None` if there is no limit, or the texts are not
/// numbers.
#[cfg(any(target_os = "linux", test))]
fn cgroup_room(
    limit: Option<&str>,
    used: Option<&str>,
    stat: Option<&str>,
    cache_key: &str,
) -> Option<u64> {
    /// v1 says "no limit" as the largest page-aligned number; nothing has this much memory.
    const NO_LIMIT: u64 = 1 << 60;
    let limit: u64 = limit?.trim().parse().ok().filter(|&l| l < NO_LIMIT)?;
    let used: u64 = used?.trim().parse().ok()?;
    let cache = stat
        .and_then(|text| stat_value(text, cache_key))
        .unwrap_or(0);
    Some(limit.saturating_sub(used.saturating_sub(cache)))
}

/// The number after `key` in the text of a `memory.stat`.
#[cfg(any(target_os = "linux", test))]
fn stat_value(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(' ')?;
        (name == key).then(|| value.trim().parse().ok())?
    })
}

/// The directories of `path` and of each ancestor of it, up to the root (`/`), nearest first:
/// `/a/b` is `/a/b`, `/a` and `/`.
#[cfg(any(target_os = "linux", test))]
fn with_ancestors(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = path.trim_end_matches('/');
    loop {
        out.push(if at.is_empty() { "/" } else { at }.to_string());
        match at.rfind('/') {
            Some(cut) => at = &at[..cut],
            None => return out,
        }
    }
}

/// The room the cgroups of the process leave it: the least, over the process's own cgroup and each
/// of its ancestors up to the root, of each one's limit less its use, in either version of cgroups
/// (a limit may be on any of them: a container's is on the group the container's runtime made, and
/// the process may be in a group below it). `proc_cgroup` is the text of `/proc/self/cgroup` and
/// `read` reads a file. `None` if no group has a limit. A directory that is not there is skipped
/// (inside a container the namespace's root is `/`, and the paths of the host are not visible).
#[cfg(any(target_os = "linux", test))]
fn container_room(read: impl Fn(&str) -> Option<String>, proc_cgroup: &str) -> Option<u64> {
    let mut rooms = Vec::new();
    for line in proc_cgroup.lines() {
        let mut parts = line.splitn(3, ':');
        let (_id, controllers, path) = (parts.next(), parts.next()?, parts.next()?);
        // v2 is `0::/path`; v1 has a line for each set of controllers, and ours has `memory`.
        let (root, limit, used, cache_key) = if controllers.is_empty() {
            (
                "/sys/fs/cgroup",
                "memory.max",
                "memory.current",
                "inactive_file",
            )
        } else if controllers.split(',').any(|c| c == "memory") {
            (
                "/sys/fs/cgroup/memory",
                "memory.limit_in_bytes",
                "memory.usage_in_bytes",
                "total_inactive_file",
            )
        } else {
            continue;
        };
        for dir in with_ancestors(path) {
            let file = |name: &str| read(&format!("{root}{}/{name}", dir.trim_end_matches('/')));
            rooms.extend(cgroup_room(
                file(limit).as_deref(),
                file(used).as_deref(),
                file("memory.stat").as_deref(),
                cache_key,
            ));
        }
    }
    rooms.into_iter().min()
}

/// The kibibytes of `key` in the text of `/proc/meminfo`.
#[cfg(any(target_os = "linux", test))]
fn meminfo_kib(text: &str, key: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// What [`pin_to_profile`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pinning {
    /// The profile has all the machine's processors, or the system has no way to hold a process
    /// to some (neither is a failure).
    NotNeeded,
    /// The process, with the threads it has and will make, runs on these logical processors only
    /// (on Linux but for a thread that sets its own mask, as Mesa's shader cache does).
    Pinned(Vec<usize>),
    /// The system refused or could not say which processors the process may use: the process runs
    /// as before, on all of them, with [`threads`] threads of work.
    Failed(String),
}

impl Pinning {
    /// The mask in one line for the log and the report: `cpus 0,1,2,3`, `not pinned` or what failed.
    pub fn describe(&self) -> String {
        match self {
            Pinning::NotNeeded => "not pinned".to_string(),
            Pinning::Pinned(cpus) => format!(
                "pinned to cpus {}",
                cpus.iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Pinning::Failed(why) => format!("pinning failed ({why}); not pinned"),
        }
    }
}

/// The logical processors to hold a process to for `wanted` threads: the first `wanted` of the
/// `allowed` ones, in order (the processors the process may already use, which a container or an
/// earlier mask may have cut down). None if it has no more than `wanted` of them: there is nothing
/// to cut. The first of them are the nearest to each other on the machines that number a core's
/// hardware threads together, so on one with SMT `wanted` threads may be fewer cores than that:
/// the harder way to be a small machine, and not the easier.
pub fn cpus_for(allowed: &[usize], wanted: usize) -> Option<Vec<usize>> {
    (wanted >= 1 && allowed.len() > wanted).then(|| allowed[..wanted].to_vec())
}

/// Holds the process to [`threads`] logical processors when the profile has fewer than the machine
/// ([`constrained`]), so that the threads of it (the frame thread, the scene thread, the
/// simulation, the driver's and the system's helpers) compete for them, as they would on a machine
/// that small, and not only the threads that [`threads`] counts (#264). Call it first thing in
/// `main`, after [`take_flags`], before any thread is made: threads inherit the mask (on Linux the
/// ones already there are held too). It cannot raise what the process may use.
///
/// **On Windows the process's mask binds every thread. On Linux a thread can set its own mask, and
/// one does: Mesa's shader disk-cache thread (`client:disk$0`) resets its affinity, so it runs on
/// any processor and escapes the pin** (it is a thread of little work). The threads the game
/// makes stay on the processors.
pub fn pin_to_profile() -> Pinning {
    PINNING
        .get_or_init(|| {
            if constrained() {
                pin(threads())
            } else {
                Pinning::NotNeeded
            }
        })
        .clone()
}

static PINNING: OnceLock<Pinning> = OnceLock::new();

/// What [`pin_to_profile`] did (it has not been called: [`Pinning::NotNeeded`]), for the report.
pub fn pinning() -> Pinning {
    PINNING.get().cloned().unwrap_or(Pinning::NotNeeded)
}

#[cfg(target_os = "linux")]
fn pin(wanted: usize) -> Pinning {
    // `sched_setaffinity(0)` is the calling thread's, and the threads made later inherit it: the
    // ones that are already there are held one by one.
    let mut tasks: Vec<i32> = std::fs::read_dir("/proc/self/task")
        .map(|dir| {
            dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    tasks.push(0);
    pin_tasks(wanted, &tasks)
}

/// [`pin`] on the Linux threads `tasks` (0 is the caller).
#[cfg(target_os = "linux")]
fn pin_tasks(wanted: usize, tasks: &[i32]) -> Pinning {
    // SAFETY: `cpu_set_t` is plain data, and the calls are given the set's size.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut set) != 0 {
            return Pinning::Failed(std::io::Error::last_os_error().to_string());
        }
        let allowed: Vec<usize> = (0..libc::CPU_SETSIZE as usize)
            .filter(|&cpu| libc::CPU_ISSET(cpu, &set))
            .collect();
        let Some(chosen) = cpus_for(&allowed, wanted) else {
            return Pinning::NotNeeded;
        };
        let mut mask: libc::cpu_set_t = std::mem::zeroed();
        for &cpu in &chosen {
            libc::CPU_SET(cpu, &mut mask);
        }
        let mut failed = None;
        for &tid in tasks {
            if libc::sched_setaffinity(tid, size_of::<libc::cpu_set_t>(), &mask) != 0 {
                failed.get_or_insert(std::io::Error::last_os_error().to_string());
            }
        }
        match failed {
            Some(why) => Pinning::Failed(why),
            None => Pinning::Pinned(chosen),
        }
    }
}

#[cfg(windows)]
fn pin(wanted: usize) -> Pinning {
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessAffinityMask, SetProcessAffinityMask,
    };
    // SAFETY: the pseudo-handle of the current process is valid for these calls, and the masks are
    // plain integers the calls write to.
    unsafe {
        let process = GetCurrentProcess();
        let (mut mine, mut system) = (0usize, 0usize);
        if GetProcessAffinityMask(process, &mut mine, &mut system) == 0 {
            return Pinning::Failed(std::io::Error::last_os_error().to_string());
        }
        let allowed: Vec<usize> = (0..usize::BITS as usize)
            .filter(|&cpu| mine >> cpu & 1 == 1)
            .collect();
        let Some(chosen) = cpus_for(&allowed, wanted) else {
            return Pinning::NotNeeded;
        };
        let mask = chosen.iter().fold(0usize, |mask, &cpu| mask | 1 << cpu);
        if SetProcessAffinityMask(process, mask) == 0 {
            return Pinning::Failed(std::io::Error::last_os_error().to_string());
        }
        Pinning::Pinned(chosen)
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn pin(_wanted: usize) -> Pinning {
    Pinning::NotNeeded
}

/// Threads of the machine (see [`Profile::threads`]), at least one.
pub fn threads() -> usize {
    profile().threads.max(1)
}

/// Threads for bulk work that a thread of the game waits on (the scene thread's selection of the
/// scene, the generation of the ring, the Guard and the world map at load): all but one, so that
/// the frame thread always has a core. At least one.
pub fn bulk_threads() -> usize {
    bulk_threads_for(threads())
}

/// [`bulk_threads`] of a machine of `cores` threads.
pub fn bulk_threads_for(cores: usize) -> usize {
    cores.saturating_sub(1).max(1)
}

/// Workers of the loader, which runs all the time: two fewer than the machine's threads, so that
/// the simulation and the frame thread always have one each, and at least one.
pub fn loader_workers() -> usize {
    loader_workers_for(threads())
}

/// [`loader_workers`] of a machine of `cores` threads.
pub fn loader_workers_for(cores: usize) -> usize {
    cores.saturating_sub(2).max(1)
}

/// What every thread of [`par_pieces`] runs before it starts: the client lowers their priority
/// with it, so that bulk work never takes a core from the frame thread (#168).
static WORKER_START: OnceLock<fn()> = OnceLock::new();

/// Sets what each thread of the `par_*` functions that are given `start` runs before it starts.
/// Once: a second call changes nothing.
pub fn set_worker_start(start: fn()) {
    let _ = WORKER_START.set(start);
}

/// What [`set_worker_start`] set, to be passed as the `start` of [`par_pieces_with`].
pub fn worker_start() -> Option<fn()> {
    WORKER_START.get().copied()
}

/// `f` of each piece number in `0..pieces`, on up to `threads` threads, in piece order. The
/// threads take the next number as they finish one, so the pieces that are slow do not leave the
/// rest of the threads waiting at the end of a fixed share. `f` of a number is what is in the
/// result at that number, whichever thread ran it.
pub fn par_pieces<R: Send>(pieces: usize, threads: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    par_pieces_with(pieces, threads, None, f)
}

/// [`par_pieces`] in which each thread runs `start` first, if there is one.
pub fn par_pieces_with<R: Send>(
    pieces: usize,
    threads: usize,
    start: Option<fn()>,
    f: impl Fn(usize) -> R + Sync,
) -> Vec<R> {
    let threads = threads.clamp(1, pieces.max(1));
    if threads == 1 {
        return (0..pieces).map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let mut made: Vec<(usize, R)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    if let Some(start) = start {
                        start();
                    }
                    let mut mine = Vec::new();
                    loop {
                        let piece = next.fetch_add(1, Ordering::Relaxed);
                        if piece >= pieces {
                            break mine;
                        }
                        mine.push((piece, f(piece)));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    });
    made.sort_unstable_by_key(|&(piece, _)| piece);
    made.into_iter().map(|(_, r)| r).collect()
}

/// Folds the pieces `0..pieces` into one accumulator a thread: each of up to `threads` threads makes
/// its own with `init` and calls `fold(&mut accumulator, piece)` for each piece it takes. **Which
/// pieces a thread takes is not fixed**, so the accumulators come back in no particular order and
/// what is in each depends on the scheduling: combine them only with an operation that is exact in
/// any order and grouping (a sum of integers, a least, a greatest), never a sum of floats, and
/// use [`par_pieces`] when the result of each piece has to be kept apart.
pub fn par_fold<A: Send>(
    pieces: usize,
    threads: usize,
    init: impl Fn() -> A + Sync,
    fold: impl Fn(&mut A, usize) + Sync,
) -> Vec<A> {
    let threads = threads.clamp(1, pieces.max(1));
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut acc = init();
                    loop {
                        let piece = next.fetch_add(1, Ordering::Relaxed);
                        if piece >= pieces {
                            break acc;
                        }
                        fold(&mut acc, piece);
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|w| w.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    })
}

/// The ranges of `0..len` in pieces of `piece` items, the last one shorter: what a fixed piece size
/// makes of a length.
pub fn ranges(len: usize, piece: usize) -> impl Iterator<Item = Range<usize>> {
    let piece = piece.max(1);
    (0..len.div_ceil(piece)).map(move |n| n * piece..((n + 1) * piece).min(len))
}

/// `f` of each of `items`, in order, on up to `threads` threads, in pieces of `piece` items.
pub fn par_map<T: Sync, R: Send>(
    items: &[T],
    piece: usize,
    threads: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    par_map_with(items, piece, threads, None, f)
}

/// [`par_map`] in which each thread runs `start` first, if there is one.
pub fn par_map_with<T: Sync, R: Send>(
    items: &[T],
    piece: usize,
    threads: usize,
    start: Option<fn()>,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let piece = piece.max(1);
    let parts = par_pieces_with(items.len().div_ceil(piece), threads, start, |n| {
        let range = n * piece..((n + 1) * piece).min(items.len());
        items[range].iter().map(&f).collect::<Vec<R>>()
    });
    let mut all = Vec::with_capacity(items.len());
    all.extend(parts.into_iter().flatten());
    all
}

/// Sets `out[index]` to `f(index)` for every index, on up to `threads` threads, in pieces of
/// `piece` slots. `f` sees only the index, so the result does not depend on the threads.
pub fn par_fill<T: Send>(
    out: &mut [T],
    piece: usize,
    threads: usize,
    f: impl Fn(usize) -> T + Sync,
) {
    par_chunks(out, piece, threads, |from, chunk| {
        for (k, slot) in chunk.iter_mut().enumerate() {
            *slot = f(from + k);
        }
    });
}

/// `f(from, chunk)` for each piece of `piece` slots of `out`, starting at index `from`, on up to
/// `threads` threads. The pieces are disjoint, so each is written by one thread.
pub fn par_chunks<T: Send>(
    out: &mut [T],
    piece: usize,
    threads: usize,
    f: impl Fn(usize, &mut [T]) + Sync,
) {
    let piece = piece.max(1);
    let threads = threads.clamp(1, out.len().div_ceil(piece).max(1));
    if threads == 1 {
        for (n, chunk) in out.chunks_mut(piece).enumerate() {
            f(n * piece, chunk);
        }
        return;
    }
    // The pieces are handed out in order, a lock a piece: a piece is thousands of slots.
    let chunks = Mutex::new(out.chunks_mut(piece).enumerate());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let next = chunks.lock().unwrap_or_else(|e| e.into_inner()).next();
                    let Some((n, chunk)) = next else { break };
                    f(n * piece, chunk);
                }
            });
        }
    });
}

#[cfg(test)]
mod tests;

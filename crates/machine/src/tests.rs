use super::*;

fn big() -> Profile {
    Profile {
        threads: 32,
        memory: 64 * GIB,
        video_memory: Some(24 * GIB),
        ray_queries: true,
    }
}

fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |name| {
        pairs
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.to_string())
    }
}

#[test]
fn the_low_end_profile_lowers_a_big_machine_to_the_reference_and_never_raises_a_small_one() {
    let low = from_environment(big(), env(&[(ENV_PROFILE, "low-end")]));
    assert_eq!(low, Profile::LOW_END);
    let small = Profile {
        threads: 2,
        memory: 4 * GIB,
        video_memory: Some(2 * GIB),
        ray_queries: false,
    };
    assert_eq!(from_environment(small, env(&[(ENV_PROFILE, "low")])), small);
    // A machine that does not know its video memory takes the reference's.
    let unknown = Profile {
        video_memory: None,
        ..big()
    };
    let low = from_environment(unknown, env(&[(ENV_PROFILE, "low-end")]));
    assert_eq!(low.video_memory, Some(4 * GIB));
}

#[test]
fn the_single_overrides_each_lower_one_thing() {
    let p = from_environment(
        big(),
        env(&[
            (ENV_THREADS, "6"),
            (ENV_MEMORY_MIB, "8192"),
            (ENV_VIDEO_MEMORY_MIB, "4096"),
            (ENV_RAY_QUERIES, "0"),
        ]),
    );
    assert_eq!(p.threads, 6);
    assert_eq!(p.memory, 8 * GIB);
    assert_eq!(p.video_memory, Some(4 * GIB));
    assert!(!p.ray_queries);
    // More than the machine has is not more than it has.
    let p = from_environment(
        big(),
        env(&[(ENV_THREADS, "64"), (ENV_MEMORY_MIB, "1000000")]),
    );
    assert_eq!((p.threads, p.memory), (32, 64 * GIB));
}

#[test]
fn a_value_that_is_not_understood_changes_nothing() {
    let p = from_environment(
        big(),
        env(&[
            (ENV_PROFILE, "nonsense"),
            (ENV_THREADS, "0"),
            (ENV_MEMORY_MIB, "lots"),
            (ENV_RAY_QUERIES, "maybe"),
        ]),
    );
    assert_eq!(p, big());
    assert_eq!(
        from_environment(big(), env(&[(ENV_PROFILE, "auto")])),
        big()
    );
}

#[test]
fn the_thread_counts_leave_the_frame_and_simulation_threads_a_core_and_have_a_floor() {
    for (cores, bulk, loader) in [
        (1, 1, 1),
        (2, 1, 1),
        (3, 2, 1),
        (4, 3, 2),
        (8, 7, 6),
        (32, 31, 30),
    ] {
        assert_eq!(bulk_threads_for(cores), bulk, "bulk of {cores}");
        assert_eq!(loader_workers_for(cores), loader, "loader of {cores}");
    }
    assert!(threads() >= 1 && bulk_threads() >= 1 && loader_workers() >= 1);
}

#[test]
fn meminfo_is_read_in_kibibytes() {
    let text =
        "MemTotal:       16384000 kB\nMemFree:         1000 kB\nMemAvailable:   15000000 kB\n";
    assert_eq!(meminfo_kib(text, "MemAvailable"), Some(15_000_000));
    assert_eq!(meminfo_kib(text, "MemTotal"), Some(16_384_000));
    assert_eq!(meminfo_kib(text, "Nothing"), None);
}

#[test]
fn the_machine_has_threads_and_memory() {
    let found = detect();
    assert!(found.threads >= 1);
    assert!(found.memory > 0);
    // On the systems the game runs on the read of free memory works (on Windows this is
    // `GlobalMemoryStatusEx`, which CI is the first to run), and it is not the fallback's guess.
    if cfg!(any(target_os = "linux", windows)) {
        let free = available_memory().expect("the system says how much memory is free");
        assert!(free > 0);
        // Free memory moves between two reads, but it is the system's figure, not the fallback's
        // guess (the profile's 5 GiB): within a quarter of itself.
        assert!(
            found.memory.abs_diff(free) < free / 4,
            "{} and {free}",
            found.memory
        );
    }
}

#[test]
fn a_limits_room_is_the_limit_less_the_use_that_is_not_page_cache_in_either_version_of_cgroups() {
    let room = |limit, used, stat, key| cgroup_room(limit, used, stat, key);
    // v2: `memory.max` is a number or `max`.
    assert_eq!(
        room(
            Some("8589934592\n"),
            Some("1073741824\n"),
            None,
            "inactive_file"
        ),
        Some(7 * GIB)
    );
    assert_eq!(
        room(Some("max\n"), Some("1073741824\n"), None, "inactive_file"),
        None
    );
    // v1: the limit is a number near 2^63 when there is none.
    assert_eq!(
        room(
            Some("4294967296"),
            Some("1073741824"),
            None,
            "total_inactive_file"
        ),
        Some(3 * GIB)
    );
    assert_eq!(
        room(Some("9223372036854771712"), Some("1073741824"), None, "x"),
        None
    );
    // The page cache the kernel can take back is not used memory: 1 GiB used, 600 MiB of it
    // inactive files, so 3 GiB + 600 MiB of a 4 GiB limit is left.
    let stat = "anon 100\nfile 700\ninactive_file 629145600\nactive_file 5\n";
    assert_eq!(
        room(
            Some("4294967296"),
            Some("1073741824"),
            Some(stat),
            "inactive_file"
        ),
        Some(3 * GIB + 600 * (1 << 20))
    );
    // v1 names it `total_inactive_file`, and `inactive_file` there (this group's only) is not it.
    let v1 = "cache 1\ninactive_file 1\ntotal_inactive_file 629145600\n";
    assert_eq!(
        room(
            Some("4294967296"),
            Some("1073741824"),
            Some(v1),
            "total_inactive_file"
        ),
        Some(3 * GIB + 600 * (1 << 20))
    );
    // A key that is missing, or more cache than use, is no change or no room taken, not a wrap.
    assert_eq!(
        room(Some("1000"), Some("500"), Some("anon 1\n"), "inactive_file"),
        Some(500)
    );
    assert_eq!(
        room(
            Some("1000"),
            Some("500"),
            Some("inactive_file 900\n"),
            "inactive_file"
        ),
        Some(1000)
    );
    // Use over the limit is no room, not a wrap; a missing file or text is no limit.
    assert_eq!(room(Some("1000"), Some("5000"), None, "x"), Some(0));
    assert_eq!(room(None, Some("1"), None, "x"), None);
    assert_eq!(room(Some("1000"), None, None, "x"), None);
    assert_eq!(stat_value("a 1\nb 22\n", "b"), Some(22));
    assert_eq!(stat_value("a 1\nab 22\n", "b"), None);
}

#[test]
fn the_ancestors_of_a_cgroup_path_go_up_to_the_root() {
    assert_eq!(with_ancestors("/"), ["/"]);
    assert_eq!(with_ancestors(""), ["/"]);
    assert_eq!(
        with_ancestors("/user.slice/user-1000.slice/session-2.scope"),
        [
            "/user.slice/user-1000.slice/session-2.scope",
            "/user.slice/user-1000.slice",
            "/user.slice",
            "/"
        ]
    );
    assert_eq!(with_ancestors("/a/"), ["/a", "/"]);
}

/// A fake file system of the files that matter to the memory read.
fn files<'a>(list: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |path| {
        list.iter()
            .find(|(name, _)| *name == path)
            .map(|(_, text)| text.to_string())
    }
}

#[test]
fn the_room_is_the_least_over_the_processs_own_cgroup_and_its_ancestors_in_v2() {
    // The process is in /a/b/c. The limit of 2 GiB is on /a, which the container's runtime made;
    // /a/b/c has none (`max`) and the root has none (no file). /a/b/c has used 1.5 GiB, 1 GiB of
    // it page cache; /a has used 1.5 GiB of its own, 0.25 of it cache. The nearest group alone
    // says no limit; the least is /a's: 2 GiB less 1.25.
    let proc_cgroup = "0::/a/b/c\n";
    let fs = files(&[
        ("/sys/fs/cgroup/a/b/c/memory.max", "max\n"),
        ("/sys/fs/cgroup/a/b/c/memory.current", "1610612736\n"),
        (
            "/sys/fs/cgroup/a/b/c/memory.stat",
            "inactive_file 1073741824\n",
        ),
        ("/sys/fs/cgroup/a/memory.max", "2147483648\n"),
        ("/sys/fs/cgroup/a/memory.current", "1610612736\n"),
        ("/sys/fs/cgroup/a/memory.stat", "inactive_file 268435456\n"),
    ]);
    assert_eq!(
        container_room(&fs, proc_cgroup),
        Some(GIB * 2 - (GIB + GIB / 4))
    );
    // A nearer, smaller limit wins.
    let fs = files(&[
        ("/sys/fs/cgroup/a/b/c/memory.max", "1073741824"),
        ("/sys/fs/cgroup/a/b/c/memory.current", "536870912"),
        ("/sys/fs/cgroup/a/memory.max", "2147483648"),
        ("/sys/fs/cgroup/a/memory.current", "0"),
    ]);
    assert_eq!(container_room(&fs, proc_cgroup), Some(GIB / 2));
    // No limit anywhere is none; a path that is not visible (a container's namespace) is skipped
    // and the namespace's root is read.
    assert_eq!(container_room(files(&[]), proc_cgroup), None);
    let fs = files(&[
        ("/sys/fs/cgroup/memory.max", "4294967296"),
        ("/sys/fs/cgroup/memory.current", "1073741824"),
    ]);
    assert_eq!(container_room(&fs, proc_cgroup), Some(3 * GIB));
    assert_eq!(container_room(&fs, "0::/\n"), Some(3 * GIB));
}

#[test]
fn the_room_is_read_in_v1_from_the_memory_controllers_own_group_and_a_hybrid_has_both() {
    let proc_cgroup = "12:cpu,cpuacct:/docker/x\n11:memory:/docker/x\n0::/system.slice/x\n";
    let fs = files(&[
        // The cpu line is not read.
        (
            "/sys/fs/cgroup/cpu,cpuacct/docker/x/memory.limit_in_bytes",
            "1",
        ),
        (
            "/sys/fs/cgroup/memory/docker/x/memory.limit_in_bytes",
            "3221225472",
        ),
        (
            "/sys/fs/cgroup/memory/docker/x/memory.usage_in_bytes",
            "2147483648",
        ),
        (
            "/sys/fs/cgroup/memory/docker/x/memory.stat",
            "total_inactive_file 536870912\n",
        ),
        // No limit on the parent (v1's "no limit").
        (
            "/sys/fs/cgroup/memory/docker/memory.limit_in_bytes",
            "9223372036854771712",
        ),
        (
            "/sys/fs/cgroup/memory/docker/memory.usage_in_bytes",
            "2147483648",
        ),
        // And a v2 group of the hybrid, with the least room of them.
        ("/sys/fs/cgroup/system.slice/x/memory.max", "1073741824"),
        ("/sys/fs/cgroup/system.slice/x/memory.current", "1073741824"),
    ]);
    // v1: 3 GiB less (2 GiB less 0.5 cache) = 1.5 GiB; v2: 0. The least.
    assert_eq!(container_room(&fs, proc_cgroup), Some(0));
    assert_eq!(
        container_room(&fs, "11:memory:/docker/x\n"),
        Some(3 * GIB / 2)
    );
    // Lines that are not understood are skipped.
    assert_eq!(container_room(&fs, "nonsense\n\n"), None);
}

#[test]
fn the_processors_to_pin_to_are_the_first_of_those_allowed_and_none_if_there_is_nothing_to_cut() {
    let allowed = [2, 3, 4, 5, 6, 7];
    assert_eq!(cpus_for(&allowed, 4), Some(vec![2, 3, 4, 5]));
    assert_eq!(cpus_for(&allowed, 1), Some(vec![2]));
    assert_eq!(cpus_for(&allowed, 6), None);
    assert_eq!(cpus_for(&allowed, 9), None);
    assert_eq!(cpus_for(&allowed, 0), None);
    assert_eq!(Pinning::NotNeeded.describe(), "not pinned");
    assert_eq!(Pinning::Pinned(vec![0, 1]).describe(), "pinned to cpus 0,1");
}

/// The mask is really set: a thread of ours is held to two processors and reads them back. (The
/// whole process is not pinned here: that would hold every other test.)
#[cfg(target_os = "linux")]
#[test]
fn a_thread_is_held_to_the_processors_it_is_pinned_to() {
    std::thread::spawn(|| {
        // SAFETY: `cpu_set_t` is plain data.
        let allowed: Vec<usize> = unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            assert_eq!(
                libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut set),
                0
            );
            (0..libc::CPU_SETSIZE as usize)
                .filter(|&cpu| libc::CPU_ISSET(cpu, &set))
                .collect()
        };
        if allowed.len() < 3 {
            return; // a box of one or two processors has nothing to cut to two
        }
        let pinned = pin_tasks(2, &[0]);
        assert_eq!(pinned, Pinning::Pinned(allowed[..2].to_vec()));
        // SAFETY: as above.
        let now: Vec<usize> = unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            assert_eq!(
                libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut set),
                0
            );
            (0..libc::CPU_SETSIZE as usize)
                .filter(|&cpu| libc::CPU_ISSET(cpu, &set))
                .collect()
        };
        assert_eq!(now, allowed[..2]);
    })
    .join()
    .unwrap();
}

/// Work whose cost is very uneven: the pieces at the start are slow.
fn uneven(piece: usize) -> u64 {
    let rounds = if piece < 4 { 20_000 } else { 100 };
    (0..rounds).fold(piece as u64, |h, k| {
        h.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ k
    })
}

#[test]
fn par_pieces_gives_each_pieces_result_in_piece_order_on_any_number_of_threads() {
    let serial: Vec<u64> = (0..97).map(uneven).collect();
    for threads in [1, 2, 4, 7, 16, 200] {
        assert_eq!(par_pieces(97, threads, uneven), serial, "{threads} threads");
    }
    assert!(par_pieces(0, 4, uneven).is_empty());
}

#[test]
fn par_map_and_par_fill_do_not_depend_on_the_threads_or_the_piece_size() {
    let items: Vec<u32> = (0..10_007).collect();
    let serial: Vec<u64> = items.iter().map(|&n| uneven(n as usize % 9)).collect();
    for (piece, threads) in [(1, 1), (16, 4), (64, 3), (4096, 16), (100_000, 8)] {
        let got = par_map(&items, piece, threads, |&n| uneven(n as usize % 9));
        assert_eq!(got, serial, "map, pieces of {piece}, {threads} threads");
        let mut filled = vec![0u64; items.len()];
        par_fill(&mut filled, piece, threads, |i| uneven(i % 9));
        assert_eq!(filled, serial, "fill, pieces of {piece}, {threads} threads");
    }
}

#[test]
fn ranges_cover_a_length_in_pieces_of_a_fixed_size() {
    let r: Vec<_> = ranges(10, 4).collect();
    assert_eq!(r, [0..4, 4..8, 8..10]);
    assert_eq!(ranges(0, 4).count(), 0);
    assert_eq!(ranges(3, 0).count(), 3);
}

#[test]
fn a_panic_in_a_piece_is_not_lost() {
    let result = std::panic::catch_unwind(|| {
        par_pieces(8, 4, |n| {
            assert_ne!(n, 5, "piece five");
            n
        })
    });
    assert!(result.is_err());
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn flags_are_taken_out_of_the_arguments_wherever_they_are_and_the_rest_is_kept_in_order() {
    // Nothing asked: nothing is applied, so the profile can still be read.
    let rest = take_flags(args(&["bench", "--out", "d", "--short"])).unwrap();
    assert_eq!(rest, args(&["bench", "--out", "d", "--short"]));
    for bad in [
        &["--threads"][..],
        &["--threads", "0"],
        &["--threads=x"],
        &["--profile", "fast"],
        &["--profile"],
        &["--memory-mib", "-3"],
    ] {
        assert!(take_flags(args(bad)).is_err(), "{bad:?}");
    }
    // `--no-ray-queries=1` is not a flag of ours.
    assert_eq!(
        take_flags(args(&["--no-ray-queries=1"])).unwrap(),
        args(&["--no-ray-queries=1"])
    );
}

#[test]
fn the_low_end_profile_describes_itself() {
    let text = Profile::LOW_END.describe();
    assert!(text.contains("4 threads") && text.contains("5.0 GiB") && text.contains("off"));
}

/// The mask is really set on Windows: the process is held to one processor, reads it back, and is
/// let go (nextest runs each test in a process of its own, and the mask is put back for `cargo
/// test`, which does not).
#[cfg(windows)]
#[test]
fn the_process_is_held_to_the_processor_it_is_pinned_to() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessAffinityMask, SetProcessAffinityMask,
    };
    // SAFETY: the pseudo-handle of the current process is valid, and the masks are plain integers.
    let mask = || unsafe {
        let (mut mine, mut system) = (0usize, 0usize);
        assert_ne!(
            GetProcessAffinityMask(GetCurrentProcess(), &mut mine, &mut system),
            0
        );
        mine
    };
    let before = mask();
    let allowed: Vec<usize> = (0..usize::BITS as usize)
        .filter(|&cpu| before >> cpu & 1 == 1)
        .collect();
    if allowed.len() < 2 {
        return; // a runner of one processor has nothing to cut
    }
    let pinned = pin(1);
    let now = mask();
    // SAFETY: as above, putting back what was read.
    unsafe {
        assert_ne!(SetProcessAffinityMask(GetCurrentProcess(), before), 0);
    }
    assert_eq!(pinned, Pinning::Pinned(vec![allowed[0]]));
    assert_eq!(now, 1usize << allowed[0]);
    assert_eq!(mask(), before);
}

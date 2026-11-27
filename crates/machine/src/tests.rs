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
fn a_containers_room_is_its_limit_less_its_use_in_either_version_of_cgroups() {
    // v2: `memory.max` is a number or `max`.
    assert_eq!(
        cgroup_room(Some("8589934592\n"), Some("1073741824\n")),
        Some(7 * GIB)
    );
    assert_eq!(cgroup_room(Some("max\n"), Some("1073741824\n")), None);
    // v1: the limit is a number near 2^63 when there is none.
    assert_eq!(
        cgroup_room(Some("4294967296"), Some("1073741824")),
        Some(3 * GIB)
    );
    assert_eq!(
        cgroup_room(Some("9223372036854771712"), Some("1073741824")),
        None
    );
    // Use over the limit is no room, not a wrap; a missing file or text is no limit.
    assert_eq!(cgroup_room(Some("1000"), Some("5000")), Some(0));
    assert_eq!(cgroup_room(None, Some("1")), None);
    assert_eq!(cgroup_room(Some("1000"), None), None);
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

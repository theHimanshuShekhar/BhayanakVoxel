# Research: bit-identical Planet generation across server and clients

Issue: [#12](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/12). Researched 2026-10-28.
Vocabulary follows [`CONTEXT.md`](../../CONTEXT.md): Planet, Voxel, Material, Edit.

Labels used below:
- **[source]** means a primary source states it, and the link follows.
- **[measured]** means I ran it on this dev machine (AMD Ryzen 7 5700G, Debian 13, rustc 1.99.1 / LLVM 23.1.1, GCC 14.2.0). It shows what can happen, not what always happens.
- **[calc]** is my own arithmetic, with every step shown.
- **[inference]** is my reading of the sources. No source says it directly.
- **[unverified]** means I could not confirm it from a primary source.
- **[recommendation]** is a judgement call.

## TL;DR

1. **Bit-identical CPU generation is achievable in Rust without fixed-point.** Rust gives exact IEEE 754 results for `+ - * /`, `%`, `sqrt`, `mul_add`, `as` casts and comparisons. It never fuses `a*b+c` into an FMA behind your back ([RFC 3514](https://github.com/rust-lang/rfcs/blob/master/text/3514-float-semantics.md)). The leftover traps are transcendental functions (`sin`, `exp`, `powf`, `powi`, …), whose precision Rust explicitly leaves unspecified ([`f64` docs](https://doc.rust-lang.org/std/primitive.f64.html)). The others are NaN and signed-zero bits, data-structure order, threads, and dependency updates.
2. **Measured:** the same Rust f64/f32 noise gave the **same digest** across `target-cpu=x86-64`, `x86-64-v3`, `native` and a debug build. Rust emitted **zero** FMA instructions even with FMA available. The same noise in C under GCC produced **three different digests** from three flag sets (default, `-march=x86-64-v3`, `-ffast-math`). Separately, glibc's `sin`/`exp`/`pow` disagreed with the pure-Rust `libm` crate on **3–10 % of inputs**.
3. **The GPU cannot be authoritative.** WGSL lets implementations reassociate, fuse, flush subnormals and pick either rounding direction, and its `sin` only promises an absolute error of 2⁻¹¹ ([WGSL §15.7](https://www.w3.org/TR/WGSL/#floating-point-evaluation)). Vulkan SPIR-V has the same freedoms ([Vulkan spec, spirvenv](https://github.com/KhronosGroup/Vulkan-Docs/blob/main/appendices/spirvenv.adoc)). Use GPU generation, if at all, only for far visual detail that never feeds collision, Edits or hashes.
4. **Prior art mostly dodges the problem.** Minecraft, Luanti and Veloren generate on the server and stream chunks. Space Engineers is the closest match to this plan: clients rebuild an unedited planet from `(seed, generator, radius)`, and the server sends voxel storage once it has changed. Factorio and Box2D v3 show cross-platform float determinism is practical. Their recipe: no fast-math, no FMA, your own trig, and a CI hash test on every platform.
5. **CI strategy:** a `planet_gen` crate gets golden chunk hashes, checked by a GitHub Actions matrix covering Linux/Windows × x64/ARM64 × debug/release × baseline/`x86-64-v3`. A final job requires every runner's hashes to match each other and the committed golden file. Clippy bans `sin`/`powf`/`HashMap` in the crate, and a rule ties golden-file changes to a `GENERATOR_VERSION` bump.

## Recommendation (plain language)

The generator should be **one small, pure Rust library** that the server and client both link. Given a seed, a generator version and an integer chunk coordinate, it returns Voxels. Nothing else may influence the result.

The rules, in priority order:

1. **Integer coordinates in, Voxels out.** The generator never sees `f32` world positions. At 1,000 km from the Planet's centre, an `f32` can only step in 6.25 cm increments [calc: 1,000,000 m lies in [2¹⁹, 2²⁰), so ulp = 2¹⁹⁻²³ = 2⁻⁴ m = 0.0625 m]. That is too coarse for 10 cm Voxels. Work in `i64` Voxel coordinates and convert to `f64` inside the generator. [recommendation]
2. **Floats are fine, but only the exact operations:** `+ - * /`, `sqrt`, `floor`/`ceil`/`round`/`trunc`, `abs`, comparisons and `as` casts. Rust guarantees these on x86-64 and AArch64 ([RFC 3514](https://github.com/rust-lang/rfcs/blob/master/text/3514-float-semantics.md), [f64 docs](https://doc.rust-lang.org/std/primitive.f64.html)). [recommendation]
3. **No standard-library transcendentals in the generator.** Where `sin`/`cos`/`atan2`/`exp`/`pow` are needed, call the pure-Rust [`libm`](https://github.com/rust-lang/compiler-builtins/tree/main/libm) crate pinned to an exact version (`libm = "=0.2.16"`). Better still, use a cube-sphere mapping that needs only `sqrt` and division. Replace `x.powi(4)` with `let x2 = x * x; x2 * x2`. [recommendation]
4. **Integer hashing for all randomness.** Use a stateless integer hash of `(seed, cell coordinates)`, such as Squirrel Eiserloh's noise-based RNG ([GDC 2017](https://www.gdcvault.com/play/1024365/Math-for-Game-Programmers-Noise)). Avoid a stateful RNG whose output depends on generation order. Integer wrap-around is fully defined in Rust release builds when written as `wrapping_mul`. [recommendation]
5. **Order independence.** A chunk's Voxels depend only on `(seed, version, coord)`. They never depend on which chunks were generated first, on thread count, or on `HashMap` iteration order, which is randomly seeded ([HashMap docs](https://doc.rust-lang.org/std/collections/struct.HashMap.html)). Never sum floats across threads; rayon documents that parallel float sums are not deterministic ([rayon source](https://github.com/rayon-rs/rayon/blob/main/src/iter/mod.rs)). [recommendation]
6. **Write your own small noise module, or vendor and patch one.** `noise-rs` (0.9.0, `develop` branch) uses `powi(4)` in OpenSimplex/SuperSimplex and `powf(2.0)` in Worley ([open_simplex.rs](https://github.com/Razaekel/noise-rs/blob/develop/src/core/open_simplex.rs#L21), [worley.rs](https://github.com/Razaekel/noise-rs/blob/develop/src/core/worley.rs#L76)). Rust documents both functions as having non-deterministic precision. FastNoise2 picks a SIMD level at runtime and makes no cross-level determinism claim ([README](https://github.com/Auburn/FastNoise2)). A Perlin or OpenSimplex2 implementation with an integer hash is about 200 lines and easy for an agent to write against golden tests. [recommendation]
7. **The generator version is part of the data.** Any change to generator output bumps `GENERATOR_VERSION`. Server and client compare version plus probe hashes at join (see the CI section). Pin the toolchain with `rust-toolchain.toml` and commit `Cargo.lock`, because a dependency or `std` update may change transcendental results. [recommendation]
8. **The CPU generates every LOD that matters.** That covers collision, Edits and anything hashed. The GPU may draw far, coarse, never-touched detail if that turns out to be needed for Zoom performance. [recommendation]

**Do not go fixed-point for terrain generation.** Rust floats already give the needed guarantee. A fixed-point noise in my test cost about the same as float noise (see the cost section), so speed is not the issue. The cost is extra beginner pitfalls: overflow, choosing Q formats, no `sin`/`log` in the [`fixed`](https://docs.rs/fixed/latest/fixed/) crate. **Do use integers for Edit geometry.** "Which Voxels does this dig sphere remove?" should be an integer radius-squared test on Voxel coordinates. This agrees with the networking research (#6). [recommendation]

## 1. Float pitfalls

### 1.1 What Rust guarantees

- **Basic operations are exact IEEE 754-2008.** RFC 3514 says `+`, `-`, `*`, `/`, `%`, `abs`, `copysign`, `mul_add`, `sqrt`, `as` casts and comparisons "produce results that exactly match IEEE 754-2008". The rounding mode is round-to-nearest-even and nothing traps. Only NaN bits are non-deterministic ([source](https://github.com/rust-lang/rfcs/blob/master/text/3514-float-semantics.md)).
- **No implicit FMA contraction.** The RFC notes that strict IEEE semantics "precludes many transformations, such as turning `a*b + c` into FMA operations". Fused behaviour is only available through `mul_add` ([source](https://github.com/rust-lang/rfcs/blob/master/text/3514-float-semantics.md)).
- **The default floating-point environment is assumed.** Status bits are "not observable". Changing rounding modes or other control bits from inline assembly is UB unless they are restored ([source](https://github.com/rust-lang/rfcs/blob/master/text/3514-float-semantics.md)). In practice: never touch MXCSR/FTZ/DAZ, and never link C code compiled with `-ffast-math`. Clang links `crtfastmath.o`, which sets FTZ/DAZ for the whole process ([Clang manual](https://clang.llvm.org/docs/UsersManual.html)).
- **Subnormals must not be flushed**, with a stated exception for 32-bit ARM NEON ([source](https://github.com/rust-lang/rfcs/blob/master/text/3514-float-semantics.md)). That target is irrelevant here: x86-64 and AArch64 are the targets.
- **The x87 / i586 / i686 targets are known exceptions,** listed as "bugs in the Rust implementation for those targets" ([source](https://github.com/rust-lang/rfcs/blob/master/text/3514-float-semantics.md)). Do not ship 32-bit x86 builds.
- **`sqrt` and `mul_add` are correctly rounded.** The docs say they are "guaranteed to be the rounded infinite-precision result … guaranteed not to change". The same goes for `floor`, `ceil`, `round`, `trunc` and `rem_euclid` ([f64 docs](https://doc.rust-lang.org/std/primitive.f64.html)).
- **Transcendentals are not.** `sin`, `cos`, `exp`, `ln`, `powf` and `powi` all say: "The precision of this function is non-deterministic. This means it varies by platform, Rust version, and can even differ within the same execution from one invocation to the next." `powi` also "might have a different sequence of rounding operations than `powf`" ([f64 docs](https://doc.rust-lang.org/std/primitive.f64.html)).
- **Small traps:**
  - `min`/`max` on `+0.0` vs `-0.0` "either input may be returned non-deterministically".
  - NaN sign and payload are non-deterministic ([f64 docs](https://doc.rust-lang.org/std/primitive.f64.html)).

  Rule: the generator never branches on the sign of zero or on NaN bits. Debug-assert that values are finite.

### 1.2 Measured on this machine

All [measured]. These are single-machine results, intended as illustrations.

| Experiment | Result |
|---|---|
| `f64::sin` (glibc 2.41) vs `libm::sin` (crate 0.2.16), 10 M inputs in [-1000, 1000) | 309,224 differ (3.1 %) |
| `f64::exp` vs `libm::exp` | 979,242 differ (9.8 %) |
| `f64::powf(x, 1.7)` vs `libm::pow` | 1,010,087 differ (10.1 %) |
| `f32::sin` vs `libm::sinf` | 126,550 differ (1.3 %) |
| `a.mul_add(b, c)` vs `a * b + c`, random operands | 2,457,840 of 10 M differ (24.6 %) |
| Rust f64+f32 Perlin digest, `target-cpu` = `x86-64` / `x86-64-v3` / `native` / debug build | identical: `3eb2fa9f8f302f88` in all four |
| `vfmadd` instructions in the Rust binary built with `-C target-cpu=x86-64-v3` | 0 |
| GCC 14 compiling `a*b+c`: `-O2` / `-O2 -march=x86-64-v3` / `… -ffp-contract=off` / `-O2 -std=c11 -march=x86-64-v3` | `mulsd addsd` / **`vfmadd132sd`** / `vmulsd vaddsd` / `vmulsd vaddsd` |
| Same Perlin in C, GCC 14: `-O2` / `-O2 -march=x86-64-v3` / `… -ffp-contract=off` / `-O2 -ffast-math` | `4fedc812…` / **`880fe1a8…`** / `4fedc812…` / **`9c033d04…`** |
| That C Perlin (with a height gradient), 256³ samples: values differing between `-O2` and `-O2 -march=x86-64-v3` | 9,265,072 of 16,777,216 (f64), 2,796,190 (f32). **Solid/air flips at threshold 0: 0** |

How to read the last row: FMA-sized differences are about 1 ulp, so a Voxel flips from solid to air only when the density lies within about 1 ulp of the threshold. That is rare per Voxel. A Planet has around 10⁸ surface Voxels per km² at 10 cm [calc: (1000 m / 0.1 m)² = 10⁸]. Generators also tend to amplify small differences through `floor()` into Material bands, hash-of-float decisions and iterative erosion. So the realistic failure is **rare, silent, and permanent once Edits are layered on top**. It is not "the whole terrain looks different". This is why the CI test hashes Voxels and does not compare floats with a tolerance. [inference]

### 1.3 C++ toolchains (the alternative engine path)

If the engine goes C++/Vulkan, every one of these defaults must be overridden:

- **GCC:** "The default is -ffp-contract=off for C in a standards compliant mode (-std=c11 or similar), -ffp-contract=fast otherwise" ([GCC docs](https://gcc.gnu.org/onlinedocs/gcc/Optimize-Options.html)). GNU dialects (the usual default) fuse across statements whenever the target has FMA, as measured above.
- **Clang:** `-ffp-contract=on` is the default, meaning fusion within one statement ("default for languages other than CUDA/HIP"). `-ffast-math` and `-ffp-model=fast` set it to `fast` ([Clang manual](https://clang.llvm.org/docs/UsersManual.html)).
- **MSVC:** `/fp:precise` is the default. Since Visual Studio 2022, "Floating-point contractions aren't generated by default under /fp:precise". Earlier versions could contract. `/fp:fast` enables `/fp:contract` ([MSVC docs](https://learn.microsoft.com/en-us/cpp/build/reference/fp-specify-floating-point-behavior)).
- **Standard library math differs per platform.** Factorio found that "trigonometric functions (sin, cos, asin, atan, etc.) are not guaranteed to give same results across different platforms" and wrote its own ([FFF #36](https://www.factorio.com/blog/post/fff-36)). It later had to replace `printf` as well, because GCC and MSVC format `%g` differently ([FFF #108](https://www.factorio.com/blog/post/fff-108)).
- **The required C++ recipe:** `-ffp-contract=off` with no `-ffast-math`/`-Ofast` on GCC/Clang; `/fp:precise` without `/fp:contract` and no `/fp:fast` on MSVC 2022+; vendored math functions; and the same CI matrix. This is noticeably more fragile than Rust, where the safe behaviour is the default. Rust's own risk is concentrated in `std` transcendentals and crates. [inference]

### 1.4 SIMD width

- **Element-wise SIMD is safe.** Each lane performs the same correctly rounded IEEE operation, and Rust never reassociates float sums during auto-vectorization, because that would violate RFC 3514's exact semantics. The `target-cpu` digests above match. [inference, consistent with measurement]
- **Risks come from code that picks a different algorithm per SIMD level:**
  - FastNoise2's runtime dispatch ([README](https://github.com/Auburn/FastNoise2)).
  - Hand-written `std::arch` paths that use FMA intrinsics.
  - Different horizontal-reduction orders.
  - Rapier, for example, makes `enhanced-determinism` incompatible with `simd8` because that feature "changes the SIMD lane width" ([Rapier docs](https://rapier.rs/docs/user_guides/rust/determinism/)).
- **`glam`:** its `libm` feature "enables libm and prefers it over std" ([glam Cargo.toml](https://github.com/bitshifter/glam-rs/blob/main/Cargo.toml)), and `scalar-math` disables SIMD ([glam README](https://github.com/bitshifter/glam-rs)). If `glam` is used inside the generator, turn on `libm`. Whether its SIMD paths (`Vec3A` dot products, etc.) match scalar bit-for-bit is **[unverified]**, so cover it with the golden test, or use plain `f64` structs in the generator.
- **AArch64:** Rust's guarantees are target-independent apart from the listed exceptions ([RFC 3514](https://github.com/rust-lang/rfcs/blob/master/text/3514-float-semantics.md)). I did not run the experiment on ARM. ARM64 runners in the CI matrix close this gap. **[unverified on hardware]**

## 2. Integer and fixed-point noise, and its cost

- **Options:**
  - Pure integer hash noise (value or cellular), e.g. Squirrel3/SquirrelNoise5 ([GDC talk](https://www.gdcvault.com/play/1024365/Math-for-Game-Programmers-Noise)). Fully exact, and cheap: a few multiplies and xor-shifts.
  - Fixed-point gradient noise, e.g. Q16.16 with `i64` intermediates.
  - Float gradient noise with an integer hash. This is Luanti's design: integer lattice hash, float interpolation ([noise.cpp](https://github.com/luanti-org/luanti/blob/master/src/noise.cpp#L174)).
- **Measured cost** [measured]: the same 3D Perlin, scalar, integer hash in all versions, 128³ samples (a 12.8 m cube of 10 cm Voxels):

  | Variant | ns/sample |
  |---|---|
  | f32 | 25.6–25.8 |
  | f64 | 25.1–25.5 |
  | fixed Q16.16 (i64 math) | 27.7–27.9 |

  The hash and table lookups dominate, so fixed-point is about 10 % slower here. It is not dramatically slower. These are scalar loops; I did not hand-vectorize any variant. Both float versions were deterministic across CPU targets anyway. The cost of fixed-point is engineering, not speed:
  - It loses precision at large coordinates unless Q formats are chosen per stage.
  - It overflows silently in release builds unless `wrapping_*`/`checked_*` is used.
  - The `fixed` crate provides no trig or transcendentals: "No trigonometric functions are provided" ([fixed docs](https://docs.rs/fixed/latest/fixed/)).
- **Where integers are worth it** [recommendation]:
  - Hashing and seeding.
  - Edit rasterization.
  - Anything that must also run on the GPU and match exactly. WGSL integer arithmetic is defined: overflow results are "modulo 2^bitwidth" ([WGSL](https://www.w3.org/TR/WGSL/)).

## 3. Can GPU generation be authoritative?

**No, not for float math, and not worth trying even for integer math in the Vertical slice.**

- **WGSL** (what wgpu exposes) [source: [WGSL §15.7](https://www.w3.org/TR/WGSL/#floating-point-evaluation)]:
  - "An implementation may reassociate operations."
  - It may also fuse operations "if the transformed expression is at least as accurate".
  - Subnormal inputs and outputs "may be flushed to zero".
  - "WGSL does not specify a rounding mode": a correctly rounded result may go up or down.
  - `x / y` is 2.5 ULP.
  - `sin` only promises "Absolute error at most 2⁻¹¹" on [-π, π].
  - `exp` is "3 + 2·|x| ULP".
  - `pow` is "Inherited from exp2(y * log2(x))".
  - WGSL's `fma` "may expand to an ordinary multiply … and an add".
- **Vulkan/SPIR-V** (the C++ path): "Implementations may rearrange floating-point operations using any of the mathematical properties governing the expressions in precise arithmetic". Only the `NoContraction` decoration prevents this. Denormals "may be flushed to 0" unless float-controls execution modes are used ([spirvenv.adoc](https://github.com/KhronosGroup/Vulkan-Docs/blob/main/appendices/spirvenv.adoc)).
- **Integer-only GPU generation** is exact on paper. In practice WGSL has no 64-bit integers without the native-only `SHADER_INT64` feature ([wgpu Features](https://docs.rs/wgpu/latest/wgpu/struct.Features.html)), driver and shader-compiler bugs would only show up on players' machines, and CI runners have no GPUs. [inference]
- **Allowed use** [recommendation]: GPU-generated **far visual detail only**, beyond the range where any player can collide or Edit. Any Voxel that feeds collision, Edits, path-traced hit testing near the player, or a hash must come from the CPU generator. Expect a small visual "pop" when a CPU chunk replaces GPU far detail. Veloren does something similar: it sends clients a coarse server-computed altitude map for distant terrain (`WorldMapMsg.alt`) and does not reproduce far terrain bit-exactly ([world_msg.rs](https://gitlab.com/veloren/veloren/-/blob/master/common/net/src/msg/world_msg.rs#L24)).

## 4. Prior art

| Game / library | What it does | Lesson |
|---|---|---|
| **Minecraft (Java)** | The server sends chunks: "Chunk Data and Update Light … Sent when a chunk comes into the client's view distance, specifying its terrain" ([protocol wiki](https://minecraft.wiki/w/Java_Edition_protocol/Packets#Chunk_Data_and_Update_Light)). "Multiplayer servers no longer send the seed to clients." Generator updates are logged as "Terrain-breaking change to world gen". Java and Bedrock match terrain for a seed, but structures and features "generate differently" ([Seed wiki](https://minecraft.wiki/w/Seed_(level_generation))). Since Java 17 all float operations are strict ([JEP 306](https://openjdk.org/jeps/306)), and `StrictMath` must reproduce fdlibm results ([StrictMath](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/lang/StrictMath.html)), but `Math` is "not defined to return the bit-for-bit same results" ([Math](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/lang/Math.html)). | It streams terrain, so client determinism is not needed. Cross-edition parity was a deliberate engineering effort, and even it is partial. Generator versions really do change output. |
| **Luanti (Minetest)** | Server mapgen; `TOCLIENT_BLOCKDATA` sends a "serialized MapBlock" ([networkprotocol.h](https://github.com/luanti-org/luanti/blob/master/src/network/networkprotocol.h#L70)). Noise uses an integer lattice hash with float interpolation ([noise.cpp](https://github.com/luanti-org/luanti/blob/master/src/noise.cpp#L174)). Release builds add `-fno-math-errno -fno-trapping-math -fno-signed-zeros` and force SSE on 32-bit x86 after an x87 test failure ([CMakeLists.txt](https://github.com/luanti-org/luanti/blob/master/src/CMakeLists.txt#L979-L1000), [issue #11810](https://github.com/luanti-org/luanti/issues/11810)). | Under GCC's default `-ffp-contract=fast` plus `-fno-signed-zeros`, two Luanti builds are probably not bit-identical. That is harmless because the server is authoritative over terrain. [inference] |
| **Veloren** (Rust) | The server generates and sends `TerrainChunkUpdate { key, chunk }`, deflate- or PNG-compressed ([server.rs](https://gitlab.com/veloren/veloren/-/blob/master/common/net/src/msg/server.rs#L194)). Clients get a coarse world map with an altitude grid for the far view ([world_msg.rs](https://gitlab.com/veloren/veloren/-/blob/master/common/net/src/msg/world_msg.rs#L24)). | A Rust voxel game that chose streaming and avoided the determinism problem. Its far view is server-provided data, not client-generated. |
| **Space Engineers** | Planet storage saves only `Version`, `Seed` and the generator name ([MyPlanetStorageProvider.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/Sandbox.Game/Engine/Voxels/Planet/MyPlanetStorageProvider.cs)). Replication sends the full saved storage only when `contentChanged` is true; otherwise the client builds the voxel map itself ([MyVoxelReplicable.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/Sandbox.Game/Game/Replication/MyVoxelReplicable.cs)). Procedural asteroids "are re-inserted with the exact same shape" from a noise seed ([dev blog](https://blog.marekrosa.org/2014/12/space-engineers-super-large-worlds_17/)). | The closest match to this plan, and evidence that it works in a shipped game. However, SE clients were Windows/.NET x64 only, so it never had to cross compilers or operating systems. I found no Keen statement about how they guarantee bit-identity. [unverified] |
| **Factorio** | Deterministic lockstep. Custom trig after cross-OS replays desynced ([FFF #36](https://www.factorio.com/blog/post/fff-36)), and custom `printf` ([FFF #108](https://www.factorio.com/blog/post/fff-108)). | Cross-OS float determinism works in a shipped C++ game, but every libc call is suspect. |
| **Box2D v3** (C) | Cross-platform deterministic since v3.1 ([FAQ](https://box2d.org/documentation/md_faq.html)). Recipe: never fast-math, "FMA must be disabled for cross-platform determinism", custom `atan2f` because it "gives different answers on different platforms". `sqrtf` is deterministic. A CI "Falling Hinges" test checks a step count plus a transform hash on MSVC/Clang/GCC × x64/ARM ([Determinism post](https://box2d.org/posts/2024/08/determinism/)). | The template for our CI test. It reports no measurable performance cost. |
| **Rapier** (Rust) | `enhanced-determinism` gives identical results "on two different computers (including different OS and/or different processors)". It requires IEEE 754-2008 platforms and the use of `nalgebra`'s `ComplexField::sin` rather than `f32::sin` ([Rapier docs](https://rapier.rs/docs/user_guides/rust/determinism/)). | The same rule applies in Rust: route transcendentals through a pure-Rust math library. Relevant later for Debris physics. |

## 5. CI test strategy (concrete)

Goal: any change that makes any supported build produce different Voxels fails CI before merge.

1. **Golden probe set.** `planet_gen/tests/golden.rs` generates a fixed list of about 200 probe chunks for 3 fixed seeds. The probes cover the equator, both poles, cube-face edges and corners (if cube-sphere), the deep interior and core, the highest mountain and deepest ocean floor, and every LOD level the CPU produces. For each probe, it hashes the raw Voxel bytes (Material IDs in a fixed order) with an in-crate FNV-1a or a pinned `blake3`. It writes `chunk_id hash` lines and compares them to `planet_gen/golden/v{GENERATOR_VERSION}.txt`. Use an exact hash, never a float tolerance.
2. **Platform matrix (GitHub Actions)** on free public-repo runners ([runner list](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)):
   - `ubuntu-24.04` (x64, `x86_64-unknown-linux-gnu`)
   - `windows-2025` (x64, `x86_64-pc-windows-msvc`)
   - `ubuntu-24.04-arm` and `windows-11-arm` (ARM64, the nice-to-have)
   - optionally `macos-latest` (arm64)
   - × `--release` and debug
   - × on x64: default `target-cpu` and `RUSTFLAGS=-C target-cpu=x86-64-v3` (an FMA/AVX2 canary)
3. **Cross-runner comparison job.** Every matrix job also hashes 50 chunks for a seed derived from `github.run_id` and uploads its hash file as an artifact. A final job downloads all of them and fails unless they are byte-identical. This catches divergence even where nobody has a golden value yet.
4. **Ship-binary check.** Build the real headless-server and client binaries with their actual release profiles (LTO, `codegen-units`, `panic`, etc.). Run a `--gen-probe` subcommand on each and compare with the golden file. This catches profile-specific differences that a `cargo test` build would miss.
5. **Order and thread independence test.** Generate a 4×4×4-chunk region in three ways: single-threaded in order, shuffled, and with rayon on 1 and 8 threads. Assert identical bytes.
6. **Static guards.** In `planet_gen/clippy.toml`, use [`disallowed-methods`](https://doc.rust-lang.org/clippy/lint_configuration.html) to ban:
   - `f32/f64::{sin, cos, tan, asin, acos, atan, atan2, exp, exp2, ln, log, log2, log10, powf, powi, hypot, cbrt, sinh, cosh, tanh}`, with `reason = "use libm (pinned)"`
   - `std::collections::HashMap::new`
   - `std::time::*`

   Also deny `std::arch`/`unsafe` in the crate, and run `cargo clippy -p planet_gen -- -D clippy::disallowed_methods` in CI.
7. **Version discipline.** A CI step fails if any `golden/*.txt` changed without `GENERATOR_VERSION` changing too. A simple `git diff` script against the merge base is enough.
8. **Toolchain drift canary.** A weekly scheduled job runs the golden test on Rust `beta` and on the latest dependency versions (`cargo update`), so drift shows up before a planned upgrade.
9. **Runtime canary** (not CI, but the same idea) [recommendation]. At join, the client sends `GENERATOR_VERSION` plus the hash of 3–5 probe chunks. The server rejects a mismatch with a clear error. Combined with the networking research's per-chunk hash fallback (#6), a silent divergence in the field becomes a detectable, repairable event rather than corrupted Edits.

## 6. Open questions for the decision tickets

**For "Choose engine foundation & language" (#7):**
- Rust is the safer pick on this axis. Contraction is off by default and IEEE semantics are language-guaranteed. In C++ the safe settings must be forced per compiler, and one stray `-march`/`-ffast-math` silently breaks determinism, as measured. Does that weigh enough to settle the Rust vs C++ choice, given that a beginner plus AI agents will touch the build files?
- If a third-party engine or noise library is adopted, can its generation path be audited for `std` transcendentals and runtime SIMD dispatch? Or does the generator always need to be first-party code?
- Is ARM64 in or out for the Vertical slice? It adds two CI runners, but no extra code under these rules.

**For "Choose network architecture" (#11):**
- These findings support client-side generation of unedited terrain with Edits as operations (#6's design). One rule must go into the protocol from day one: a `GENERATOR_VERSION` + probe-hash handshake.
- What happens on a per-chunk hash mismatch in the field? Should the client send a hash of every chunk it generates near an Edit, so the server can push the true chunk? Or is the join handshake plus CI enough?
- Edit shapes must be specified in integer Voxel coordinates (no float `sin`/`cos` for rotated tools). Does that limit tool designs the game wants?
- Far LOD for Zoom: does the client generate it on the CPU (deterministic, cacheable), on the GPU (fast, visual only), or does it come from the server like Veloren's world map? This affects bandwidth and how fast Zoom-out appears.

## Sources not fully verified

- Space Engineers' approach to bit-identity (compiler settings, x64-only assumption) is not documented in the sources I found. **[unverified]**
- Whether `glam`'s SIMD types match its scalar path bit-for-bit. **[unverified]** Cover it with the golden test.
- All measurements are from one x86-64 machine. ARM64 and Windows/MSVC behaviour of the same code was not run here. The CI matrix in section 5 is the verification plan. **[unverified on hardware]**

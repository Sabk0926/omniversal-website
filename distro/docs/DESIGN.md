# Omnia OS — design record

Living document. Records what is settled, why, and what is still open. Supersedes the
earlier docs in this directory, which described a desktop-first, confirm-on-write design
with a Python runtime and no capability generation. That design is abandoned; those docs
were deleted rather than left to mislead.

## Thesis

The user never needs to know a command, and when the OS lacks a capability, it builds
one, proves it works, and permanently gains it.

The design tension this creates — freedom to invent vs. discipline to remember — is
resolved by making *declaration*, not execution, the terminal step of building. See the
README for the lifecycle.

## Settled decisions

| # | Decision | Choice | Reasoning |
|---|---|---|---|
| 1 | Kernel role | Kernel notices (uevents, eBPF, `/dev/omnia`); userspace reasons | No FPU/SIMD in ring 0, no multi-hundred-MB tensor allocations, no forked kernel, keeps stock Ubuntu kernel updates and Secure Boot |
| 2 | Hardware floor | 4 GB ARM64 SBC, 1.5B int4 orchestrator | If the OS doesn't work on the floor target it isn't in the design; forces the parts library (below) rather than wishful code generation |
| 3 | Shipping | One multi-arch deb source → amd64 ISO + arm64 `.img` | Familiar apt upgrade path; one rootfs recipe, two assemblers |
| 4 | Autonomy | Full, everywhere; nothing prompts the user | User decision. Safety is provided by floors that make bad outcomes impossible, not by prompts that make them the user's fault |
| 5 | Language | Rust userspace, C for module + BPF, llama.cpp for inference | Ctrl-G pays interpreter startup per keypress (Python: ~200–300 ms on a Pi off SD, Rust: ~2 ms); 55 MB of CPython in the initramfs vs ~3 MB static; and Python cannot consume BPF ringbufs without a C shim |
| 6 | Code origin | **Both**: compose from a vetted parts library, *and* escalate to a large model for novel code | A 1.5B model cannot write a correct backup daemon but can reliably wire `snapshot + schedule + encrypt`; escalation covers the long tail where hardware/network allows |
| 7 | Generated-code trust | Declared-permission sandbox; undeclared access denied by construction | Manifest renders into systemd hardening (`DeviceAllow`, `ProtectSystem`, `SystemCallFilter`) + seccomp — reuses a mature sandbox rather than inventing one |
| 8 | Ring 0 | Prefer userspace drivers; self-written kernel modules only with rollback sentinel armed | There is no sandbox for ring 0; a userspace driver can be killed, a module cannot |
| 9 | Proof | A capability is not kept until it writes a test that proves it *works* and that test passes; the test is retained | "The backup ran" and "the backup can be restored" are different claims and only the second matters; retention turns capabilities into a regression suite |

## Safety: three floors, no prompts

1. **Kernel floor (BPF-LSM)** — enforced below the root daemon, pinned before it starts,
   in a cgroup it cannot edit. Covers: boot-device writes, undo-journal deletion, guard
   self-modification, root unmount, human-access paths (sshd keys, sudoers, console).
   `omniad` refuses the executor claim unless the guard reports ACTIVE.
2. **Capability sandbox** — the permission manifest, rendered into systemd unit hardening
   plus seccomp.
3. **Reversibility** — every action records its reverse before executing; generated
   capabilities are `.deb`s so removal is `apt remove`; a boot sentinel replays the undo
   journal if a boot after an autonomous change fails to reach `multi-user.target` twice.

Plus an action budget with a circuit breaker: repeated failed remediation of the same
symptom drops to observe-only rather than looping unattended.

## The driver ladder

A uevent with VID/PID, PCI class or DT `compatible` walks cheapest-and-safest first.

| # | Situation | Action | Cost |
|---|---|---|---|
| 1 | In-kernel module not loaded | `modprobe`, verify bind | filesystem lookup, no model |
| 2 | Missing firmware blob | fetch from `linux-firmware`, reload | filesystem lookup, no model |
| 3 | Needs quirk / ID / udev rule / modprobe option / DT overlay | generate **config, not code** | 1.5B classification |
| 4 | Out-of-tree source exists | fetch, build against running kernel, DKMS, load, verify | **see open question 1** |
| 5 | USB/I2C/SPI/serial, no driver anywhere | generate a **sandboxed userspace driver** | parts composition |
| 6 | Novel ring-0 device | scaffold + harness, load only with sentinel armed | escalation |

Rows 1–4 cover the large majority of real "no driver" situations. Row 5 is where the
thesis literally happens and is sandboxable by construction. Row 6's real blocker is
missing information — a driver *is* the register map, and no model infers one from a USB
ID — not code generation.

## Parts library

Vetted, tested building blocks with typed manifests that the small model composes:
`snapshot`, `schedule`, `encrypt`, `sync`, `watch-path`, `notify`, `http-fetch`,
`usb-bulk`, `i2c-read`, `spi-xfer`, `serial`, `framing`, `archive`, `verify-restore`.

This is what makes the 4 GB floor honest. The library grows once, centrally, for
everyone — not per machine.

## Open questions

1. **Where may rung 4 look for driver source?** Fetching and building arbitrary internet
   code into the kernel autonomously is the highest-value rung *and* a supply-chain
   attack surface. Current recommendation: signed/curated sources only (Ubuntu archive,
   `linux-firmware`, a vetted DKMS index), with arbitrary repositories as
   escalation-only. **Not yet decided.**
2. Which 1.5B / 0.5B GGUF builds to pin, and whether their licences permit
   redistribution inside an image.
3. Whether escalation defaults to a large local model, a cloud API, or a builder machine
   on the LAN when more than one is available.
4. How much of the parts library ships in v1 — it determines whether a Pi can build
   anything useful offline.

## Implementation phases

1. **Foundation** — `omnia-abi` (compile-time size asserts + header-parity test),
   `omnia-core`, `omnia-kernel` (device client, native BPF ringbuf mmap consumer, sysfs
   pollers, merged event stream).
2. **Model layer** — probe (CUDA/ROCm/Tegra/RKNN/Hailo/Vulkan/CPU), catalog, llama.cpp
   supervisor with lazy load / idle unload / hot swap, tier routing and escalation.
3. **Forge** — parts, registry, sandbox, and the five-stage pipeline end to end, with
   backup as the reference capability.
4. **Driver ladder** — uevent ingestion, rungs 1–4, then userspace generation (rung 5),
   then rung 6 behind the sentinel.
5. **Autonomy** — event loop, triage, budget/breaker, undo journal, boot sentinel,
   executor claim and heartbeat.
6. **Surface** — `omni` CLI, shell integration, systemd units, initramfs hook.
7. **Packaging, images, docs** — deb set, both image builders, full docs.

## Verification strategy

The test that proves the thesis (`tests/cold-capability/`): start from an image with no
backup tool, issue "back up ~/Pictures nightly", then assert

1. a `.deb` now exists and is installed,
2. its permission manifest names only `~/Pictures` and the destination,
3. its retained test passes,
4. a file restores **byte-for-byte**,
5. `apt remove` leaves the machine clean,
6. asking again does **not** build a second one.

Driver ladder (`tests/devices/`): a `dummy_hcd` USB gadget with an unknown VID/PID
exercises rungs 3 and 5; assert a sandboxed userspace driver is generated, its test
passes, and its unit's `DeviceAllow` names only that device.

Autonomy (`tests/faults/`): fill a disk, kill a unit, stall the scheduler, drive a
thermal event. Each asserts the daemon acted, undo replays cleanly, the budget
decremented, and the kernel guard refuses the out-of-bounds variant.

## Notes on what is built

`kernel/` is real and compiles as C. Highlights worth knowing when reading it:

- `omnia_abi.h` is **padding-free by construction**, and the Rust side asserts struct
  sizes at compile time plus re-parses the header in a test. A DKMS module built from one
  version talking to a package from another otherwise shows up as garbled events rather
  than an error.
- The event ring drops the **oldest** record when full, not the newest: during an
  incident the newest events are the ones that explain it. Sequence numbers skip so the
  gap stays visible.
- Exactly one process may hold the executor claim, and the kernel watchdog emits an event
  if it stops heart-beating — a wedged autonomous daemon is more dangerous than an absent
  one, so its absence must be observable.
- `OMNIA_IOC_EMIT` refuses `OMNIA_EV_GUARD_DENY`. The one event type that attests the
  floor is enforcing must not be forgeable by any userspace writer.
- No `libbpf` anywhere: the ringbuf is consumed by mmapping the map fd directly (stable
  kernel ABI), so the image ships no libbpf and the build needs no libelf.

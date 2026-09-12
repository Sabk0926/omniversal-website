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
| 10 | Observation | **Nothing audits itself.** Every layer is checked by the layer beneath it | A component's own account of itself is worthless when it is the thing that is broken. The kernel watchdog observing the executor, and `OMNIA_IOC_EMIT` refusing `GUARD_DENY`, are the first two instances |
| 11 | Baselines | Learn per-host statistical baselines, not fixed thresholds | Catches gradual degradation (a fan dying over months, a leak over weeks) that no threshold sees; a limit correct for a desktop is wrong for a fanless SBC in a hot cabinet |
| 12 | Audit integrity | Hash-chained records + independent kernel cross-check | Makes tampering detectable without infrastructure or telemetry. Honest limit: detection, not prevention — see "the circularity problem" |
| 13 | Learning | Capabilities, baselines, knowledge base and routing first; **weight updates last** | Capabilities are inspectable, reversible and transferable; weights are none of those, and baking behaviour into weights destroys the audit story |
| 14 | Adapter training | Builder box trains, fleet installs a signed `.deb`; eligible data is verified outcomes (structure, not content) plus explicit user corrections | The 4 GB floor cannot train — that is arithmetic. Verified-only intake is also the defence against model collapse |

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

## Self-diagnosis, self-healing, self-audit

Governed by decision 10: nothing audits itself. Capabilities are checked by the daemon,
the daemon by the kernel, the kernel from off-box.

### Diagnosis — three levels

| Level | Question | Mechanism |
|---|---|---|
| Liveness | is it running? | systemd, plus the kernel watchdog on the executor claim |
| **Correctness** | is it doing the right thing? | **the retained capability tests** |
| Baseline | is this normal *for this machine*? | learned per-host profile |

The middle level is the unusual one and it falls out of the architecture for free. Because
every capability carries a test that proves it *works*, the machine accumulates an
executable definition of "healthy" that grows as it gains capabilities. `omni doctor` is
therefore not a static script: it is "run everything this machine claims it can do."

Conventional monitoring can only report that the backup process exited 0. It can never
report that the backup can be restored.

Baselines tracked: thermal curve, disk growth rate, boot time, RSS ceilings, service
restart frequency, unit start latency. Cold start is roughly a week, during which the
system has no baseline opinion and says so rather than guessing.

### Healing — escalation ladder

Verification is **the same retained test that originally proved the capability**, never
the model's own assessment. Without an objective criterion, self-healing degenerates into
a system that confidently reports success.

| Tier | Action | Reversal |
|---|---|---|
| 0 | restart / reload the unit | trivial |
| 1 | reconfigure | undo journal |
| 2 | repair or reinstall the package | `apt` |
| 3 | **re-forge the capability** | previous `.deb` retained |
| 4 | roll back to last known-good | boot sentinel |
| 5 | stop, degrade safely, report loudly | — |

Tier 3 is the payoff for the whole architecture. Concretely: a kernel upgrade breaks a
generated USB driver, its retained test fails on next boot, and the OS rebuilds the driver
against the new kernel and re-runs the test. If it passes the machine keeps working with
nobody paged. If it fails, tier 4 rolls back the kernel and *then* reports.

This is only possible because the machine knows what its own capabilities are supposed to
do. A system without retained tests has nothing to re-forge against.

### Audit — and the circularity problem

A root daemon writing its own audit log can lie or omit. If `omniad` is compromised or
simply malfunctioning, its account of what it did is worth nothing. Auditing is the one
function that cannot be self-hosted, and claiming otherwise would not survive review.

Implemented:

1. **Hash-chained records** — each entry commits to the previous, making deletion and
   editing detectable. Near-free; unconditional.
2. **Kernel cross-check** — the module independently counts executor actions, and the
   guard counts denials, in maps `omniad` cannot write. A divergence between what the
   daemon logged and what the kernel observed is hard evidence of a problem.

Deferred, and the only real answers to full host compromise: off-box anchoring of the
chain head, and fleet mutual attestation. Both need infrastructure outside the machine.

**The honest limit:** a single fully-compromised box with no external anchor cannot audit
itself. That is arithmetic, not a gap to be engineered away.

Record format requirement: **audit the reasoning, not only the action** — inputs, the plan
chosen, alternatives rejected, and the test result. "Why did it restart postgres at 03:00"
must have an answer other than "it restarted postgres at 03:00."

## Learning

Ordered cheapest and safest first. Weight updates are the last resort, not the first.

| # | Mechanism | Inspectable | Reversible | Transferable |
|---|---|---|---|---|
| 1 | **Capabilities** | yes — a `.deb` | `apt remove` | copy the file |
| 2 | **Baselines** | yes — numbers | delete the row | per-host by nature |
| 3 | **Host knowledge base** — incidents, what worked, what did not | yes — editable text | delete the entry | selectively |
| 4 | **Learned routing / few-shot exemplars** | yes — text | revert | yes |
| 5 | **LoRA adapter** | no — opaque weights | swap the file | yes |
| 6 | Full fine-tune | no | no | no |

Levels 1–4 provide most of what "it is learning" feels like with no loss of auditability.

### Why adapters are nevertheless worth it here

The retained capability tests yield labelled training data as a by-product of normal
operation: every forge attempt is `(situation → plan chosen → test passed or failed)`,
with an *objective* label. Most on-device learning has no ground truth and ends up
training on "the user did not complain," which is noise.

Scope is deliberately narrow: **plan selection** (which parts to compose), **tool-call
format adherence**, and **local vocabulary** (device names, site conventions). Not general
reasoning, not world knowledge.

### Risks and their mitigations

| Risk | Mitigation |
|---|---|
| Catastrophic forgetting — a 1.5B model has little headroom and is *more* fragile to fine-tuning than a large one | The eval suite includes a frozen general-capability regression set; an adapter that regresses it is discarded |
| Model collapse from training on its own output | Only **verified** examples are eligible — the retained test must have passed. Never the model's own say-so |
| Cannot train on the floor target | LoRA on a 1.5B needs ~6–12 GB even with checkpointing and 8-bit optimisers. Training happens on the builder box, never on the board |

### Adapter lifecycle

An adapter is just another capability and inherits the whole pipeline:

```
collect verified examples (+ redacted user corrections)
  → train on the builder box, never on the board
  → PROVE: must beat the incumbent on a frozen eval suite
           (retained capability tests + general-capability regression set)
  → shadow mode: run alongside the incumbent, compare decisions
  → DECLARE: signed .deb, versioned, swappable; fleet installs it
```

An adapter must prove itself before activation exactly as a backup tool must restore a
file. Failing the eval means discarded, never activated; regressing later means
`apt remove` and the previous version returns.

The builder box is the same machine already designated as the escalation target for novel
code generation — one capable machine serves the fleet for both.

### Training-data policy

Eligible: the intent, the plan chosen, which parts were composed, the test result, and
explicit user corrections after redaction review.

Excluded: file contents, command output, and anything the context providers read. The
system learns that *requests shaped like this* are served by `snapshot + schedule +
encrypt`, without ever learning what is in the photos.

User corrections are the highest-signal data available — a human-labelled correction is
worth many passive examples — but they routinely quote paths and content, so they pass
through redaction before becoming eligible.

Note the asymmetry that motivates the exclusions: levels 1–4 support selective deletion;
**a trained adapter does not.** There is no "forget that one file" once it is in weights.

## Open questions

1. **Where may rung 4 look for driver source?** Fetching and building arbitrary internet
   code into the kernel autonomously is the highest-value rung *and* a supply-chain
   attack surface. Current recommendation: signed/curated sources only (Ubuntu archive,
   `linux-firmware`, a vetted DKMS index), with arbitrary repositories as
   escalation-only. **Not yet decided.**
2. Which 1.5B / 0.5B GGUF builds to pin, and whether their licences permit
   redistribution inside an image — and separately whether they permit LoRA
   fine-tuning and redistribution of the resulting adapter.
3. Whether escalation defaults to a large local model, a cloud API, or a builder machine
   on the LAN when more than one is available.
4. How much of the parts library ships in v1 — it determines whether a Pi can build
   anything useful offline.
5. What the frozen general-capability regression set actually contains, and who owns
   it. It is the only thing standing between incremental learning and a model that has
   quietly forgotten how to do its job.
6. Off-box audit anchoring and fleet mutual attestation are deferred, not rejected.
   They become necessary the moment this ships to someone else's hardware.

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

# Omnia OS design

Working document. Records what we decided and why, plus what's still open.

## What we're building

An Ubuntu 24.04 derivative where you never need to know a command, and where the OS
builds capabilities it doesn't have.

Ask for nightly photo backups on a machine with no backup tool. It builds one, tests that
a file actually restores, packages it as a `.deb`, and installs it. The machine now has
backup. So does any other machine you copy that package to.

## The problem this has to solve

The OS needs freedom to invent things it doesn't have. It also needs to remember what it
invented. Those pull against each other.

Without discipline you get twelve half-working backup scripts, three of them running at
once, and a machine nobody can explain a year later.

Without freedom you get something that only does what someone anticipated.

**The fix: building ends in declaring, not in running.** Every generated capability
becomes a `.deb`. That gives us:

| Need | How `.deb` answers it |
|---|---|
| Undo it | `apt remove` |
| Why is this here? | `dpkg -l` plus a provenance file |
| Put it on my other machine | copy the file |
| Did it break? | its test is retained and re-run |

Building a custom registry would mean reimplementing packaging, badly.

## How a capability gets built

Any request goes through the same five steps. A request can come from you typing, or from
an event that implies a need (a device appeared, a disk is filling).

| Step | What happens |
|---|---|
| 1. Look it up | Do we already have this? If yes, use it. This is what stops the junk drawer. |
| 2. Plan | Can we build it from vetted parts? The small local model wires them together. If it needs genuinely new code, escalate to a bigger model. |
| 3. Write the permissions | Before any code: exactly which paths, devices, syscalls and network it needs. Anything not listed is blocked. |
| 4. Prove it | The model writes a test that proves the capability works. A backup must restore a file byte-for-byte. If the test fails, nothing gets installed. |
| 5. Declare it | Build the `.deb`, install it, register it. Keep the test and re-run it on every upgrade. |

Step 4 is the important one. It's the difference between "the OS built itself a backup"
being reassuring or alarming.

### Parts library

Vetted building blocks the small model composes: `snapshot`, `schedule`, `encrypt`,
`sync`, `watch-path`, `notify`, `http-fetch`, `usb-bulk`, `i2c-read`, `spi-xfer`,
`serial`, `framing`, `archive`, `verify-restore`.

The 8 GB floor runs a 3-4B model at int4, around 2-2.5 GB resident. That's meaningfully
better than the 1.5B the old 4 GB floor allowed: better at structured output, tool-call
formatting and multi-step planning.

It still can't write a correct backup daemon, and neither can a 7B. That's why the parts
library stays the primary path. Note what changed though: at 4 GB the library was forced
on us, at 8 GB it's a choice. We're keeping it because composing tested parts is
verifiable and generating a critical daemon from scratch isn't, regardless of model size.
The library grows once, centrally, for everyone.

### Model tiers

| Tier | Size | Where | Job |
|---|---|---|---|
| initramfs | 1.5B int4, ~1 GB | boot partition, opt-in | Boot-failure triage before the rootfs is up |
| **orchestrator** | **3-4B int4, ~2-2.5 GB** | resident | **The floor. Everything the OS depends on must work here** |
| desktop | 7B+ int4 | loaded on demand | Harder planning, rung 5 driver generation |
| cloud | — | opt-in, off by default | Escalation when local can't |

At 8 GB the orchestrator stays resident and a 7B can still be loaded transiently, which
means the floor target can do its own escalation instead of always needing the builder
box. That's the main practical gain over 4 GB.

## Decisions

| # | Decision | Why |
|---|---|---|
| 1 | Kernel notices, userspace reasons | No floating point in ring 0, no forked kernel, keeps stock Ubuntu kernel updates and Secure Boot |
| 2 | 8 GB ARM64 board is the floor | If it doesn't work there it isn't in the design. Buys a 3-4B orchestrator and enough headroom to load a 7B transiently |
| 3 | One deb source, two image builders | amd64 ISO for desktops, arm64 `.img` for boards, one rootfs recipe |
| 4 | Autonomy as far as things can be undone, and set per profile | See decision 15 for the reversibility line, and 16 for why `server` defaults lower |
| 5 | Rust userspace, C for module and BPF | Shell integration pays interpreter startup per keypress (~250ms on a Pi vs ~2ms). CPython is 55 MB in the initramfs vs ~3 MB. Python also can't read BPF ringbufs without a C shim |
| 6 | Build from parts *and* escalate to a big model | Composing tested parts is verifiable; generating a critical daemon from scratch isn't, at any model size. Escalation covers what parts can't express |
| 7 | Generated code declares its permissions up front | Renders into systemd hardening plus seccomp. Reuses a mature sandbox instead of writing one |
| 8 | Prefer userspace drivers | You can kill a userspace driver. There's no sandbox for a kernel module |
| 9 | Nothing is kept until its test passes | "The backup ran" and "the backup can be restored" are different claims |
| 10 | Nothing audits itself | A broken component's report on itself is worthless. Each layer is checked by the one below it |
| 11 | Learn per-host baselines, not fixed thresholds | A limit that's right for a desktop is wrong for a fanless board in a hot cabinet. Gradual failures cross no threshold |
| 12 | Hash-chained audit plus a kernel cross-check | Makes tampering detectable with no external infrastructure |
| 13 | Learn in capabilities first, weights last | Capabilities can be inspected, reverted and deleted. Weights can't |
| 14 | Adapters train on a builder box, not on the board | 8 GB still can't train. Training data is structure, not file contents |
| 15 | Curated driver sources act alone; arbitrary repos don't | A backdoored driver that ran as the kernel isn't undone by uninstalling it |
| 16 | One machine profile sets autonomy, latency and model tier together | Three orthogonal knobs is a matrix nobody configures correctly |
| 17 | `server` defaults to propose-only | Ubuntu's biggest install base is multi-tenant production. The risky default shouldn't be the one that ships there |
| 18 | The builder is a role, not separate software | Keeps "one deb source" true. Any machine with the hardware can be one, and none is required |
| 19 | `realtime` nodes keep a resident model where the hardware can isolate it | Affinity alone doesn't stop bandwidth and cache contention, but AMP, MPAM/RDT and NPUs do. The node proves its own timing rather than trusting a spec sheet |
| 20 | Natural language lives in `command_not_found_handle`, interactive shells only | Real commands resolve first and are never touched. A typo in a script must still fail loudly |
| 21 | One inbox, three renderers | Terminal, push to existing tools, then web UI. Same data model. The web UI goes last because it is an authenticated network surface on a box running an autonomous root daemon |

## Machine profiles

All Ubuntu use cases are in scope: desktop, server, container host, cloud instance,
appliance, board. Role, latency class and autonomy level were accumulating as separate
settings, so they collapse into one declared profile with individual overrides.

| Profile | Autonomy | Latency | Resident model |
|---|---|---|---|
| `workstation` | full | interactive | orchestrator + 7B on demand |
| `appliance` | full | standard | orchestrator |
| `realtime` | full, capped to reflexes | realtime | **orchestrator, isolation-gated** |
| `server` | **propose-only by default** | standard | orchestrator |
| `builder` | full | standard | large model + trainer |

Profile is declared at image build or first boot, and it's a hard contract.

`realtime` was called `controller` in earlier drafts. That name collides with Kubernetes,
where a controller is a reconciliation loop in the control plane, and k8s nodes are a large
part of Ubuntu's server base.

`server` defaulting to propose-only matters. An autonomous root daemon restarting a
500-user database is a different proposition from an appliance in a cabinet. It does the
full analysis and writes the exact fix, then queues it. An operator raises the level per
host.

Two consequences:

- **Containers get no autonomy, correctly.** No `/dev/omnia`, no BPF, no guard — and the
  rule is no floor, no autonomy. Containerised Omnia is capability-building only.
- **Ephemeral nodes propagate capabilities upward.** A cloud instance that builds
  something and then terminates has wasted the work, so results push to the builder's
  registry rather than staying local.

### A realtime node keeps its own model

An earlier draft said RT nodes run no model at all. That was wrong. CPU affinity alone
does not stop a resident model disturbing a control loop, because inference saturates
memory bandwidth and evicts the last-level cache, both shared across cores. But affinity
is not the only tool available.

| Isolation available | How the loop is protected | Resident model |
|---|---|---|
| **AMP** — application cores plus a Cortex-M/R with TCM (i.MX8/93, TI AM62x/AM64x, STM32MP, Zynq UltraScale+) | Loop runs on the M/R core out of tightly-coupled memory and never touches DRAM. Structurally immune to what the A cores do | full orchestrator |
| **Hardware partitioning** — ARM MPAM (v8.4+) or Intel RDT/CAT/MBA | LLC ways and memory bandwidth partitioned in hardware between core groups | orchestrator, bandwidth-capped |
| **NPU** — Hailo, RKNN, NVDLA | Inference leaves the CPU cores entirely | orchestrator on the NPU |
| **None of the above** | `SCHED_DEADLINE` budget, inference only in slack windows between cycles | micro tier only, weaker guarantee |

Inference still runs in userspace per decision 1. "Kernel-resident" here means resident on
the node and wired into the kernel's event path, not executing in ring 0.

**The gate is measured, never assumed.** Per decision 9 the `realtime` profile carries its
own retained test: a cyclictest-style latency histogram run *with the model under load*.
Exceeding the declared p99.9 budget drops the node to the next isolation tier, and to
micro-only or no model if it must. Re-run after every kernel and model change.

Two gains over the old rule. RT nodes get local reasoning instead of a round trip to a
peer, and the timing claim becomes something the machine demonstrates rather than something
the design asserts — so an untested board is handled correctly by default.

### Latency classes

Fast response and reasoning are different paths, and the model isn't in the fast one.
Capabilities already are the reflex layer: the forge emits a `.deb` with a systemd unit
that runs at machine speed. The model reasons once, ahead of time, and emits something
that runs without it. "Thermal event, throttle in 50 ms" is not a model call; it's a
reflex the model wrote last week.

| Class | Needs | Reflex coverage | Reasoning |
|---|---|---|---|
| realtime | sub-10ms guaranteed | 100% of the hot path | local when isolation allows, else remote |
| responsive | sub-second | common cases | local seconds, or remote |
| standard | seconds | thermal, power, link | local |
| interactive | fast enough for a human | few needed | local + escalation |

Reflexes become a first-class capability kind. A capability declares itself a reflex
(bounded latency, no model, no network, no allocation in the hot path) or a deliberation.
Per decision 9, a reflex's generated test must include a **worst-case latency assertion
measured under load** before it can be installed.

CPU isolation is enforced in the kernel floor: the guard denies the daemon
`sched_setaffinity` onto isolated cores, so the model can't steal CPU from a control loop.
Same principle as the rest of the floor.

### GPU: never required, sometimes wanted

Token generation is memory-bandwidth-bound (Pi 5 roughly 5 tok/s at 3-4B q4, Orin Nano
roughly 25). But this workload is long-context and short-output — a few thousand tokens of
journal and device state in, a short plan out — so it's **prefill**-dominated, which is
compute-bound, which is what GPUs and NPUs actually accelerate.

The no-GPU mitigation is **KV cache reuse**, and it's the highest-leverage optimisation in
the design. Most system context is static: OS release, hardware, installed packages,
baselines. Cache that prefix, prefill only what changed, and a 3k-token prefill becomes a
few hundred. Must be in the model layer from the start.

## How users interact

The thesis says you never need to know a command, but `omni ask` is a command. The honest
version is narrower: you never need to know the four hundred commands Linux otherwise
demands — `tar`, `systemctl`, `iptables`, `lvm`, `rsync`, `dd` — or their flags. `omni` is
an escape hatch, an inspection tool, and something scripts call. It is not the front door.

Two halves. Most designs build only the first. The second matters more here, because the
system acts on its own: an OS that changes your machine at 03:00 with no good way to say
what it changed is a liability.

### You to the OS

| Where | How |
|---|---|
| A shell | Type it. Unrecognised input becomes intent |
| Desktop | Super+Space |
| File manager | Right-click, describe the outcome |
| SSH | Identical shell behaviour |
| A script | `omni` with arguments, JSON out |

**Real commands are never affected.** Intent lives in `command_not_found_handle`, the last
thing the shell tries:

| Order | Shell tries | `ls -al` |
|---|---|---|
| 1 | Syntax, pipes, redirects, `if`/`for` | — |
| 2 | Aliases | — |
| 3 | Shell functions | — |
| 4 | Builtins | — |
| 5 | `$PATH` lookup | **resolves, runs** |
| 6 | `command_not_found_handle` — intent | never reached |

So `ls -al` runs with zero added latency and no model involvement, as do aliases, dotfiles
and every existing workflow. `ls -zzz` also resolves; `ls` prints its own error, which is
correct.

The one real edge case is a mistyped command *name* (`sl` for `ls`). Three rules:

- A single token within edit distance 1-2 of a real command gets "did you mean `ls`?",
  never an intent.
- Intent must look like language: several words, no leading dash, not a bare path.
- Anything ambiguous prints the normal "command not found" and offers rather than acts.

**Non-interactive shells never do this.** A build script that silently invoked a model
instead of failing on a typo would be a disaster.

### The OS to you

All three surfaces are required. They share one queue of events, actions taken and pending
proposals, which is what makes three surfaces affordable rather than three products.

```
$ omni
3 things happened · 1 needs you

  did     reclaimed 4.2 GB from journald and the apt cache      2h ago
  did     restarted nginx after it failed its own health check  5h ago
  built   backup-pictures 1.0.1 — rebuilt for kernel 6.14       1d ago

  needs you
  → vendor driver for 0bda:8153 found. built, tested, passes.
      omni show 4      omni approve 4
```

1. **Inbox** — `omni` with no arguments, as above.
2. **Push to existing tools** — the same queue to syslog, Prometheus, email, Slack and
   webhooks, approvals returning the same way. This is what makes `server` propose-only
   usable at scale; nobody will SSH into 400 hosts to read an inbox.
3. **Local web UI** — batch approval and, later, the fleet view.

Built in that order. The inbox is the data model and the other two render it. The web UI is
last on purpose: an authenticated network surface on a machine running an autonomous root
daemon is the highest-risk component in the design, and it should not exist before the
queue it renders is stable.

A third requirement falls out of the capability model: **users must be able to see what the
machine can now do.** `omni capabilities` lists what it has taught itself, with provenance
for each. Needed as soon as the count passes about five.

## The builder

Not separate software. An Omnia machine running the `builder` profile — same image, same
source, one flag. Three jobs, each impossible on a small node:

| Job | Why not on a node |
|---|---|
| Escalation target running a 7B-30B+ | No headroom for a big model plus the node's own work |
| Building and signing `.deb`s for other arches and kernels | Cross-building and key custody want a real machine |
| Training LoRA adapters from verified examples | Needs 16-24 GB VRAM |

**It is not required.** Without one: nodes compose from parts entirely offline, nodes with
headroom escalate to their own transient 7B, nodes with neither queue or use cloud if
enabled. The only loss is adapter training. A single laptop is a complete system.

Found by mDNS (`_omnia-builder._tcp`) or explicit config, but discovery is only a hint.
Trust comes from a pinned signing key, never from who answered the broadcast.

**It is a supplier, not a controller.** It produces packages every node installs, making it
the highest-value target in a fleet, so nodes stay sovereign:

- It signs; nodes verify against a pinned key.
- Nodes still run each capability's own test before keeping it. The builder does not get
  to skip step 4. A compromised builder can ship bad code; it cannot ship code that passes
  a test it does not control on a machine it does not own.
- The declared permission manifest and sandbox are unchanged.
- It never holds root on a node. It hands over packages; nodes decide.

Data flowing to it follows the existing rule — structure, not content: intent, plan, parts
used, test result, redacted corrections, and device descriptors for build requests. Not
user files.

Ships as `omnia-role-builder`, pulling in the large model, training toolchain, apt repo
server and signing setup. No new crates: the same `omnia-forge` and `omnia-learn`,
configured to accept work from peers.

## The driver ladder

When a device appears with no driver, work down this list. Cheapest and safest first.

| # | Situation | What happens |
|---|---|---|
| 1 | Driver is in the kernel, just not loaded | `modprobe`, check it binds |
| 2 | Needs a firmware blob | Fetch from `linux-firmware`, reload |
| 3 | Needs a quirk, ID, udev rule, or device-tree overlay | Generate config. No code |
| 4 | Driver source exists somewhere | Fetch, build against this kernel, DKMS, load, verify |
| 5 | USB/I2C/SPI/serial with no driver anywhere | Write a sandboxed userspace driver |
| 6 | Novel device needing kernel code | Scaffold and build, load only with rollback armed |

Rows 1 and 2 are two filesystem lookups and don't wake the model at all. Most devices end
there.

Row 5 is where the OS actually writes a driver, and it's sandboxable because it's
userspace. Good fit for boards, where an undocumented I2C sensor is a real and common
case.

Row 6's real blocker isn't writing code. A driver is essentially the chip's register map,
and nothing can infer that from a USB ID. With a datasheet it's doable. Without one,
nothing can do it.

### Driver source trust (rung 4)

Rung 4 compiles third-party code into your kernel. It's the most useful rung and the only
place the OS extends trust outward.

| Sources | Acts alone? |
|---|---|
| Ubuntu archive, `linux-firmware`, archive DKMS packages | Yes, archive-signed |
| A DKMS index you vet and sign | Yes, your key |
| Arbitrary vendor or community repos | No |

For the third case the OS does everything except decide. It finds the candidate, reads
the source, builds it in a sandbox, generates the test, and writes up what the code does
and what privileges it wants. Then it waits. You answer yes or no to a finished analysis,
not a research project.

Two things make this cheaper than it sounds:

- When rung 4 is blocked, rung 5 usually still works, and a sandboxed userspace driver is
  the better outcome anyway.
- You approve a driver once, not once per machine. It joins your signed index and the
  fleet picks it up automatically.

## Keeping itself working

The rule: **nothing audits itself.** Capabilities are checked by the daemon. The daemon is
checked by the kernel. The kernel is checked from off-box.

### Diagnosing

| Level | Question | How |
|---|---|---|
| Liveness | Is it running? | systemd, plus the kernel watchdog on the executor |
| Correctness | Is it doing the right thing? | The retained capability tests |
| Baseline | Is this normal for this machine? | Learned per-host profile |

The middle one comes free. Every capability keeps the test that proved it, so the machine
ends up with a working definition of "healthy" that grows as it gains capabilities.
`omni doctor` runs everything the machine claims it can do.

Normal monitoring can tell you the backup process exited 0. It can't tell you the backup
can be restored.

Baselines tracked: temperature curve, disk growth rate, boot time, memory ceilings,
restart frequency. Takes about a week to learn, and says so rather than guessing before
then.

### Healing

Verification at every tier is the capability's own retained test, not the model's opinion
that it fixed things.

| Tier | Action | How to undo |
|---|---|---|
| 0 | Restart or reload the unit | trivial |
| 1 | Reconfigure | undo journal |
| 2 | Repair or reinstall the package | `apt` |
| 3 | Rebuild the capability | previous `.deb` |
| 4 | Roll back to last known-good | boot sentinel |
| 5 | Stop, degrade safely, report loudly | — |

Tier 3 is the payoff. A kernel upgrade breaks a generated USB driver. Its test fails on
next boot. The OS rebuilds the driver against the new kernel and re-runs the test. If it
passes, nobody gets paged. If it fails, tier 4 rolls the kernel back and then reports.

This only works because the machine knows what its capabilities are supposed to do.

### Auditing

A root daemon writing its own log can lie or leave things out. If `omniad` is broken or
compromised, its account of itself is worth nothing. So:

1. **Hash-chained records.** Each entry commits to the previous one, so edits and
   deletions show up. Cheap, always on.
2. **Kernel cross-check.** The module counts executor actions, and the guard counts
   denials, in maps the daemon can't write. A doctored log shows up as a mismatch.

Records include the reasoning, not just the action: inputs, the plan, what it rejected,
and the test result. "Why did it restart postgres at 3am" needs a better answer than "it
restarted postgres at 3am."

**Limit worth stating plainly:** a fully compromised machine with nothing outside it can't
audit itself. Off-box anchoring and fleet cross-checking are the only real answers, and
both need something beyond the one box. Deferred, not rejected.

## Learning

Ordered by how inspectable each option is. Weights come last.

| # | Mechanism | Can you read it? | Can you undo it? | Can you copy it? |
|---|---|---|---|---|
| 1 | Capabilities | Yes, it's a `.deb` | `apt remove` | Copy the file |
| 2 | Baselines | Yes, numbers | Delete the row | Per-host anyway |
| 3 | Knowledge base (incidents, what worked) | Yes, editable text | Delete the entry | Selectively |
| 4 | Learned routing and examples | Yes, text | Revert | Yes |
| 5 | LoRA adapter | No | Swap the file | Yes |
| 6 | Full fine-tune | No | No | No |

Levels 1 to 4 give most of what "it's learning" feels like, with nothing hidden.

### Why adapters are still worth it

The retained tests produce labelled training data as a side effect. Every build attempt is
`(request → plan chosen → test passed or failed)`, with a real label. Most on-device
learning has no ground truth and ends up training on "the user didn't complain," which is
noise.

Scope stays narrow: picking which parts to compose, tool-call formatting, and local
vocabulary like your device names. Not general reasoning.

### The three risks

| Risk | What we do about it |
|---|---|
| Forgetting. A 3-4B model still has limited headroom and is more fragile to fine-tuning than a large one | The eval suite includes a frozen general-capability set. An adapter that regresses it is thrown away |
| Collapse from training on its own output | Only examples whose test actually passed are eligible |
| Can't train on the floor target | LoRA on a 3-4B needs 12-20 GB. 8 GB minus the OS and a resident model doesn't come close. Training happens on the builder box |

### How an adapter ships

An adapter is a capability, so it goes through the same pipeline.

```
collect verified examples (plus redacted user corrections)
  → train on the builder box
  → must beat the current adapter on a frozen eval suite
  → shadow run: compare decisions against the current one
  → ship as a signed .deb; apt remove reverts it
```

Same builder box that handles novel code generation. One capable machine serves the
fleet.

### What can become training data

Allowed: the request, the plan, which parts were used, the test result, and your explicit
corrections after redaction.

Not allowed: file contents, command output, anything the context providers read.

So it learns that requests shaped a certain way are served by `snapshot + schedule +
encrypt`. It never learns what's in your photos.

The reason for the line: levels 1–4 let you delete one specific thing. A trained adapter
doesn't. There's no "forget that one file."

## Safety

Three layers, none of which ask you anything:

1. **Kernel floor.** BPF-LSM programs pinned before `omniad` starts, in a cgroup it can't
   edit. Blocks: writing the boot device, deleting the undo journal, modifying the guard,
   unmounting root, and touching the paths that keep you able to log in. `omniad` refuses
   to run autonomously unless the guard reports active.
2. **Capability sandbox.** The permission manifest becomes systemd hardening plus seccomp.
3. **Reversibility.** Every action records its undo first. Generated capabilities are
   packages. If a boot after an autonomous change fails twice, the initramfs replays the
   undo journal.

Plus an action budget with a circuit breaker. If it keeps failing to fix the same thing,
it drops to watching instead of looping all night.

## Where the code lives

```
distro/
├── kernel/          C. Written.
│   ├── omnia-kmod/  char device, event ring, executor claim, watchdog
│   └── bpf/         probes + the BPF-LSM floor + loader
├── runtime/         Rust workspace, 15 crates. Stubs.
│   └── crates/      abi, core, http, kernel, model, parts, forge, registry,
│                    sandbox, autonomy, audit, learn, omni, omniad, omnia-modeld
├── images/          Not started. amd64 ISO + arm64 img.
└── docs/            This file.
```

No libbpf anywhere. The ringbuf is read by mmapping the map fd directly, which is stable
kernel ABI. So the image ships no libbpf and the build needs no libelf.

## Build order

The original plan was seven horizontal phases: foundation, then model layer, then the
forge, and so on. That's probably wrong. It means building three layers before finding
out whether the core idea works.

Better: a thin vertical slice. The minimum of every layer needed to make one capability
work end to end.

Boards that clear the 8 GB floor: Pi 5 8GB, Orange Pi 5 8/16GB, Radxa Rock 5B, Jetson
Orin Nano 8GB. It rules out Pi 4, Pi Zero, Jetson Nano and most cheap industrial boards.
That's a real narrowing of reach and worth revisiting if a deployment needs those.

- enough `omnia-core` to load config
- enough `omnia-model` to talk to llama.cpp
- three parts: `snapshot`, `schedule`, `verify-restore`
- the full five-step pipeline
- the cold-capability test

That's the riskiest part of the system. If it works, everything else is a variation on a
pipeline that already runs. If it doesn't, we find out in a week.

## Notes on the kernel code

- `omnia_abi.h` has no padding by design, so Rust can assert struct sizes at compile time
  and re-parse the header in a test. A DKMS module built from one version talking to a
  package from another otherwise shows up as garbled events, not an error.
- The event ring drops the **oldest** record when full. During an incident the newest
  events are the ones that explain it. Sequence numbers skip so the gap is visible.
- One process holds the executor claim, and the kernel raises an event if it stops
  heartbeating. A stuck daemon otherwise looks exactly like a healthy idle one.
- `OMNIA_IOC_EMIT` refuses `GUARD_DENY`. That's the event proving the floor works, so
  userspace must not be able to fake it.

## Still open

1. Which 3-4B and 1.5B GGUF builds to pin (orchestrator and initramfs tiers). Also whether
   their licences allow shipping them in an image and redistributing a LoRA adapter
   trained on them.
2. Whether escalation defaults to a big local model, a cloud API, or a builder box on the
   LAN when more than one is available. 8 GB makes local escalation to a 7B genuinely
   viable on the floor target, which it wasn't at 4 GB.
3. How much of the parts library ships in v1. This decides whether a board can build
   anything useful offline.
4. What's in the frozen general-capability eval set, and who owns it. It's the only thing
   between incremental learning and a model that quietly forgot its job.
5. When off-box audit anchoring becomes necessary. It does the moment this runs on
   someone else's hardware.

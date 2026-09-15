# Velnor refactoring goal

You are leading a long-running, multi-agent rearchitecture of **Velnor**.

You are not here to make a small improvement, clean up a few bugs, or finish the previous Mr. Boxington experiment.

You are here to reassess Velnor from first principles and transform it into the fastest practical, most predictable, most stable production-grade self-hosted GitHub Actions runner we can build, with **Rust-heavy CI on the default Docker backend as the first and most important workload**.

This is a research project. Breaking changes are explicitly allowed. Existing architecture is evidence of prior thinking, not a constraint. Anything may be deleted, redesigned, decomposed, consolidated, or replaced when a better architecture exists.

The objective is not the smallest diff.

The objective is the best Velnor.

## 1. Exact repositories and branches

Work against these exact branch lines.

Primary implementation repository:

* repository: `tailrocks/velnor`
* branch: `perf/docker-rust-mbx`
* inspected starting head when this goal was written:
  `2858e92df0eb78df4f1a6fe2ad4cbf86f1d56355`

Required verifier/integration repository:

* repository: `tailrocks/velnor-actions-fixture`
* branch: `codex/verifier-completion-fixes`
* inspected starting head when this goal was written:
  `5c8b57aa64dcbfd8fe6b2f6edae625ae344fc496`

At the beginning:

1. resolve the current head of each exact branch;
2. record both immutable SHAs in the working plan;
3. inspect all applicable `AGENTS.md` files before doing anything;
4. never silently substitute `main`, another branch, or an old release;
5. if either branch has advanced since the SHAs above, use the newer head of the **same requested branch** and record that fact.

Read other branches and external repositories only as references unless implementation explicitly requires otherwise.

The two repositories are one system for purposes of this goal:

* Velnor is the runner under test.
* `velnor-actions-fixture` is the differential/conformance/readiness/performance verifier.

A change to Velnor that requires verifier changes is incomplete until both sides agree.

## 2. Mission

Build Velnor into a runner where a user can reasonably ask:

> Why didn't my job start immediately?
>
> Why did the runner sit there doing nothing?
>
> Why did this workflow work on GitHub-hosted but behave differently on Velnor?
>
> Why was the second Rust build still slow?
>
> Why did two concurrent builds fight each other?
>
> Why did a runner restart lose the result?
>
> Why is cleanup holding the next job?
>
> Why is disk usage growing without bound?
>
> Why did this cache unexpectedly miss?
>
> Why did this step behave differently from actions/runner?

and the architectural goal is that these questions should almost never arise.

For every state of an accepted workload, Velnor should have a deterministic explanation and a bounded transition to the next state.

The design should feel boringly predictable under normal operation, concurrency, failure, cancellation, restart, resource pressure, and long-running use.

## 3. Product priority

The product priority is:

1. extreme performance;
2. extreme stability and predictability;
3. GitHub Actions semantic correctness for every capability we claim;
4. isolation and security;
5. architectural simplicity;
6. maintainability and comprehensibility;
7. latest stable Rust and modern APIs;
8. efficient resource and disk utilization.

These are not independent goals.

Performance obtained by weakening correctness or isolation is invalid.

Stability obtained by serializing everything unnecessarily is invalid.

Compatibility obtained through layers of patches and special cases is invalid if a cleaner semantic model can represent the behavior.

Architectural cleanliness that makes the hot path slower without a real capability reason is invalid.

Among designs that are correct, choose the fastest and simplest design supported by evidence.

## 4. How to decide what should be fixed

Judge every piece of work by whether it **should** be done.

Ask:

* Is the current behavior correct?
* Is the architecture internally coherent?
* Does the behavior match the capability Velnor claims?
* Does it serve the goal?
* Is there a faster correct architecture?
* Is there a simpler correct architecture?
* Does the current structure permit entire classes of bugs?
* Does it unnecessarily serialize work?
* Does it create unbounded or unpredictable behavior?

Never decide based on ROI, engineering effort, diff size, schedule, or whether something is "worth it."

Do not label a known-wrong condition:

* low value;
* marginal;
* rare;
* edge case;
* expensive to fix;
* too invasive;
* not worth the refactor.

Those are not valid reasons to preserve incorrect architecture.

"The upstream runner also has an issue" does not make Velnor's issue acceptable.

"A competitor also gets this wrong" does not make it acceptable.

"Nobody will probably hit this" does not make it acceptable.

A known architectural defect remains a defect until it is fixed or until a concrete capability/technical limitation proves that the correct solution cannot be implemented.

Hard is not impossible.

Large is not impossible.

Breaking is not impossible.

Expensive is not impossible.

If feasibility is uncertain, investigate, prototype, measure, and prove it.

## 5. Bug-fixing rule: eliminate bug classes

Treat every bug as architectural evidence.

Before fixing any bug, explicitly ask:

1. What observable behavior is wrong?
2. What invariant should have prevented it?
3. Why was that invariant not represented or enforced?
4. What ownership/state/API boundary permitted this bug?
5. Is this one instance of a broader bug class?
6. What other paths can fail for the same structural reason?
7. Can we redesign the boundary so the invalid state becomes impossible or substantially harder to construct?
8. What regression/conformance test proves the entire bug class?

Prefer structural fixes over symptom patches.

A guard, retry, special case, timeout increase, exception, boolean, or extra `if` is not automatically a root-cause fix.

Use a symptom-level patch only if the root architectural correction is demonstrably infeasible or genuinely belongs to a separately tracked architectural change.

Do not preserve an enabling architecture merely because restructuring it is more work.

## 6. Mandatory agent/model routing

This is a multi-agent goal.

Use subagents aggressively.

Do not attempt to perform the entire investigation or implementation in one context.

## 6.1 Opus 5 High owns analysis and architecture

All substantial investigation, architectural reasoning, protocol research, benchmarking design, root-cause analysis, and planning must be delegated to **Opus 5 with High reasoning**.

Use several independent Opus 5 High agents in parallel.

Do not ask one Opus agent to investigate everything.

At minimum begin with independent Opus 5 High investigations for:

1. runner process/slot lifecycle;
2. GitHub V2 broker/run-service protocol;
3. GitHub workflow semantic model;
4. expressions, conditions, step outcomes and conclusions;
5. cancellation and timeout semantics;
6. completion, durability and crash recovery;
7. Docker runtime architecture;
8. Docker Engine API versus Docker CLI;
9. Docker lease/proxy/resource ownership;
10. BuildKit/buildx lifecycle;
11. Rust compilation architecture;
12. Mr. Boxington;
13. Cargo dependency/source persistence;
14. filesystem/worktree/checkout performance;
15. resource scheduling and concurrency;
16. cache architecture and trust;
17. disk ownership and garbage collection;
18. action resolution and JavaScript/Docker/composite execution;
19. artifacts/cache/results/timeline/log publishing;
20. async/threading/blocking-I/O architecture;
21. crate/module/API organization;
22. dependency/API freshness;
23. security and cross-job isolation;
24. observability/performance instrumentation;
25. `velnor-actions-fixture` coverage architecture;
26. benchmark methodology;
27. fault injection and soak testing;
28. production operability.

Run investigations concurrently whenever independent.

Require each agent to cite exact files/functions/types and compare claims with actual source.

Do not accept vague recommendations.

## 6.2 Independent Opus synthesis

After the initial investigations, assign at least one fresh Opus 5 High agent to act as an **architecture synthesizer**.

It must read the independent reports, inspect relevant source itself, challenge assumptions, detect contradictions, and create one coherent target architecture.

Use another independent Opus 5 High agent as a **red-team architecture reviewer** before implementation begins.

The reviewer must look for:

* hidden serialization;
* invalid state transitions;
* cache poisoning possibilities;
* lost completion scenarios;
* unbounded work;
* resource leaks;
* unsupported GitHub semantics;
* unnecessary abstraction;
* accidental performance regressions;
* architecture that exists only to preserve historical design.

The coordinator resolves disagreements from evidence.

Do not use majority vote.

## 6.3 Sonnet owns implementation

Once the architecture and detailed plan for a work package have passed Opus review, delegate implementation to **Sonnet** agents.

Sonnet agents should receive bounded, explicit tasks containing:

* architectural context;
* exact invariant;
* target files/modules;
* behavior to preserve;
* behavior to remove;
* tests to add;
* benchmark to run;
* dependencies on other tasks;
* forbidden compatibility shims;
* completion criteria.

Sonnet should implement, test, format, lint, and produce a concise implementation report.

Sonnet should not independently redesign major architecture when the implementation exposes a new architectural question.

Escalate such decisions back to Opus 5 High.

## 6.4 Opus reviews every substantial implementation

Every substantial Sonnet implementation batch must receive an independent **Opus 5 High review**.

The reviewer should inspect code, not merely read the Sonnet report.

Review for:

* architectural consistency;
* root-cause correctness;
* Rust quality;
* safety;
* concurrency;
* state transitions;
* lifecycle ownership;
* GitHub compatibility;
* performance;
* allocations/copies/process spawning;
* blocking I/O;
* error taxonomy;
* cleanup;
* cancellation;
* tests;
* benchmark validity.

When problems are found:

1. update the master plan;
2. write a precise correction task;
3. delegate the correction to Sonnet;
4. send the result back to Opus;
5. repeat.

Do not accept a work package until Opus has no unresolved correctness or architectural finding.

## 6.5 Final independent verification

At the end use fresh Opus 5 High agents for at least:

* GitHub Actions semantic parity review;
* Rust/Docker performance review;
* concurrency/reliability review;
* security/trust review;
* architecture/code-quality review;
* verifier coverage review;
* final red-team review.

Do not allow an implementation agent to be its own final verifier.

## 7. Parallel implementation model

Represent the work as a dependency graph.

Parallelize independent branches.

Use isolated worktrees/branches for implementation agents when appropriate so independent agents do not overwrite each other.

Do not parallelize writes to the same architectural boundary merely for the appearance of concurrency.

Good parallel work:

* Docker client architecture and expression-semantic tests;
* benchmark tooling and dependency audit;
* fixture refactoring and Velnor module extraction;
* documentation research and fault-injection harnesses.

Bad parallel work:

* multiple agents independently redesigning the same state machine;
* several agents touching the same 1 MB executor module with incompatible assumptions;
* concurrent migrations where one depends on types being removed by another.

The coordinator owns integration.

## 8. Write the plan before broad implementation

Before broad implementation, create a living master plan in the Velnor repository:

`VELNOR_REARCHITECTURE_PLAN.md`

Also create/update an implementation plan in the verifier when needed:

`VELNOR_VERIFIER_PLAN.md`

These are temporary engineering source-of-truth documents for this goal.

The Velnor master plan must include:

* exact starting SHAs;
* current architecture;
* target architecture;
* architectural invariants;
* discovered bug classes;
* observed bottlenecks;
* benchmark baseline;
* dependency freshness findings;
* decisions and rejected alternatives;
* task dependency graph;
* P0/P1/P2/P3 classification;
* agent ownership;
* required tests;
* required benchmarks;
* status;
* implementation commits;
* review findings;
* remaining blockers.

Every task needs:

* ID;
* problem;
* root cause;
* desired invariant;
* proposed architecture;
* affected modules;
* dependencies;
* test proof;
* benchmark proof where performance relevant;
* completion condition.

Continuously update the plan as new evidence appears.

Do not treat the initial plan as immutable.

When implementation reveals that the plan was wrong, change the plan.

## 9. Important known starting observations

The following are **starting hypotheses/evidence**, not conclusions.

Revalidate them against the current requested branch heads.

## 9.1 The current branch is already an mbx experiment

The Velnor branch already contains a substantial previous Mr. Boxington optimization/refactoring pass.

Do not waste time implementing the old proposal again.

Treat the current mbx implementation as one candidate architecture to audit from first principles.

Ask whether:

* the integration is correctly placed;
* it is latest;
* its stores are correctly scoped;
* its scheduler is used optimally;
* it overlaps with other caches;
* image integration is optimal;
* its GC ownership is coherent;
* explicit sccache compatibility remains justified;
* current Velnor abstractions exist solely because of the previous sccache/Kache architecture.

Delete remnants that no longer belong.

## 9.2 Dependency freshness has already moved

At goal-writing time:

* the branch pins Rust `1.98.0`;
* Rust `1.98.1` already exists and fixes a compiler miscompilation;
* the branch pins Mr. Boxington `1.6.0`;
* Mr. Boxington `1.7.0` already exists and includes both performance and correctness work.

These versions may themselves be stale when this goal executes.

Therefore the first dependency pass must discover the **actual current latest stable versions**.

Never hardcode the observations above as permanent truth.

## 9.3 Runner implementation is too concentrated

At the inspected branch state, examples include roughly:

* `executor.rs`: ~1 MB;
* `runner.rs`: ~696 KB;
* `protocol.rs`: ~316 KB;
* `docker_lease.rs`: ~173 KB.

File size alone is not the problem.

The problem to investigate is whether these modules contain too many ownership domains and state machines, making invalid interactions easy to create.

Do not perform cosmetic file splitting.

Reorganize around coherent responsibilities and invariants.

## 9.4 Docker remains subprocess-heavy

The current runtime performs substantial Docker work through `std::process::Command` and the Docker CLI, including timeout/watchdog/stream handling.

Measure:

* number of Docker CLI invocations per job;
* exec/fork latency;
* parsing overhead;
* repeated inspections;
* repeated network/image/builder checks;
* thread creation;
* stdout/stderr buffering;
* cancellation overhead.

Compare it against a native Rust Docker Engine API implementation over the Unix socket.

Do not blindly rewrite the Docker path.

But do not preserve the CLI path merely because it already works.

Choose from evidence, including architectural benefits such as typed lifecycle ownership, cancellation, streaming, API negotiation and avoiding subprocesses.

## 9.5 Admission has potential serialization boundaries

The current runner includes single-permit boundaries around action admission and operational/SQLite admission.

Investigate whether these permit choices can cause one acquired job to fail or stall because another job occupies an internal worker.

Do not simply increase semaphore counts.

Determine:

* why serialization exists;
* what operation actually requires serialization;
* whether networking/read-only work is unnecessarily inside it;
* whether SQLite should use a dedicated actor/writer;
* whether preparation can be split into parallel read-only stages followed by a short durable transaction;
* how multiple slots behave under contention.

The goal is that internal Velnor architecture must not create mysterious job acquisition latency.

## 9.6 Current docs acknowledge incomplete cancellation parity

Treat this as a real compatibility gap.

Do not leave "Docker cancellation is different from upstream" or "MicroVM cancellation is narrower" as accepted final behavior if the corresponding capability is claimed.

Study upstream worker cancellation, graceful step termination, process trees, action post steps, container signals, escalation and completion semantics.

Implement a coherent cancellation model.

## 9.7 The current benchmark is incomplete

The current benchmark is useful for Cargo/worktree experiments, but it is not sufficient to prove runner performance.

Create a proper Velnor benchmark system that measures the complete path from runner readiness/job delivery through cleanup.

## 9.8 The fixture baseline is stale relative to this branch

The current verifier documents a Velnor release/manifest baseline older than this performance branch.

Re-derive capability coverage from the exact Velnor branch under test.

A verifier that certifies an old capability manifest cannot establish readiness of the new architecture.

## 9.9 The fixture's Rust suite currently selects sccache

The current Rust fixture sets `RUSTC_WRAPPER=sccache` and explicitly invokes `mozilla-actions/sccache-action`.

The current Velnor Docker design disables mbx when explicit sccache is used.

Therefore that suite cannot serve as the primary proof of Velnor's intended default mbx path.

Redesign the Rust verification matrix.

## 9.10 Existing fixture evidence already exposed step semantic bugs

Existing live fixture history has shown Velnor cases where `continue-on-error` / step `outcome` / `conclusion` behavior diverged from GitHub-hosted execution.

The exact performance branch may already contain a partial fix.

Do not assume either way.

Reproduce against the exact branch build and build comprehensive semantic coverage for the entire class.

## 10. Pure Rust policy

Velnor's own runtime functionality must be implemented in Rust.

Do not introduce TypeScript, JavaScript, Python, Go, Java, or shell-based business logic into Velnor.

Third-party GitHub JavaScript actions are different: Velnor must be capable of executing them because GitHub Actions compatibility requires it.

Node may exist in a job image for execution of third-party JavaScript actions.

That does **not** justify implementing Velnor itself in JavaScript.

For first-party support tooling:

* migrate substantial benchmark logic to Rust;
* migrate substantial verifier logic to Rust;
* migrate evidence normalization/comparison to Rust;
* migrate Docker probe logic to Rust;
* migrate capability/workflow audits to Rust where they are Velnor-specific logic.

YAML remains appropriate for GitHub workflow declarations.

Small shell snippets are acceptable when the fixture is explicitly testing shell/GitHub semantics or when they are thin command glue.

Do not leave Python as the core verifier architecture.

A long-running production-grade Rust runner should be tested by deterministic compiled tooling, not a growing pile of ad-hoc Python scripts.

## 11. Upstream GitHub runner is the semantic source of truth

For GitHub Actions behavior, `actions/runner` is the source of truth.

At the beginning:

1. resolve the latest stable `actions/runner` release;
2. record the release/tag and source commit;
3. inspect the relevant upstream source;
4. compare Velnor behavior with actual upstream implementation.

At goal-writing time the latest observed upstream release was `v2.337.0`, but re-check.

Do not infer behavior from documentation alone where source exists.

Do not guess.

Do not implement behavior because it "seems like GitHub."

For each Velnor capability, identify the corresponding upstream ownership/code path where feasible.

## 12. Build a GitHub semantic compatibility model

Do not think of compatibility as isolated adapter functions.

Design one coherent job semantic model.

Audit at least:

* job contexts;
* step contexts;
* expression evaluation;
* expression coercion;
* functions;
* status functions;
* `success()`;
* `failure()`;
* `cancelled()`;
* `always()`;
* `if`;
* skipped steps;
* failed steps;
* step outcome;
* step conclusion;
* `continue-on-error`;
* job-level `continue-on-error`;
* outputs;
* job outputs;
* environment mutation;
* `GITHUB_ENV`;
* `GITHUB_OUTPUT`;
* `GITHUB_PATH`;
* `GITHUB_STATE`;
* `GITHUB_STEP_SUMMARY`;
* workflow command escaping;
* masks;
* annotations;
* groups;
* command echoing;
* action state;
* pre/main/post actions;
* post-step ordering;
* composite actions;
* nested composites;
* local actions;
* JavaScript actions;
* Docker actions;
* services;
* job containers;
* default shell;
* explicit shells;
* working directories;
* timeout handling;
* cancellation;
* process exit codes;
* process signals;
* action cleanup;
* secrets;
* token exposure;
* permissions;
* OIDC;
* result publishing;
* timelines;
* logs;
* annotations;
* artifacts;
* cache restore/save;
* reusable workflow boundaries that Velnor claims to admit;
* architecture/OS contexts;
* matrix-derived expressions passed in the acquired job;
* environment URLs;
* post-job behavior.

Create a differential test whenever GitHub-hosted execution can be used as an oracle.

Do not claim generic GitHub Actions parity if only a subset exists.

Instead, maintain a machine-readable supported capability model and make unsupported inputs fail early and explicitly.

## 13. Turn lifecycle into explicit state machines

The current architecture should be examined for implicit boolean/state combinations.

Model major lifecycles explicitly.

At minimum investigate explicit state machines for:

## Runner slot

Possible conceptual states include:

* absent;
* registering;
* ready;
* polling;
* acquired;
* busy;
* draining;
* recycling;
* unhealthy;
* stopped.

Do not copy these names mechanically.

Design the states that make invalid transitions impossible.

## Job lifecycle

Represent the difference between:

* broker notification;
* job acquisition;
* local claim;
* durable admission;
* resource reservation;
* preparation;
* execution;
* cancellation requested;
* cancelling;
* publishing;
* durable completion staged;
* completion sent;
* completion acknowledged;
* teardown;
* terminal local cleanup.

The type/API architecture should prevent operations from occurring in invalid phases.

For example:

* credentials should not become executable before admission;
* execution should not begin without capacity;
* completion should not be forgotten because publishing failed;
* teardown ownership should not be ambiguous;
* a cancelled job should not accidentally run ordinary subsequent steps.

## Docker resource lifecycle

Explicitly own:

* job container;
* service containers;
* network;
* Docker action containers;
* BuildKit resources;
* testcontainer-created resources when Velnor claims them;
* socket proxy;
* temporary volumes;
* persistent scoped volumes/caches.

There must be one owner and one cleanup contract per resource.

## 14. Crash consistency and completion

Treat job completion as a distributed systems problem.

Analyze every crash point between:

1. acquire;
2. local claim;
3. durable admission;
4. execution;
5. terminal result creation;
6. publisher drain;
7. outbox persistence;
8. GitHub completion request;
9. GitHub acknowledgement;
10. local cleanup.

For each crash point specify what happens after restart.

Use durable state only where it changes correctness.

Do not turn everything into expensive persistence.

Make completion idempotent/replayable wherever the upstream API semantics permit.

Ensure stale runner processes/generations cannot publish as current owners.

Fault-inject process death at every meaningful lifecycle boundary.

## 15. Cancellation must become first-class

Cancellation is not "kill the container."

Research upstream cancellation deeply.

Design one cancellation abstraction that propagates through:

* broker cancellation message;
* current step;
* script process tree;
* JS action;
* Docker action;
* service/container lifecycle;
* BuildKit;
* child Docker operations;
* post actions where required;
* result publishing;
* job completion;
* teardown.

Support graceful termination where GitHub does.

Escalate after a bounded timeout.

Never leave children or Docker resources running after terminal completion.

Add fixture differential tests for:

* cancellation during shell;
* cancellation during Cargo build;
* cancellation during Docker build;
* cancellation during service use;
* cancellation during JS action;
* cancellation near step completion;
* cancellation during post action;
* cancellation during artifact/cache operations;
* repeated cancellation;
* daemon shutdown while cancellation occurs.

## 16. Docker is the highest-priority backend

Docker is the default packaged backend and the primary optimization target.

Firecracker can remain supported, but its abstraction requirements must not force Docker through a slower architecture.

Shared interfaces should exist only where the backends genuinely share semantics.

Do not make the fast path pay for theoretical backend symmetry.

Profile the complete Docker job lifecycle.

Measure separately:

* job planning;
* Docker readiness check;
* image resolution;
* image inspection;
* image pull;
* workspace mount preparation;
* cache mount preparation;
* network creation;
* service creation;
* service readiness;
* job container creation;
* job container start;
* attach/exec;
* first user command;
* Docker action creation;
* Docker action execution;
* BuildKit setup;
* testcontainers;
* teardown;
* network deletion;
* container deletion;
* BuildKit cleanup.

Produce command/API call traces for representative jobs.

## 17. Investigate a native Rust Docker Engine path

The current Docker CLI path is a major investigation target.

Compare:

A. current Docker CLI subprocess model;

B. a current maintained Rust Docker client;

C. a minimal direct Docker Engine API implementation over the Unix socket if existing clients introduce undesirable overhead/complexity.

Judge on:

* job startup latency;
* number of processes;
* allocations;
* streaming;
* API negotiation;
* cancellation;
* timeout handling;
* error typing;
* connection reuse;
* observability;
* correctness;
* maintenance;
* testability.

Use persistent HTTP/Unix-socket connections where safe.

Do not hardcode an old Docker API version if negotiation is available.

Do not introduce a giant dependency without inspecting its costs.

A native API migration is justified if it provides either:

* measurable hot-path improvement; or
* materially stronger lifecycle/cancellation correctness with no meaningful regression.

## 18. Remove repeated Docker discovery work

Search for repeated:

* `docker info`;
* `docker version`;
* image inspect;
* network inspect;
* Buildx inspect;
* builder selection;
* mount probes;
* filesystem probes;
* cgroup probes;
* daemon identity probes;
* API capability probes.

Separate:

* host facts;
* daemon-generation facts;
* image-generation facts;
* job-specific facts.

Cache immutable/stable facts for the correct lifetime.

Invalidate them when Docker daemon identity/generation changes.

Do not cache facts that can become incorrect without an invalidation mechanism.

## 19. Container creation and image architecture

Treat the job image as executable architecture.

Audit:

* Ubuntu base;
* package count;
* package necessity;
* layer structure;
* layer invalidation;
* image compressed size;
* unpacked size;
* pull time;
* cold create time;
* warm create time;
* architecture variants;
* toolchain installation;
* mise bootstrap;
* Node installation;
* Rust installation;
* mbx installation;
* optional sccache presence;
* duplicated binaries;
* startup shell work.

Do not install tooling per job when it can be baked reproducibly.

But do not make the default image enormous for rare compatibility tools if a faster lazy/optional mechanism is superior.

Benchmark image-size versus startup tradeoffs.

## 20. Mr. Boxington architecture

Re-research Mr. Boxington from current source.

Inspect:

* latest stable release;
* release notes since the Velnor pin;
* source architecture;
* Cargo shim/transparent mode;
* `mbx setup`;
* scheduler;
* CAS;
* managed targets;
* prediction/inheritance behavior;
* container guidance;
* Docker path handling;
* trust model;
* GC;
* server/remote modes;
* cache export/import;
* lockfile behavior;
* worktree behavior;
* toolchain identity;
* linker/build-script behavior;
* concurrent job behavior.

At goal-writing time Velnor was one release behind and the newer release contained both performance and correctness work.

Do not merely bump the version.

Understand whether the new implementation changes assumptions in Velnor.

## 21. Default Rust experience

The desired default experience is ordinary commands:

```bash
cargo check
cargo build
cargo test
cargo nextest run
cargo clippy
cargo doc
```

The user should not have to write `mbx ...` everywhere.

If transparent Cargo remains the best mechanism, make it robust.

Test:

* `cargo`;
* `cargo +stable`;
* pinned toolchains;
* workspace commands;
* aliases;
* installed subcommands;
* `cargo nextest`;
* clippy;
* rustdoc;
* build scripts;
* procedural macros;
* native `*-sys` crates;
* C/C++ compilation;
* multiple targets;
* feature changes;
* lockfile changes;
* worktree changes;
* source edits during/near a build.

Provide an explicit opt-out.

The opt-out should be understandable and deterministic.

## 22. Do not stack compiler caches accidentally

The default path must have one clear compiler/build acceleration architecture.

Do not layer:

* mbx;
* sccache;
* Kache;
* persistent `target`;
* `actions/cache target`;
* another Velnor CAS

without proving why each layer exists.

The previous compiler-cache service/Kache architecture has already been largely removed.

Finish the conceptual migration.

If explicit sccache support remains useful for compatibility, treat it as an alternative mode, not a simultaneous default.

Investigate whether sccache even needs to remain baked into the default job image.

If not, remove it from the default image and install/provide it only through the explicit compatibility path.

## 23. Persistent Cargo state

Exploit the fact that a Velnor node is persistent.

Optimize safe reuse of:

* Cargo registry index;
* crate downloads;
* Cargo git DB;
* git checkouts;
* relevant global cache state;
* mbx CAS;
* mbx managed targets;
* toolchain downloads;
* mise downloads.

Do not tar/un-tar host-local persistent data merely because GitHub-hosted runners have to do that.

Avoid copying bytes when bind mounting/reflinking/content-addressing is safe.

Partition state by the minimum isolation boundary required by correctness.

Do not unnecessarily partition caches so aggressively that reuse disappears.

But never permit untrusted cache poisoning.

Measure both reuse and isolation cost.

## 24. Trust and cache architecture

Model trust explicitly.

At minimum:

* trusted default branch;
* trusted same-repo branch;
* same-repo PR;
* fork PR;
* tag;
* release;
* workflow dispatch;
* distinct repository;
* distinct repository owner;
* toolchain;
* architecture;
* target triple.

Determine which cached artifacts may safely flow between each class.

A trusted release must not consume poisoned compiler state.

Fork PRs must not write state consumed by trusted builds unless the cache mechanism cryptographically/content-semantically guarantees safety.

Keep namespace decisions explicit in types/configuration.

## 25. Filesystem and checkout

Profile checkout independently from compilation.

Investigate:

* persistent bare mirrors;
* mirror locking;
* fetch negotiation;
* protocol v2;
* fetch depth;
* tags;
* partial clone;
* object reuse;
* LFS;
* worktrees;
* detached checkout;
* clean/reset;
* mtime normalization;
* file metadata churn;
* permissions;
* submodules;
* safe-directory setup;
* credential setup/cleanup.

The existing persistent mirror design may be good.

Prove it.

Measure:

* already-known commit;
* new commit;
* fresh repo;
* small update;
* large update;
* concurrent checkout of same repo.

No duplicate network fetch should occur without a correctness reason.

## 26. Rust fingerprint stability

The runner already contains behavior intended to stabilize mtimes/fingerprints between worktrees.

Audit this carefully against Cargo semantics.

Prove that it is correct for:

* commit checkout;
* generated files;
* build scripts;
* source edits;
* dirty worktree state;
* Git LFS;
* submodules;
* restored caches.

Never manipulate timestamps in a way that allows stale output.

Create tests specifically designed to detect false cache hits.

## 27. Resource scheduling

Velnor, Cargo, mbx, Docker and BuildKit must not independently believe they own the whole machine.

Build one coherent resource model.

Understand:

* Velnor slots;
* CPU count;
* cpuset;
* cgroup CPU quota;
* memory limits;
* IO pressure;
* Cargo `-j`;
* rustc parallelism;
* linker pressure;
* mbx scheduler;
* BuildKit;
* service containers;
* testcontainers;
* simultaneous unrelated jobs.

Optimize throughput and latency.

Benchmark at least:

* 1 Rust job;
* 2 concurrent Rust jobs;
* 4 concurrent Rust jobs;
* configured machine capacity;
* mixed Cargo + Docker build;
* Cargo + service-heavy job.

Avoid both oversubscription and artificial serialization.

## 28. BuildKit

BuildKit must be treated as persistent expensive infrastructure.

Audit:

* builder creation;
* builder inspection;
* selected driver;
* daemon lifetime;
* cache storage;
* cache GC;
* builder reuse;
* repository/trust isolation;
* cancellation;
* orphan cleanup;
* buildx wrappers;
* QEMU/binfmt initialization.

Do not create/destroy expensive BuildKit infrastructure for every job if safely reusable persistent builders can do better.

Do not share mutable BuildKit state across trust boundaries without a safety model.

## 29. Garbage collection and disk ownership

There should be one understandable host disk model.

Inventory every persistent byte class:

* Git mirrors;
* checkouts/worktrees;
* Cargo registry;
* Cargo git;
* mbx CAS;
* mbx managed targets;
* GHA cache;
* artifacts;
* Docker images;
* Docker layers;
* BuildKit;
* logs;
* journals;
* temporary files;
* toolchain caches;
* mise installs.

Identify the owner of each.

Do not run independent GCs that unknowingly fight each other.

Velnor should enforce global host capacity.

Subsystems such as mbx may manage their internal storage within an assigned budget if that remains the cleanest design.

Test disk exhaustion and near-exhaustion.

Job acquisition must not enter an indefinite "waiting for disk" state.

## 30. Async architecture

Audit all blocking operations on Tokio workers and all `spawn_blocking` usage.

Inspect:

* synchronous filesystem operations;
* synchronous HTTP;
* SQLite;
* subprocess waits;
* archive processing;
* hashing;
* directory walks;
* Git;
* Docker;
* compression;
* artifact work.

Do not mechanically convert everything to `tokio::fs`.

Choose the correct architecture.

Potential patterns include:

* async network client;
* bounded blocking pool;
* dedicated SQLite writer/actor;
* dedicated filesystem worker;
* Rayon for CPU-heavy parallel processing;
* streaming rather than whole-buffer operations.

Eliminate:

* accidental unbounded channels;
* unbounded spawn;
* thread-per-stream patterns on hot paths if avoidable;
* locks held across unrelated slow operations;
* process-global mutexes that serialize independent jobs.

## 31. Error taxonomy

Do not turn every failure into `anyhow` text at architectural boundaries.

Design typed errors where behavior depends on category.

Distinguish at least conceptually:

* permanent configuration;
* unsupported capability;
* invalid job;
* trust rejection;
* GitHub authentication;
* GitHub transient transport;
* GitHub protocol;
* Docker daemon unavailable;
* Docker operation transient;
* Docker operation terminal;
* capacity pressure;
* disk pressure;
* cache corruption;
* action failure;
* user step failure;
* cancellation;
* internal invariant violation.

Retry decisions must derive from category and idempotence, not string matching.

Every retry requires:

* reason;
* bounded policy;
* deadline;
* observability;
* idempotence analysis.

## 32. Timeouts

Inventory every timeout and retry constant.

For each, determine:

* operation;
* expected normal duration;
* ownership;
* cancellation behavior;
* whether it includes queue time;
* whether it can cause hidden serialization;
* what the operator sees.

Do not solve hangs by increasing timeouts.

A timeout is a safety boundary, not a performance architecture.

Expose stage information so an operator can always distinguish:

* no job delivered;
* delivered;
* acquisition;
* admission;
* capacity;
* checkout;
* image;
* container;
* action preparation;
* executing;
* publishing;
* completion;
* teardown.

## 33. No mysterious idleness

This is a first-class product requirement.

At any instant where a job exists but is not executing, Velnor should be able to explain why.

Instrument transitions with monotonic timings.

At minimum capture:

* runner registered;
* runner ready;
* broker poll started;
* broker message arrived;
* busy acknowledged;
* acquire started/completed;
* local claim;
* admission started/completed;
* capacity wait;
* checkout;
* cache prep;
* Docker prep;
* container create/start;
* first step start;
* each step;
* completion staging;
* publisher drain;
* completion request/ack;
* teardown.

Provide a concise operator-visible diagnostic timeline.

Do not require reading raw debug logs to answer "why didn't it start?"

## 34. Observability must be cheap

Keep instrumentation structured.

Measure overhead.

Avoid:

* synchronous logging on the hot path;
* huge string formatting;
* unbounded trace files;
* logging secrets;
* high-cardinality metrics that become unusable.

Use tracing spans around actual architectural boundaries rather than guessing from shell strings.

Do not use ad-hoc parsing of scripts such as looking for `cargo build` text as the authoritative source of compile-stage correctness.

Telemetry heuristics may exist, but correctness must not depend on them.

## 35. Production panic policy

Audit production code for:

* `unwrap`;
* `expect`;
* `panic!`;
* assertions;
* indexing;
* impossible-state comments.

Test-only uses are fine.

Production uses require a proven invariant.

Where an invariant is real, represent it structurally where possible.

Where external input can violate it, return a typed error.

Remove broad `#![allow(dead_code)]` and similar allowances when they are hiding stale architecture.

## 36. Dependency/API modernization

Perform a complete direct-dependency audit.

Use latest stable Rust.

Use Rust 2024 idioms.

Investigate current stable versions/APIs of:

* Tokio;
* reqwest;
* rustix;
* rusqlite;
* tracing;
* OpenTelemetry;
* Docker client ecosystem;
* serde stack;
* cryptographic/signing libraries;
* zip/tar libraries;
* Git-related options;
* all build/job-image tools.

Upgrade direct dependencies where current versions are stale unless there is a concrete technical incompatibility.

Do not preserve old APIs to reduce diff size.

Remove dependencies that only support removed architecture.

Inspect feature flags.

Avoid compiling blocking clients, crypto backends, protocols, or other heavy features that are no longer used.

Run supply-chain/security checks.

## 37. Benchmark architecture

Create a first-class Rust benchmark/measurement tool rather than relying primarily on shell + Python.

The benchmark system must be reproducible and machine-readable.

Capture environment identity:

* CPU;
* RAM;
* architecture;
* kernel;
* filesystem;
* Docker version;
* Docker API;
* storage driver;
* BuildKit version;
* Velnor commit;
* fixture commit;
* Rust;
* Cargo;
* mbx;
* job image digest;
* runner configuration.

Separate internal Velnor latency from external GitHub network/scheduling latency.

## 38. End-to-end runner benchmarks

Measure at least:

## Job acquisition/startup

* ready runner → broker delivery;
* broker delivery → acquired payload;
* acquire → admission;
* admission → capacity;
* capacity → checkout start;
* checkout duration;
* Docker setup;
* create;
* start;
* first user command;
* completion overhead;
* teardown.

## Rust

* fully cold build;
* warm build;
* no-op build;
* fresh worktree same commit;
* small source edit;
* dependency/lockfile update;
* feature-set change;
* build-script change;
* native `*-sys`;
* proc macro;
* `cargo check`;
* `cargo build`;
* nextest;
* clippy;
* docs;
* multiple simultaneous jobs.

## Docker

* existing image;
* image pull;
* simple job container;
* service container;
* Docker action;
* Buildx;
* cached Docker build;
* uncached Docker build;
* testcontainers.

## Persistent host

* first job;
* 2nd job;
* 10th job;
* 100th job;
* long-running node after GC.

Collect:

* p50;
* p95;
* p99 where enough samples exist;
* min/max;
* variance;
* CPU;
* max RSS;
* IO;
* disk bytes;
* process count;
* Docker API/CLI call count;
* cache hit/miss;
* bytes copied;
* bytes downloaded;
* bytes reused.

Do not report one lucky run.

## 39. Comparative performance

At minimum benchmark Velnor against an official self-hosted `actions/runner` installation on the same host class for identical workflows.

Where practical and reproducible, investigate other serious GitHub-compatible runner implementations/products that can be tested under comparable conditions.

Do not make "fastest in the world" a marketing statement without evidence.

Treat it as the engineering target.

Find the fastest measurable competitor/baseline available and beat it in the workloads Velnor prioritizes.

For Rust-heavy workloads, measure both:

* runner overhead excluding compile;
* total workflow wall clock.

## 40. Profile before optimizing

Use appropriate tooling:

* tracing;
* `perf`;
* flamegraphs;
* syscall/process traces;
* filesystem measurement;
* Docker event/API traces;
* heap/allocation profiling where useful.

Find actual time.

Do not optimize based on intuition.

For every claimed hot-path optimization record:

1. hypothesis;
2. baseline;
3. change;
4. correctness result;
5. after measurement;
6. conclusion.

## 41. `velnor-actions-fixture` must become the executable specification

Re-derive its capability baseline from the exact target Velnor branch.

Do not keep a hardcoded obsolete release/manifest as the readiness authority.

The verifier should know exactly which Velnor commit/image is under test.

Require live evidence.

No stale evidence may establish readiness.

## 42. Redesign Rust fixture scenarios

Separate at least these scenarios:

## A. Default Velnor Rust path

Workflow uses ordinary Cargo.

No explicit sccache.

No explicit mbx action.

No `RUSTC_WRAPPER`.

No `actions/cache` of `target` unless the scenario specifically investigates conflict.

This is the most important Rust scenario.

It proves the out-of-box Velnor path.

Use the same workflow commands on the GitHub-hosted lane.

The hosted lane may naturally be slower; semantic evidence must still match.

## B. Explicit sccache compatibility

Explicitly request sccache.

Verify mbx is disabled as designed.

Verify correct semantics.

This is compatibility, not the default benchmark.

## C. Explicit acceleration opt-out

Disable Velnor Rust acceleration.

Verify plain Cargo still works.

## D. Cache interaction

Test:

* mbx + Cargo source persistence;
* user `actions/cache`;
* target caching conflicts;
* explicit sccache;
* toolchain changes;
* lockfile changes;
* trust changes.

## E. Parallel Rust

Run independent simultaneous Rust jobs against one Velnor host.

Measure compile deduplication/resource scheduling and overall completion.

## 43. Fixture should compare semantics, not implementation details

For ordinary parity tests, compare observable GitHub semantics.

Normalize only values that genuinely cannot be identical, such as:

* runner identity;
* run IDs;
* timestamps.

Do not normalize away fields merely because Velnor differs.

A mismatch must fail until:

* Velnor is fixed; or
* the capability is explicitly unsupported for a valid reason.

Do not change the fixture to make Velnor green.

The fixture's own `AGENTS.md` principle remains important:

**Fix fixture failures in Velnor.**

## 44. Convert first-party verifier logic to Rust

The verifier currently contains Python-based:

* semantic evidence logic;
* workflow audits;
* Docker socket probes;
* comparison scripts.

Create coherent Rust tooling for these responsibilities.

Prefer one or a small number of well-designed verifier binaries/crates over many scripts.

The Rust verifier should support:

* capability audit;
* workflow audit;
* evidence schema;
* evidence normalization;
* semantic comparison;
* Docker API probes;
* readiness verdict;
* benchmark-result validation.

Test the verifier itself.

The oracle must not be less reliable than the system it certifies.

## 45. Differential tests

For supported semantics, run the identical fixture on:

* GitHub-hosted;
* Velnor.

Compare canonical semantic evidence.

Add focused scenarios for every bug class discovered.

Especially cover:

* continue-on-error;
* outcome vs conclusion;
* `always()`;
* `failure()`;
* skipped steps;
* post steps;
* outputs;
* command files;
* composite state;
* cancellation;
* timeout;
* failed JS actions;
* failed Docker actions.

## 46. Fault-injection suite

Production readiness cannot be proved only by happy paths.

Build deterministic fault tests for at least:

* GitHub transient 5xx;
* GitHub 429/rate limit;
* expired auth;
* registration disappearance;
* broker disconnect;
* run-service disconnect;
* lease-renew failure;
* completion failure;
* publisher failure;
* process SIGKILL;
* daemon restart;
* slot restart;
* Docker daemon disconnect/restart;
* Docker command/API hang;
* container start failure;
* service readiness failure;
* disk full;
* low disk;
* cache corruption;
* malformed archive;
* stale lock;
* SQLite busy;
* slow filesystem;
* cancellation race;
* completion/cancellation race;
* cleanup failure.

After each fault prove:

* bounded behavior;
* correct GitHub result where possible;
* no orphaned resources;
* no cross-job contamination;
* recoverable durable state;
* useful diagnostics.

## 47. Soak testing

Add long-running stress tests.

Examples:

* hundreds of sequential tiny jobs;
* sustained multi-slot execution;
* repeated Rust warm builds;
* repeated checkouts;
* repeated Docker builds;
* random cancellation;
* periodic daemon restart;
* periodic GC;
* intentional cache churn.

Monitor:

* memory growth;
* file descriptors;
* threads;
* processes;
* Docker objects;
* disk;
* journal size;
* logs;
* latency drift.

The 500th job should not be mysteriously slower because the runner leaked state.

## 48. Security and isolation verification

Retain or improve fail-closed behavior.

Test:

* symlink attacks;
* path traversal;
* archive traversal;
* malicious cache paths;
* malicious action metadata;
* malicious Docker parameters;
* remote Docker endpoint injection;
* secret masking;
* credential cleanup;
* cross-repo cache access;
* fork cache poisoning;
* stale workspace state;
* Docker resource escape;
* BuildKit cross-trust state;
* OIDC;
* action download integrity.

Performance optimization must not weaken isolation.

## 49. Code organization

Do not organize code around historical implementation files.

Organize around stable domains.

Potential boundaries to investigate include:

* GitHub protocol transport;
* runner registration/session;
* slot supervisor;
* job lifecycle;
* admission;
* semantic expression/runtime state;
* action resolution;
* Docker backend;
* Docker client;
* Docker ownership;
* checkout;
* Rust acceleration;
* storage;
* capacity;
* completion/outbox;
* results publishing;
* observability.

These are hypotheses, not prescribed crate names.

Do not create dozens of tiny crates without a real ownership reason.

Do not keep enormous modules merely to avoid migration.

Use visibility intentionally.

Prefer narrow APIs and types that encode invariants.

## 50. Remove compatibility architecture

Breaking changes are allowed.

Delete:

* obsolete compiler-cache abstractions;
* old configuration fields;
* migration shims;
* duplicate APIs;
* old storage layouts;
* dead feature flags;
* fallback paths whose only purpose is historical compatibility;
* deprecated command forms;
* wrappers around wrappers.

When replacing an architecture, finish the replacement.

Do not leave `old_*`, `legacy_*`, `v1`/`v2` duplication unless two versions are genuinely simultaneously required by an external protocol.

## 51. Latest APIs, not newest-for-novelty

Use current stable APIs.

Do not use nightly/experimental APIs merely because they are newer unless a measured capability requires them and stability is understood.

For Velnor itself, current stable Rust is the default target.

A point release fixing compiler correctness should be treated as urgent.

Modernization should reduce complexity, remove workarounds, improve safety, or unlock measurable performance.

## 52. Tests belong with architecture

Whenever architecture changes, update tests at the same time.

Prefer tests around invariants rather than exact internal implementation.

Use:

* unit tests;
* state-machine tests;
* property tests;
* fuzzing where parser/input boundaries justify it;
* integration tests;
* Docker integration tests;
* differential GitHub-hosted tests;
* fault injection;
* benchmarks.

Do not create brittle tests that prevent future internal optimization without protecting behavior.

## 53. Performance regression gates

Create a durable benchmark baseline.

Where CI hardware permits reproducibility, add regression gates for major deterministic local metrics.

Do not make noisy external GitHub timing a brittle hard CI threshold.

Separate:

* deterministic microbenchmarks;
* local runner benchmarks;
* environment-sensitive end-to-end reports.

Track history.

## 54. Documentation follows the final architecture

Do not spend early effort documenting transitional states.

Once a major architecture stabilizes, update docs to describe only the final model.

Remove obsolete docs.

Document:

* runner lifecycle;
* job lifecycle;
* Docker backend;
* Rust acceleration;
* cache/storage layout;
* trust model;
* scheduling;
* cancellation;
* recovery;
* GC;
* diagnostics;
* opt-out;
* supported GitHub semantics.

Do not write migration documentation unless it is genuinely needed.

This is a research branch with breaking changes.

## 55. Priority ordering for execution

Use this priority hierarchy.

## P0 — benchmark-invalidating correctness and stability

Examples:

* stale compiler/toolchain correctness release;
* incorrect GitHub semantics;
* lost/duplicate completion;
* cache corruption;
* trust leakage;
* stale execution;
* orphan ownership;
* invalid crash recovery;
* cancellation races;
* bugs that invalidate performance measurements.

These are fixed before believing benchmarks.

## P1 — runner startup and Docker hot path

Examples:

* job acquisition/admission latency;
* internal serialization;
* Docker CLI/process churn;
* repeated inspection;
* container creation;
* image/tool initialization;
* checkout;
* first-command latency;
* teardown blocking the next job.

## P1 — Rust workload performance

Examples:

* mbx architecture;
* persistent Cargo state;
* cross-worktree reuse;
* no-op build;
* source-edit rebuild;
* dependency update;
* parallel jobs;
* linker/resource scheduling.

## P1 — semantic parity of claimed functionality

Close supported-surface gaps instead of merely documenting them.

## P2 — storage/GC/resource scheduling

Make long-running hosts efficient and bounded.

## P2 — code decomposition

Restructure architecture when it eliminates bug classes or unlocks the performance/stability work.

## P3 — smaller optimizations and cleanup

Do these after architectural problems are addressed.

Do not spend the run polishing cosmetic documentation while P0/P1 issues remain.

## 56. Implementation loop for every work package

For each meaningful change:

1. Opus investigates existing behavior.
2. Opus states root cause.
3. Opus states desired invariant.
4. Opus identifies upstream semantic reference if relevant.
5. Opus designs benchmark/test.
6. Baseline is recorded.
7. Opus writes implementation task.
8. Sonnet implements.
9. Sonnet runs targeted tests.
10. Sonnet runs format/lint.
11. Sonnet runs benchmark when relevant.
12. Independent Opus reviews code.
13. Independent Opus checks benchmark validity.
14. Findings go back to Sonnet.
15. Repeat until clean.
16. Integrate.
17. Run broader regression suite.
18. Update master plan.

Never use:

> implemented, seems good

as completion evidence.

## 57. Required final verification

Before declaring completion, run the complete relevant Velnor gates.

At minimum:

* formatting;
* Clippy with warnings denied;
* workspace tests;
* integration tests;
* Docker tests;
* verifier audits;
* differential fixture suites;
* default mbx Rust suite;
* explicit sccache suite;
* opt-out suite;
* Docker suite;
* action suite;
* runtime semantics;
* cancellation;
* fault tests;
* benchmark suite;
* soak/stress subset suitable for final verification.

Build and run the actual job image produced by the exact Velnor commit.

Do not certify source while testing an old deployed image.

Record image digest and Velnor commit in fixture evidence.

## 58. Completion criteria

This goal is not complete because:

* code compiles;
* tests mostly pass;
* mbx is installed;
* a benchmark improved;
* a plan exists;
* one bug is fixed.

It is complete only when all of the following are true.

## Architecture

* major runner lifecycles have coherent ownership;
* no known P0/P1 architectural defect remains without a demonstrated technical block;
* giant responsibility-mixed modules have been decomposed where their structure was enabling defects;
* old compiler-cache architecture is fully removed or intentionally isolated;
* current stable dependencies/APIs are used.

### GitHub correctness

* claimed semantics are differentially verified;
* step outcome/conclusion behavior is correct;
* conditions/status functions are correct for supported cases;
* cancellation is coherent;
* completion is durable and recoverable;
* unsupported work fails early and clearly.

### Performance

* runner startup/hot path is instrumented;
* Docker process/API overhead has been measured and optimized;
* ordinary Rust workloads receive the best default acceleration automatically;
* warm/no-op/cross-worktree builds are fast;
* concurrent Rust jobs use the machine coherently;
* checkout reuse is effective;
* BuildKit reuse is effective where safe;
* teardown is bounded;
* before/after evidence exists.

### Stability

* multi-slot admission does not mysteriously reject jobs because internal implementation is busy;
* long-running soak does not show unbounded resource growth;
* failures have bounded recovery;
* restart/cancellation/cleanup behavior is tested;
* no known resource leak remains.

### Disk

* storage ownership is clear;
* GC is bounded;
* duplicate caches are justified;
* disk-pressure behavior is tested.

### Pure Rust

* Velnor business/runtime logic remains Rust;
* substantial first-party verifier/benchmark logic is Rust;
* scripting remains only thin workflow/test glue.

### Verifier

* capability baseline derives from the target Velnor;
* default Rust suite actually tests transparent mbx;
* sccache is a separate compatibility scenario;
* semantic comparisons use live dual-lane evidence;
* stale/missing evidence fails readiness.

### Quality

* format passes;
* Clippy passes;
* tests pass;
* docs match implementation;
* independent Opus final reviewers have no unresolved high-severity finding.

## 59. Required final report

At the end produce one concise but evidence-rich engineering report containing:

1. exact starting SHAs;
2. exact final SHAs;
3. original architecture;
4. final architecture;
5. major bug classes discovered;
6. root architectural causes;
7. components removed;
8. components introduced;
9. module/crate restructuring;
10. GitHub semantic gaps fixed;
11. cancellation architecture;
12. completion/recovery architecture;
13. Docker architecture before/after;
14. Docker CLI/API call-count comparison;
15. Rust acceleration architecture;
16. Mr. Boxington version/integration;
17. Cargo persistence model;
18. resource scheduling model;
19. cache/trust model;
20. storage/GC model;
21. fixture/verifier redesign;
22. fault/soak results;
23. before/after startup benchmarks;
24. before/after Rust benchmarks;
25. cold/warm/no-op comparison;
26. cross-worktree comparison;
27. parallel-build comparison;
28. disk comparison;
29. official actions/runner comparison;
30. remaining measured bottlenecks;
31. any proven external/technical blockers.

Every benchmark claim must identify the environment and sample methodology.

## 60. Final working rule

Do not stop after analysis.

Do not stop after the plan.

Do not stop after the first successful refactor.

Do not stop after fixing the currently visible bugs.

Continue iteratively from the highest-priority remaining defect to the next.

When an implementation exposes a deeper architectural flaw, update the plan and fix the architecture.

When a benchmark exposes a bottleneck, investigate it.

When the verifier exposes a semantic difference, determine the upstream behavior and fix Velnor.

When an Opus reviewer finds a structural problem, send it back to Sonnet and iterate.

The desired end state is not "better than the starting branch."

The desired end state is a Velnor architecture that a principal Rust systems engineer, GitHub Actions runner engineer, CI/CD architect, distributed-systems engineer, and performance engineer can inspect and find intentionally designed:

* extremely fast;
* extremely predictable;
* extremely stable;
* easy to reason about;
* hard to put into invalid states;
* properly observable;
* fully benchmarked;
* aggressively Rust-native;
* and optimized for persistent, concurrent Rust CI on Docker.

Optimize for that architecture, even when achieving it requires substantial breaking refactoring.

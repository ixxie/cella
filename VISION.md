# Vision

A working document capturing the current state of thinking on the broader
project that cella anchors. Informal, medium-length, will evolve. Not a
formal spec. Anchored in cella because cella is the closest piece to being
the runtime substrate; other repos (seedling, cyberdeck, dotfiles, grove)
play their own roles in the same picture.

## What we're building

A unified, modular computing environment grown out of existing work
(seedling, cella, cyberdeck, dotfiles, grove) and layered with new pieces
as we go. The product is a platform with a catalog of microproducts on
top of it: each microproduct is independently valuable, all of them
composing through a shared substrate. Picking one without the others must
feel complete; picking several must feel coherent.

The values are sovereignty, privacy, cooperative computing, and agentic
safety. The audience is a deliberately diverse set of people:

- me: Linux-native software engineer, full-stack power user
- my partner: dance professional, no interest in configuring computers,
  high bar for polish
- my mother: tech-savvy senior, Apple-coming, cares about privacy and
  data ownership
- nerd friends: want a hassle-free Linux that simply works

The design target is partner and mom first. If they can use it, everyone
else inherits the power layer underneath for free.

## The soul of the project

This is a collection of microproducts, not a platform with apps on top.
Unix philosophy applies literally: do one thing well, compose through
shared conventions, let users take only what they want. The disciplines
that follow from this are non-negotiable:

- Any single mod, installed alone, is a complete product. No suite
  pitch. Installing one mod never implies needing another.
- Uninstalling one mod never breaks another. A mod that hard-depends
  on another is a graft, and grafts are themselves optional.
- Data survives mod removal. Cells own the data; mods are views and
  operations over cell state.
- No mod is the platform. Even the identity dashboard is optional.
  Cella is the only non-optional piece, and cella is just a runtime.
- The only shared dependency for mod authors is the substrate. No mod
  imports from another mod. Messages and the public api/, models/,
  and bindings/ touchpoints are the only ways mods reach each other.
- Shared UX is opt-in, not mandatory. A mod can consume the core
  design system or be pure HTML with its own styling. Both are valid.
- Polyglot mod authoring is welcome. The substrate's interfaces are
  wire-level; the mod author picks the language.
- The substrate stays lean. Every substrate feature is a tax on every
  mod. Minimum substrate = identity, signing, policy, decisions,
  message bus, sync, mod loading. Anything else lives in core as an
  opt-in library.
- Cella is replaceable too. Any runtime that speaks the same on-wire
  protocols and implements the same traits can host the cells. No
  substrate-level lock-in, even from us.
- No cross-mod marketing. Each mod has its own pitch, audience, and
  adoption curve. Bundles and starter kits are curation, not default.

The interface is richer than text streams, so keeping mods cleanly
composable under it is harder than Unix's original pipe story. That
means the "substrate stays lean" rule is the single hardest line to
hold, and the line most likely to slip. Hold it anyway.

## The core primitive: Cell

A cell is a bounded, signed container of data, optionally with a runtime.
Cells nest. Cells can join other cells. Every cell has a keypair and
every state change is signed by cells holding the roles required by
policy.

Identity and container are unified: your home cell is you. Your home
cell carries your keys, your personal data, your mod configuration. When
you interact with another cell, your home cell takes a role there. This
makes sovereignty literal rather than marketing: you are a cell you
carry between machines.

Cell is the only structural primitive. System (machine), Mod (installable
capability), Repo (storage), Agent, Organization: all are kinds of cells.
Kind is descriptive metadata, not a gating type. The cell's manifest
defines what it actually does.

## Core vocabulary

Six concepts, grouped in three layers:

Containment
- Cell: universal primitive; signed, bounded, can nest
- Mod: a cell packaged as installable capability

Governance
- Role: what a cell can do inside another cell; implies membership
- Policy: signed rules inside a cell (role definitions, decision
  thresholds, inheritance, mod policy)
- Decision: a signed state change; multi-sig when policy requires it;
  technically a typed subclass of Message

Communication
- Message: the actor-model primitive for cell-to-cell and in-cell
  signaling; the bus every mod talks over

Branches exist as a power-user concept borrowed from git, for variant
or draft work on cell state.

## Architectural principles

- Isomorphic interface, polymorphic backends, applied recursively. At
  the platform level: one cell interface over many storage and runtime
  backends (git, postgres, garage, microvm, nspawn, nix-shell). At the
  mod level: one mod domain over many surfaces (app, term, web, deck)
  and many APIs (public, network, agent, cli).
- Data lives in cells, mods are views and operations. Uninstalling a
  mod does not delete data. Swapping a mail mod preserves mail. This
  is the sovereignty guarantee with real teeth.
- Thick interface, thin drivers. Security, governance, and decision
  enforcement live in the interface layer. Drivers move bytes and boot
  runtimes but are never trusted with policy.
- Substrate leverage with strangler-fig replacement. Lean hard on
  NixOS, niri, git, microvm.nix, age, SSH, Wayland to ship. Replace
  substrates only when (a) we have used them long enough to know
  exactly what we would do differently, (b) they are actively hurting
  the product, and (c) a native version genuinely improves the user
  story. Most substrates will stay external forever; that is fine.
- Drivers are visible, named, and switchable. Every driver declares
  both its capabilities and its limitations in its manifest (size
  limits, fork cost, sync semantics, conflict behavior, missing
  features). Templates pick sensible defaults so most users never
  touch driver choice, but the ones who do are a valued audience,
  not outliers. Non-programmers tinker with drivers in every adjacent
  space (audio, graphics, game mods, printers), and the platform
  should welcome that. The rule is not "hide the drivers"; the rule
  is "make the limits legible before a limit matters".
- The driver pattern applies recursively, including to the runtime
  itself. The actor runtime is a driver behind an ActorRuntime trait;
  Kameo is the first implementation, Ractor is the fallback, a
  hand-rolled native runtime is the emergency exit. Transport
  (federation, peer discovery, NAT traversal) is a separate driver
  behind a Transport trait so it can evolve independently of the
  local actor model. Cella's internal code talks to traits, not to
  Kameo or libp2p directly. This keeps every load-bearing decision
  reversible — if any substrate misbehaves, it becomes a swap, not
  a refactor.

## Governance model

Policy is signed state inside a cell. It defines the roles that exist,
which decisions need which signatures, what inherits from parent cells,
and which mods may be installed and by whom.

Policy is changed by following the current policy: recursive, the way
constitutions, DAOs, and cooperatives already work. Bootstrap: the
creator of a cell has implicit founder authority until they add other
admins, and this should be visibly temporary in UI.

Lockout protection is mandatory. Some meta-rules are immutable at
creation, policy changes require higher quorum than routine decisions,
and changes can be time-locked so they are reversible within a window.
Pick at least two.

Irreversible actions (deleting data, publishing a secret, revoking a
key) carry higher signing thresholds than reversible ones. This is the
foundation of agent safety: agents are free to make reversible
decisions but anything irreversible requires a human signature.

## The mod system

A mod is a cell packaged for distribution and installation. Three kinds:

- feature: autonomous, owns a domain (mail, calendar, notes, invoices)
- composition graft: connects two or more features; owns the
  relationship, not a domain
- integration graft: bridges features to external systems (Xero,
  Google, Fastmail)

The layered internal structure is lifted directly from seedling's
composable architecture: models, interfaces, handlers, drivers, api,
hooks, components, bindings, app. Inner layers never import outer.
Cross-mod imports only through three public touchpoints: api/, models/,
bindings/. Dependency ordering (o1, o2, o3...) is enforced at build and
install time.

The mod system is seedling promoted to runtime. Seedling's underscore
features (_members, _circles, _decisions, _agents) become the cell
runtime's built-in substrate. Seedling itself graduates from ERP
project to reference implementation of the mod authoring pattern.

## Surfaces and APIs

A mod can render into multiple surfaces and expose multiple APIs.

Surfaces
- app: windowed native application
- term: CLI and TUI
- web: browser or embedded webview
- deck: cyberdeck shell integration (bar indicators, drawer panels,
  notifications, quick actions)

APIs
- public: in-cell, typed, fast, trusted across mods in the same cell
- network: federation, signed, versioned, auth-checked
- agent: widgets with prop schemas, the structured contract for LLM
  consumption
- cli: shell commands exposed to the cell's CLI

No surface is mandatory. Most mods will implement one primary surface
and contribute small deck surfaces. The substrate provides generic
renderers (auto-generated web view from models + schemas) so mods do
not have to build UI they do not need.

## Cyberdeck: the shell

Cyberdeck is the system-level shell, built on niri. It is the desktop
component of cells: a thin always-visible bar at the edge, with drawer
panels that expand over the screen without reflowing windows underneath.

The drawer framework is a single overlay surface with virtual drawer
layers. The layer-shell surface is tall (full output height) with a
small exclusive zone (just the base bar). Drawers animate inside the
transparent overlap region; windows below never reflow. Input region
updates dynamically so pointer events pass through when closed and are
captured when open. Keyboard interactivity switches from None to
OnDemand on drawer open.

Mods contribute drawer panels as plugs in cyberdeck's deck sockets
(deck:drawer-panel, deck:drawer-tile, deck:bar-indicator,
deck:notification-surface). Cyberdeck owns the state machine, the
backdrop dim or blur, the animation system, keyboard focus transitions,
and dismissal. Mods just provide content.

Design rules to lock in early:
- one drawer at a time; opening one closes the other
- escape always dismisses, click-outside always dismisses
- per-edge defaults (top for system, right for notifications, left for
  launcher, bottom for media), configurable, but defaults matter
- GPU-native backdrop blur from the start; no screenshot-based blur

Each cell maps to a niri virtual-monitor group with its own visual
identity: name, color, wallpaper, theme. Switching cells is a
first-class navigation action. Partner and mom navigate by muscle
memory and visual color, not by reading labels. Agent cells get their
own virtual monitor or appear as windows inside the spawning cell,
configurable.

## Sync and replication

Every cell can exist on multiple systems simultaneously, its state
kept in sync across them. Sync is the primitive; backup is the
degenerate case where one end is a passive target. A cell is never
trapped in one place. This is where sovereignty becomes concrete
rather than aspirational.

- Primary: the live endpoint where writes originate. One at a time,
  per cell, but transferable.
- Replica: a signed, verifiable copy on another system. Usually
  read-only relative to the primary, promotable if the primary is
  lost, and in some configurations bidirectionally synced.

Sync targets are just other systems: another laptop, grove, a
friend's grove, an external disk at a parent's house, a phone. Each
target is declared in the cell's policy with its own schedule,
retention, sync direction, and role requirement. The cell decides
who is allowed to hold a replica and what that replica is allowed
to do.

The sync primitive is isomorphic across backends; drivers implement
it polymorphically. Git replicates via push. Postgres via WAL
streaming or logical replication. Garage or S3 via bucket-level sync.
File-backed stores via rsync. The runtime exposes one verb (sync) and
the driver does the work. This is the same isomorphic/polymorphic
pattern applied again at the data-plane level.

Replicas are signed by their origin. The receiving system verifies
signatures before storing. A replica cannot silently drift from its
primary; any divergence is visible as unsigned state.

Recovery from a replica is a first-class operation. If your primary
system is lost, you can promote a replica on another system to become
the new primary. The transition is itself a Decision recorded in the
cell's history; the old primary, if it ever comes back, recognises it
has been superseded.

This is also where key recovery lives. Lost keys can be recovered
through a set of guardian cells that hold replicas and, per policy,
have the authority to re-sign a cell under a new key. That escapes
the seed-phrase-or-lose-everything trap for non-technical users:
mom's keys are effectively held by a circle of people and devices
she trusts, not by a piece of paper in a drawer.

Partner and mom should never configure replication manually. Cell
templates ship with reasonable defaults: a personal cell replicates
to grove and one hardware backup; a family cell replicates across
every member's grove; a work cell replicates per the policy its
template defines. Power users override; normal users inherit.

## Pedagogic sequence

Concepts are introduced when the user feels their absence, not in
architectural order. The system has dozens of concepts; the design
question is the sequence of revelation.

- Lesson 1: Cell. Your home cell is you. Cells can nest and be
  navigated.
- Lesson 2: Role. You can have roles in other cells; membership is
  implicit in having a role.
- Lesson 3: Mod. Cells can have installed capabilities.
- Lesson 4: System. Cells live on machines you can swap between.
- Lesson 5: Policy. Some things need rules.
- Lesson 6: Decision. Some things need agreement.
- Lesson 7: Inheritance. New cells from template cells.
- Lesson 8: Agent cells. Agents as members with their own cells.

Partner probably stops at lesson 2 or 3 and the system still serves her
well. Mom stops at lesson 4 or 5. Nerds reach 7 or 8. Developers reach
tiers above 8 (drivers, runtimes, conformance, mod authoring). Every
tier must be a complete product on its own. No user is ever forced to
understand a concept that does not resolve a pain they have already
felt.

Lesson 1 has to be genuinely absorbable in one sitting: cell, home
cell, nesting, navigation, current-cell indicator, all in five
sentences or fewer. If it is not that clean, the model is not ready.

## Roles of existing repos

- cella: becomes the cell runtime. Extends current worktree-and-VM work
  into a full cell lifecycle (create, open, fork, archive, destroy),
  soft and hard cells under one API, secrets and identity plumbing,
  policy enforcement at the cell boundary.
- cyberdeck: becomes the shell. GPU-rendered bar plus the drawer
  framework, deck sockets for mods, virtual-monitor-per-cell model,
  niri-native integration.
- seedling: becomes the reference mod and the source of the underscore
  substrate features. Its pattern (layered modules, sockets/plugs,
  events/subs, branded types, Deps injection) is the blueprint every
  other mod will follow.
- dotfiles: informs the OS-level choices. NixOS + home-manager + niri
  configuration becomes the platform default.
- grove: the persistent host. A forge plus a cell daemon: stores
  cells, runs hard cells, serves them to devices, syncs state. This
  is where sovereignty becomes concrete for mom and partner.

## MVP shape for the summer

The summer deliverable is explicitly a dev-lab prototype, not a
production release. First users are me, my brother, and brave friends
who can handle rough edges. Partner and mom see something late in
the summer at the earliest, via web, with the pedagogic sequence
exercised on the smallest useful mod.

Hard cells (microvm / VM sandboxing) and cyberdeck (desktop shell
and drawer framework) are explicitly deferred to after the summer.
The summer runs on soft cells with web and CLI as the initial
surfaces. Terminal UX and any shell-level work come later.

Phase 0: Hygiene. Back up the system to multiple locations, archive
dayjob (qcore) and old-job (mascope family) out of ~/repos, commit
cyberdeck's WIP, name the project, pin this document.

Phase 1: Soft cell runtime in cella. A cell is a signed directory
with a manifest, policy, keys, and signed decision history. Lifecycle:
create, open, fork, archive, destroy, sync. Policy enforcement at
the cell boundary. Keys live only on the primary host; replicas are
read-only unless promoted via a decision. No VMs, no sandbox work,
no shell. Deliverables: cella CLI for local cell management, HTTP
admin API, conformance test for the soft-cell contract, the cell
runtime spike that resolves the Kameo/libp2p distribution question.

Phase 2: Server control. Cella in server mode (a grove) hosts
multiple cells and exposes an admin API. First-pass web admin UI
for managing cells, mods, and policy on a remote grove. This is
the first user-visible milestone: a brave friend installing this
touches the admin UI first. Built on SvelteKit (lifted from
seedling's existing stack).

Phase 3: Composable mod system lifted from seedling. Mods load as
features / grafts / integrations using seedling's layered internal
structure. Bindings registry per cell. Deps injection from the
cell runtime. Message bus for in-cell and cross-cell mod
communication. Monorepo layout with tag-based maturity tiers
(see next section).

Phase 4: First work mods on web. Notes and the identity dashboard
are the leading candidates: notes because it exercises editing,
sync, and the mod pattern end to end; identity dashboard because
I need the tool to dogfood everything else. Both shipped as mods
consuming the cell runtime via the composable architecture.
Surfaces: web as primary, CLI for devs, nothing else yet.

Post-summer: Hard cells, cyberdeck shell and drawer framework,
terminal and graphical UX, deck and app surfaces for mods,
additional work mods, autonomous agent safety, federation across
groves, verified mod authoring, key recovery product design,
multi-user governance UX polish.

Prototype vs production is an honest distinction. The summer
prototype is suitable for dogfooding and small trusted groups.
Reaching production for non-technical users requires at minimum:
hard cells (isolation), a real key recovery story (guardian cells
or equivalent), threshold signing for multi-member cells, agent
containment at the decision level, and UX polish informed by
actual users. None of that is in the summer; all of it is a
logical next wave.

Agent safety in the soft-cells-only summer is handled by policy,
not isolation. Agents run in-process with the cell runtime. Every
agent action is a decision subject to current policy. Destructive
or irreversible actions always require a human signature.
Reversible actions on an ephemeral branch can be auto-approved
but require a merge decision before affecting main state. There
are no unattended background agents until hard cells exist;
agents are interactive, step-by-step, and visible. This is
sufficient for the summer demo because the agent story we're
pitching is "ask, review, commit", not "set an agent loose".

## Monorepo and maturity tiers

All mods live in one repository during development. This is a
development choice, not a shipping choice: the monorepo colocates
code so the cell runtime and the mods that consume it can evolve
together during the churn phase. Shipping artifacts are still
separate per mod; cross-mod imports remain forbidden; the Unix
philosophy still holds because what matters is runtime
independence, not repo boundaries. Nixpkgs, Chromium, and the
Rust compiler are all monorepos full of independently composable
pieces.

Maturity is expressed with Docker and git style tags, not
directories or manifest fields. A mod has one name and carries
multiple signed tag pointers into its history:

```
notes:alpha          # tip of the alpha line
notes:beta           # tip of the beta line
notes:gamma          # tip of the gamma line (stable latest)
notes:v0.4.0-alpha   # specific version in alpha channel
notes:v0.8.2-beta    # specific version in beta channel
notes:v1.0.0         # specific gamma release
notes:latest         # alias for gamma tip
```

Tags are signed by the mod author. Promoting a mod from alpha to
beta is a signed tag move; demotion is the same operation in
reverse. The tag history is auditable end to end. Unsigned tag
moves are rejected by the receiver — this is the Docker "latest"
supply-chain lesson applied by default.

Channel subscription lives in cell policy. A user cell declares
`mods.channel = "gamma"` (mom and partner default), `"beta"`
(power user), or `"alpha"` (dev / dogfood). The install verb
respects this and can be overridden explicitly. Pinning to a
specific version is always available for any user.

A registry is just another cell. The mod catalog is a cell whose
state is a list of mod references with their signed tags. Cella
is a member of one or more registry cells. Multiple registries
coexist: a default public registry, a curated family registry
mom subscribes to, a brave-friends registry for prototype-grade
mods. Resolving `notes:beta` against a registry returns a signed
mod-cell state whose author signature is verified against a
trust list. This is recursive use of the primitive, which is
right, and it is how social sovereignty over what software runs
on a user's machine becomes real.

The tiers mean something specific and that meaning is enforced:

| Tier | Target user | Guarantees | Testing bar | Cadence |
|------|-------------|------------|-------------|---------|
| alpha | Me + brave dogfooders | May break, data loss possible, API churns freely | Smoke tests, "it boots" | Every commit |
| beta | Early adopters who accept rough edges | Works for normal flows, API stable-ish, upgrade path exists, no data loss on upgrade | Integration tests cover happy paths and destructive ops | Weekly or feature-based |
| gamma | Everyone, including mom and partner | Stable API, semver, no data loss, recovery stories exist, documented, supported, security-reviewed | Integration + property + security review | Semver release cycle |

Graduation is a checklist, not a vibe:

- alpha to beta: happy-path integration tests pass; no known data
  loss bugs; upgrade path tested; a human has used it for a week
  and it held up.
- beta to gamma: all of the above, plus security review of secrets
  path, key handling, policy enforcement, decision signing;
  documented user flows for install, uninstall, data export; a
  second human (not the author) has used it for a week; the API
  has been stable for two release cycles.

Summer targets by tier (honest guess):

- cella core: alpha, approaching beta by end of summer
- identity dashboard: alpha, dogfooded heavily, possibly beta
- notes: alpha, usable by me and one or two friends
- everything else: does not exist yet or stays alpha

The summer goal is that one piece (cella core or identity
dashboard) credibly crosses into beta, and everything else is
honest stable alpha. No gamma releases this summer.

## Open questions

- The project's name. Still unnamed. Biological naming family
  (grove, seedling, cella, cell) is already coherent; the umbrella
  name should fit. Candidates worth considering: biome, mycelium,
  lattice, stroma, bloom, tendril.
- First mod pick is narrowed to notes or identity dashboard (or
  both) but not finally chosen. Notes is more platform-exercising;
  identity dashboard is more useful to me as a debug tool.
- Key recovery story for non-technical users: guardian cells,
  hardware keys, social recovery. This is the hardest unsolved
  problem and needs a design spike before production.
- Threshold-signing story for multi-member cells: age multi-sig,
  BLS, or append-only signed governance branch.
- Whether to keep "Circle" as UX vocabulary for collective cells
  even though the architecture has no Circle concept.
- Exact shape of server control for Phase 2. Admin-over-cells vs.
  host-of-self-governing-cells changes the UX and the API surface.
  Leaning toward the latter: cells self-govern per their policy,
  grove just provides compute, storage, and network.
- Agent safety beyond multi-sig. Semantic containment, blast-radius
  tracking, reversibility at the action level. Soft-cells-only
  summer leans on "interactive and visible"; production needs more.
- Write amplification and crypto cost of signing every mutation.
  For low-volume cells it is fine; high-volume cells need a
  batching / coalescing / snapshot-commit story.
- User testing with partner and mom. When, how, and what to change
  based on what is learned. This is the single most valuable
  information available; lack of ground truth is the biggest risk
  in the whole plan.

## Design discipline to carry through

- Minimum number of concepts in the first lesson; everything else is
  reached only when felt.
- Flat cell addressing; nesting is semantic, not a path.
- One navigation verb; current cell always visible.
- Data in cells, mods as views, uninstall never destroys.
- Every tier of the pedagogy is a complete product.
- Every mod can be built against the platform's public interfaces
  alone; if it needs to reach under the hood, the platform is not
  ready, fix the platform.
- Partner and mom are the design target; power is a second-order
  consequence of the primitives being right.
- Every driver declares its limitations; warnings surface at the
  point of decision, not after. No user is ever blindsided by a
  limit the system knew about in advance.
- The substrate stays lean. Every feature added to the cell runtime
  is a tax on every mod. Anything optional lives in core as a
  library a mod can choose to use. The "substrate stays lean" rule
  is the most likely to slip; hold it anyway.
- Microproducts, not a platform with apps. Each mod is pitched,
  shipped, installed, used, and uninstalled independently. No
  suite marketing. No bundles as the default. Curated starter
  kits are allowed but always opt-in.
- Monorepo for development, independent shipping artifacts,
  tag-based maturity. What is colocated in code is separate on
  the wire.
- Tag moves are signed. Unsigned tag pointer updates are rejected
  by the receiver. Supply-chain sovereignty starts here.
- Prototype and production are different things. The summer ships
  a prototype. The word "production" is reserved for when hard
  cells, key recovery, threshold signing, agent containment, and
  user-tested UX polish all exist.
- User testing is a phase requirement, not a bonus. Partner and
  mom see prototypes before the summer is over. The design changes
  based on what they actually do, not on what I predicted.

# DiskAnalyzer

A read-only inventory of what is actually using space on your drive, with the
live operating system left out of the way so the things you *can* act on are
the things you see.

It reads. That is all it does. There is no code path in this project that
deletes, moves, renames or modifies a file.

```
cargo build --release
./target/release/diskanalyzer
```

A single native executable. No runtime, no dependencies to install.
Windows, macOS and Linux. Each tagged release also attaches a prebuilt binary
per platform to its
[GitHub release](https://github.com/JustGritt/DiskAnalyzer/releases).

---

## What you get

A **terminal report** with the volume gauge, where the space is, what kind of
thing it is, the largest files, and the large files you have not touched in
over a year (`--old-min-days` to change that).

An **HTML report** (`report.html`) with an interactive treemap you can click
into, plus sortable and filterable tables: largest folders, folders holding the
most loose files directly inside them, largest and oldest files, and what was
skipped and why. One self-contained file: no network, no companion assets.
Open it, move it, mail it to yourself; it still works.

**CSV listings** (`files.csv`, `folders.csv`, and `changes.csv` with
`--compare`) for the questions the reports do not answer. Sort every file you
own by size, filter to one extension, hand the list to another tool.

**`report.json`**, the same data the HTML report shows, for scripts and for
keeping a history of scans. It also records the settings the scan ran with
(roots, exclusions, sizing, floors). The document carries `"schema":
"diskanalyzer.report"` and a `schemaVersion`, which only changes when an
existing key changes meaning; new keys may appear at any time.

Reports land in `reports/scan-<timestamp>/` unless you pass `--out`.

---

## Usage

```
diskanalyzer                          scan the system volume, skipping the live OS
diskanalyzer --compare last           ...and say what changed since the previous scan
diskanalyzer --duplicates             also find duplicate files
diskanalyzer ~/Projects /Volumes/Ext  scan specific folders or drives
diskanalyzer --csv-min-size 0         list every file, not just large ones
diskanalyzer --help                   all options
```

### Scope

| Option | Effect |
| --- | --- |
| `--include-os` | also scan the live OS folders that are skipped by default |
| `--exclude PATH` | skip a folder (repeatable; relative paths resolve against the current directory) |
| `--exclude-name NAME` | skip any folder with this name (repeatable) |
| `--cross-filesystems` | descend into other mounted filesystems (Unix; off by default) |

An `--exclude` that is not an existing folder, or an exclusion that ends up
skipping nothing, is noted on stderr so a typo does not go unnoticed.

### Sizing

| Option | Effect |
| --- | --- |
| `--allocated` | report on-disk size instead of logical size |
| `--cluster-size BYTES` | cluster size for `--allocated` on Windows (default 4096) |
| `--dedup-hardlinks` | count a file with several hard links once (slower on Windows, see below) |

### Detail

| Option | Effect |
| --- | --- |
| `--top N` | folders per ranking (default 25) |
| `--files N` | files per ranking (default 30) |
| `--old-min-size SIZE` | size floor for the untouched-files list (default 100MB) |
| `--old-min-days DAYS` | age floor for the untouched-files list (default 365) |
| `--stale-days DAYS` | a project unchanged this long counts as idle in the reclaimable-folders list (default 90) |
| `--duplicates` | look for duplicate files |
| `--dup-min-size SIZE` | ignore duplicates below this size (default 25MB) |

### Output

| Option | Effect |
| --- | --- |
| `--out DIR` | report directory |
| `--csv-min-size SIZE` | size floor for `files.csv`; `0` lists every file (default 10MB) |
| `--folder-min-size SIZE` | size floor for `folders.csv` and for changes reported by `--compare`; `0` lists every folder (default 1MB) |
| `--no-html`, `--no-csv`, `--no-json` | skip an output format |
| `--no-color`, `--quiet` | plain text; no progress line |
| `--threads N` | worker threads (default: twice the CPU count, at most 16) |

### Compare

| Option | Effect |
| --- | --- |
| `--compare DIR` | report what changed since the scan in `DIR`; `last` picks the newest `./reports/scan-*` with a `folders.csv` |

Sizes accept `500MB`, `2GB`, `1.5g` or a plain byte count.

---

## The read-only guarantee

The promise is not "we were careful", and it is not a claim a compiled binary
can make about itself. It is enforced where it can be enforced, at build time:

- One module, `src/reports/sink.rs`, may create files, and only by bare name
  inside the directory you chose with `--out`.
- `clippy.toml` forbids every `std::fs` write, remove, rename and create API
  everywhere else in the crate. `sink.rs` carries the single, commented
  exemption.
- `tests/readonly_audit.rs` audits the source for the same API names outside
  that module, so a careless future edit fails the test suite.

Both run in CI, together with `cargo clippy -- -D warnings`.

```
diskanalyzer --verify-read-only
```

prints what is enforced and where. Scanned files are opened read-only, and only
when `--duplicates` needs to compare their contents.

---

## What is skipped, and why

The goal is "everything except the operating system", which is a narrower cut
than "everything the OS vendor made". Leftovers like `C:\Windows.old` and the
Recycle Bin are reclaimable, so they stay in the scan and get their own
categories.

Skipped by default (`--include-os` to include them):

- **Windows** — `C:\Windows`, `C:\Program Files\WindowsApps`, the Defender
  folders, `C:\Recovery`, `C:\PerfLogs`, `C:\Config.Msi`, `C:\$WinREAgent`,
  `C:\System Volume Information` (readable only as SYSTEM), and the legacy
  compatibility junctions that loop back into real folders.
- **macOS** — `/System`, `/dev`, `/Volumes` (scan other volumes explicitly),
  `/private/var/vm`, and the system-protected `/.Spotlight-V100`,
  `/.fseventsd`, `/.DocumentRevisions-V100`, `/.vol`.
- **Linux** — `/proc`, `/sys`, `/dev`, `/run`, `/lost+found`.

Never followed anywhere: junctions, symlinks and mount points. They are counted
in the report as links but not descended into, because following them would
double-count or silently wander onto a different disk.

On Unix the scan stays on the filesystem it started on, which is what keeps
`/proc`, tmpfs and external drives out of a scan of `/`. Pass
`--cross-filesystems` to descend anyway. macOS firmlinks are handled as the one
deliberate exception: a scan of `/` steps onto the data volume once, so `/Users`
is seen exactly once rather than twice or not at all.

Volume-level files the OS sizes for itself — `pagefile.sys`, `hiberfil.sys`,
macOS swap — are reported on their own line instead of being mixed into folder
totals. They can be several gigabytes each, but the way to change them is
System settings, never deletion.

---

## How space is classified

Knowing that your user folder holds 300 GB is not actionable. Knowing that
84 GB of it is regenerable build output is. Every file is sorted into one of 19
categories carrying one of three confidence levels:

- **safe** — regenerable or already discarded. Removing it costs you time, not
  data: package caches, build output, `node_modules`, temp folders.
- **review** — very often unwanted, but only you know: Downloads, installer
  caches, ISOs and archives, virtual disks.
- **info** — not junk, reported so the total adds up and you can see the shape:
  installed applications, games, media, cloud-synced folders.

Classification is by folder, inherited downward, so one match covers a whole
tree. A small set of override rules is checked at every level, because a broad
rule high in the tree would otherwise hide something specific below it.

These labels say where looking is worthwhile. Nothing more.

---

## Reclaimable folders

Category totals say *how much* is regenerable; they do not say *which* build
folders belong to projects you stopped working on. A 16 GB `node_modules` made
of small files never appears among the largest files, so the report also
ranks the top-most regenerable or review folders of 64 MB or more, each with
the project it belongs to and how long that project has been idle:

```
 48.5 GB  dev-build  idle 5mo     editor (rust)      /Users/me/Projects/editor/target
 25.8 GB  dev-cache  idle 2d      -                  /Library/Developer/CoreSimulator
 16.0 GB  dev-deps   idle 1y      old-site (node)    /Users/me/Projects/old-site/node_modules
```

- **Top-most only.** A `build/` inside `node_modules/` is part of that
  `node_modules`, and each byte is counted once. Caches inside installed
  applications or games are left out: they are the application's business.
- **The project** is the nearest folder, up to four levels above, holding a
  `.git` or a project file such as `Cargo.toml`, `package.json`,
  `pyproject.toml`, `go.mod`, a Gradle or Maven build, `CMakeLists.txt`,
  `Package.swift`, `pubspec.yaml`, `project.godot`, a `.sln` or `.csproj`, a
  `.uproject`, a Unity `ProjectSettings` folder or an Xcode project. A `.git`
  wins over a nearer project file, so a monorepo is judged as a whole. Your
  home folder and volume roots never count as projects.
- **Idle** is the time since the newest file in the project changed, *not
  counting the reclaimable folders themselves*: builds rewrite their output
  constantly and package stores may keep archive dates, so those dates say
  nothing about whether the project is alive. When no project is found, the
  folder's own newest file is used.
- A folder counts as idle after `--stale-days` (default 90). Idle ages are
  highlighted, and the HTML report can filter to idle projects only.
- **How to reclaim.** Where the tool that owns a folder has its own cleanup,
  the report names it: `cargo clean` for a `target` beside a `Cargo.toml`,
  `npm cache clean --force` for the npm cache, and so on. The terminal lists
  each one once under the folders; the HTML report and `report.json` carry it
  per folder as `hint`. A folder with no known command gets none. These are
  hints for you to run or ignore: DiskAnalyzer prints them and never runs
  them.

`folders.csv` carries a `newest_modified` column for every folder, and the
treemap can be colored by last modification instead of by category.

---

## Comparing scans

"The drive was fine last month" is a question about change, so a scan can be
compared with an earlier one:

```
diskanalyzer --compare reports/scan-20260814-075803
diskanalyzer --compare last
```

The terminal and HTML reports gain a section right after the scan summary:

```
-- CHANGED SINCE 2026-08-14 07:58 ----------------------------------------------
 Scanned  386.4 GB -> 384.0 GB  (-2.4 GB)      Free  30.1 GB -> 15.4 GB
 Grew
      +5.2 GB     48.5 GB -> 53.7 GB      .../Projects/editor/target/debug
      +1.1 GB         new -> 1.1 GB       .../Projects/site/node_modules
 Shrank
      -2.0 GB      2.0 GB -> removed      /private/tmp/something
```

- **Collapsed like the largest-folders list.** When a folder grew because one
  child grew, only the child is listed, so the same gigabytes do not repeat at
  every level up to the root.
- **The baseline** is read from the earlier report directory: `folders.csv`
  for the sizes, and `report.json` (when present) for its roots, sizing and
  folder floor. Reports from before `report.json` existed still work; the
  comparison says it cannot confirm the two scans measured the same way.
- **Floors.** `folders.csv` only lists folders above `--folder-min-size`, so
  a folder missing from the baseline may have been absent or just small; it
  shows as `new`. Changes smaller than this run's `--folder-min-size`, or the
  baseline's own floor if larger, are not listed. A baseline without
  `report.json` is assumed to have used the 1 MB default.
- **Removed** folders are listed at the top-most level that disappeared.
  One that this scan skipped (an `--exclude`, for instance) is marked
  `excluded`, and one that could not be read this time `unreadable`, rather
  than reported as gone or shrunk.
- A warning is shown when the roots differ (only what both scans cover is
  compared) or when one scan used `--allocated` and the other did not.

`changes.csv` lists every folder that changed by at least the floor, without
collapsing: `delta_bytes`, `delta`, `before_bytes`, `after_bytes`, `status`,
`full_path`. The baseline is read before anything is written, and `--out`
may not be the baseline directory itself.

---

## Duplicate detection

`--duplicates` groups files by exact size, then compares the first and last
64 KB of each candidate. A group of 40 GB virtual disks therefore costs a few
megabytes of reading rather than 40 GB.

Matching size, head and tail is strong evidence of a duplicate but it is not
proof, which is why the report says *likely* and why nothing acts on the
finding. The search stops at a read budget and says so when it does.

Hard links to the same file count as one copy: they share one set of bytes,
so removing all but one of them frees nothing. A group is reported only when
at least two separate files match.

---

## Accuracy notes

**Logical vs on-disk size.** By default the report shows logical file sizes.
Actual disk usage differs for compressed files, sparse files, and for the large
number of tiny files that each still occupy a whole block. `--allocated` uses
the real block count on Unix (`st_blocks`), and rounds up to `--cluster-size` on
Windows.

**Hard links.** A file with several hard links is counted once per link by
default, so a tree using them reads as larger than it is. Package managers
that share one copy between a cache and your projects do exactly this: uv
and pnpm hard-link packages from their cache into every environment, so the
same bytes show up in both. `--dedup-hardlinks` counts each file once, by
inode on Unix and by volume and file ID on Windows. The size goes to
whichever link the scan reaches first, so which of two folders shows it can
change between runs; the total does not.

On Unix this costs nothing. On Windows a directory listing does not carry
the link count, so each file is opened for its attributes only (no data
access, nothing created, nothing followed): a warm scan of about two million
files went from 4 to 27 seconds.

**Cloud-only files.** OneDrive or Dropbox "online-only" files on Windows, and
iCloud files that have not been downloaded on macOS, appear in a folder at
their full size although their content is not on the disk. By default they
count at that logical size, like every other file; with `--allocated` they
count as zero on Windows and at their real block count on Unix. Reading one
would make the sync client download it, so `--duplicates` never opens or
lists them, and `--dedup-hardlinks`, which opens each file on Windows, passes
over them. The scan summary says how many there are and their logical size.

**Unreadable paths.** Some folders need elevated rights; on macOS, most of your
own Library needs Full Disk Access. They are counted, listed in
`unreadable.csv`, and reported as a count so the totals can be read honestly
rather than looking complete when they are not.

**Coverage.** When a scan includes a whole volume, the summary states what
fraction of the volume's used space it accounted for. The remainder is the
skipped OS folders, unreadable paths, and the differences above. A scan of
individual folders shows no percentage, since it covers part of the volume by
design.

---

## Project layout

```
src/
  main.rs             process entry point
  cli.rs              argument parsing and wiring
  scanner/
    mod.rs            scan orchestration, config, results, rollup
    worker.rs         per-directory enumeration and thread-local statistics
    aggregate.rs      the single owner of the node arena and the streaming CSV
  exclusions.rs       what is skipped, and why
  categories.rs       classification rules
  projects.rs         project markers (Cargo.toml, .git, ...)
  compare.rs          --compare: reading a baseline and diffing against it
  insights.rs         rankings, reclaimable folders, duplicates, treemap pruning
  platform/           Windows, macOS and Linux specifics
  reports/
    terminal.rs       the terminal report
    payload.rs        the report as one JSON document
    html.rs           the self-contained HTML report
    template.html     its markup, CSS and treemap script
    json.rs           report.json
    csv.rs            the CSV listings
    sink.rs           the only module that creates files
  util.rs             formatting, size parsing, JSON
tests/
  fixture.rs          end-to-end scan of a built temp tree
  readonly_audit.rs   the read-only source audit
```

### How it works

Everything is accumulated in one pass; nothing re-reads the disk afterwards.
Per-file records are never kept — a large drive would cost gigabytes of memory
— so the walk holds one node per *directory* and streams file data into
fixed-size structures: a top-N heap, an extension histogram, age buckets, and
the CSV writer, which is written during the scan rather than after it.

A pool of workers pulls directories from one queue and pushes the
subdirectories it finds back onto it. Per-file statistics are accumulated
thread-locally and merged once at the end, so workers never take a lock on the
hot path; the two things that must be owned exactly once — the directory arena
and the `files.csv` handle — live on a single aggregator thread fed by a
channel. Child directory ids are always allocated after their parent's, which
makes the final size rollup one linear pass backwards through the arena with no
recursion and no sorting.

Scanning is dominated by directory syscalls rather than by CPU, so the win is
in the shape of the traversal, not the language. On a 226k-file tree, warm
cache: a single-threaded scan takes about the same time as the Python version
this replaced; the same scan across 16 threads is roughly four times faster.
```
python (previous version)   4.4 s
rust, --threads 1           4.2 s
rust, default threads       1.1 s
```

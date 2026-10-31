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
Windows, macOS and Linux.

---

## What you get

A **terminal report** with the volume gauge, where the space is, what kind of
thing it is, the largest files, and the large files you have not touched in
over a year.

An **HTML report** (`report.html`) with an interactive treemap you can click
into, plus sortable and filterable tables. One self-contained file: no network,
no companion assets. Open it, move it, mail it to yourself; it still works.

**CSV listings** (`files.csv`, `folders.csv`) for the questions the reports do
not answer. Sort every file you own by size, filter to one extension, hand the
list to another tool.

Reports land in `reports/scan-<timestamp>/` unless you pass `--out`.

---

## Usage

```
diskanalyzer                          scan the system volume, skipping the live OS
diskanalyzer --duplicates             also find duplicate files
diskanalyzer ~/Projects /Volumes/Ext  scan specific folders or drives
diskanalyzer --csv-min-size 0         list every file, not just large ones
diskanalyzer --help                   all options
```

### Scope

| Option | Effect |
| --- | --- |
| `--include-os` | also scan the live OS folders that are skipped by default |
| `--exclude PATH` | skip a folder (repeatable) |
| `--exclude-name NAME` | skip any folder with this name (repeatable) |
| `--cross-filesystems` | descend into other mounted filesystems (Unix; off by default) |

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
| `--duplicates` | look for duplicate files |
| `--dup-min-size SIZE` | ignore duplicates below this size (default 25MB) |

### Output

| Option | Effect |
| --- | --- |
| `--out DIR` | report directory |
| `--csv-min-size SIZE` | size floor for `files.csv`; `0` lists every file (default 10MB) |
| `--folder-min-size SIZE` | size floor for `folders.csv`; `0` lists every folder (default 1MB) |
| `--no-html`, `--no-csv` | skip an output format |
| `--no-color`, `--quiet` | plain text; no progress line |
| `--threads N` | worker threads (default: twice the CPU count, at most 16) |

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

## Duplicate detection

`--duplicates` groups files by exact size, then compares the first and last
64 KB of each candidate. A group of 40 GB virtual disks therefore costs a few
megabytes of reading rather than 40 GB.

Matching size, head and tail is strong evidence of a duplicate but it is not
proof, which is why the report says *likely* and why nothing acts on the
finding. The search stops at a read budget and says so when it does.

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

**Unreadable paths.** Some folders need elevated rights; on macOS, most of your
own Library needs Full Disk Access. They are counted, listed in
`unreadable.csv`, and reported as a count so the totals can be read honestly
rather than looking complete when they are not.

**Coverage.** The summary states what fraction of the volume's used space the
scan accounted for. The remainder is the skipped OS folders, unreadable paths,
and the differences above.

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
  insights.rs         rankings, duplicates, treemap pruning
  platform/           Windows, macOS and Linux specifics
  reports/
    terminal.rs       the terminal report
    html.rs           the self-contained HTML report
    template.html     its markup, CSS and treemap script
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
